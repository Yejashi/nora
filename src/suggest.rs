//! Inline suggestion model (ADR-013 step 3): the command lines the shell
//! reported executing, ranked by frecency, with a boost for commands run
//! in the current directory and a demotion for commands that failed.
//!
//! Everything is local: the history lives in memory and in one capped file
//! under the user's state directory ([`History::default_path`]). Command
//! lines starting with a space are never recorded (the shells'
//! `HIST_IGNORE_SPACE` / `ignorespace` convention), nor are multi-line or
//! oversized ones, or ones with control characters.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Most distinct command lines kept.
pub const MAX_ENTRIES: usize = 5_000;
/// Most bytes of command lines kept in all.
pub const MAX_HISTORY_BYTES: usize = 2 * 1024 * 1024;
/// Longest command line recorded, in bytes.
pub const MAX_CMDLINE_BYTES: usize = 4096;
/// Directories remembered per command line (the most used).
const MAX_DIRS: usize = 8;
/// First line of the history file.
const FILE_HEADER: &str = "nora-history 1";

/// One distinct command line and how it has been used.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The command line.
    pub cmdline: String,
    /// Times executed.
    pub count: u32,
    /// Times it exited with a non-zero status.
    pub failures: u32,
    /// When it was last executed (seconds since the Unix epoch).
    pub last_used: u64,
    /// Directories it was executed in, with counts.
    pub dirs: Vec<(String, u32)>,
}

impl Entry {
    /// The entry's rank for a prompt in `cwd` at time `now`: frequency
    /// weighted by recency, times 3 when it ran in `cwd`, scaled down to
    /// a fifth for a command that always failed.
    pub fn score(&self, cwd: Option<&str>, now: u64) -> f64 {
        let age = now.saturating_sub(self.last_used);
        let recency = match age {
            0..3_600 => 4.0,
            3_600..86_400 => 2.0,
            86_400..604_800 => 1.0,
            _ => 0.5,
        };
        let mut score = f64::from(self.count) * recency;
        if let Some(cwd) = cwd
            && self.dirs.iter().any(|(dir, _)| dir == cwd)
        {
            score *= 3.0;
        }
        let count = f64::from(self.count.max(1));
        let success = (count - f64::from(self.failures.min(self.count))) / count;
        score * (0.2 + 0.8 * success)
    }
}

/// The command history the suggestions come from.
#[derive(Debug, Clone, Default)]
pub struct History {
    entries: Vec<Entry>,
    index: HashMap<String, usize>,
    bytes: usize,
    dirty: bool,
}

impl History {
    /// An empty history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Distinct command lines stored.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is stored.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The stored entry for `cmdline`.
    pub fn get(&self, cmdline: &str) -> Option<&Entry> {
        self.index.get(cmdline).map(|&i| &self.entries[i])
    }

    /// Whether the history changed since it was loaded or last saved.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// An owned copy to save elsewhere (on another thread); this history
    /// is marked clean, as [`History::save`] would.
    pub fn take_snapshot(&mut self) -> Self {
        self.dirty = false;
        self.clone()
    }

    /// Mark the history changed, so it is saved again (after a failed
    /// save of a snapshot).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Record one execution of `cmdline` in `cwd` that exited with `exit`
    /// at time `now`. Returns whether it was recorded (see the module
    /// docs for what is skipped).
    pub fn record(
        &mut self,
        cmdline: &str,
        cwd: Option<&str>,
        exit: Option<i32>,
        now: u64,
    ) -> bool {
        let cmdline = cmdline.trim_end();
        if !recordable(cmdline) {
            return false;
        }
        let failed = matches!(exit, Some(status) if status != 0);
        let i = match self.index.get(cmdline) {
            Some(&i) => i,
            None => {
                self.entries.push(Entry {
                    cmdline: cmdline.to_string(),
                    count: 0,
                    failures: 0,
                    last_used: now,
                    dirs: Vec::new(),
                });
                self.bytes += cmdline.len();
                self.index
                    .insert(cmdline.to_string(), self.entries.len() - 1);
                self.entries.len() - 1
            }
        };
        let entry = &mut self.entries[i];
        entry.count = entry.count.saturating_add(1);
        entry.failures = entry.failures.saturating_add(u32::from(failed));
        entry.last_used = entry.last_used.max(now);
        if let Some(cwd) = cwd {
            match entry.dirs.iter_mut().find(|(dir, _)| dir == cwd) {
                Some((_, n)) => *n = n.saturating_add(1),
                None => {
                    if entry.dirs.len() >= MAX_DIRS
                        && let Some(least) = (0..entry.dirs.len()).min_by_key(|&d| entry.dirs[d].1)
                    {
                        entry.dirs.swap_remove(least);
                    }
                    entry.dirs.push((cwd.to_string(), 1));
                }
            }
        }
        self.dirty = true;
        self.evict(cmdline, now);
        true
    }

    /// The best suggestion for the editor buffer `buffer` in `cwd`: the
    /// highest-ranked command line that extends it. Ties go to the most
    /// recent, then the shortest.
    pub fn suggest(&self, buffer: &str, cwd: Option<&str>, now: u64) -> Option<&str> {
        if buffer.trim().is_empty() {
            return None;
        }
        self.entries
            .iter()
            .filter(|e| e.cmdline.len() > buffer.len() && e.cmdline.starts_with(buffer))
            .max_by(|a, b| {
                a.score(cwd, now)
                    .total_cmp(&b.score(cwd, now))
                    .then(a.last_used.cmp(&b.last_used))
                    .then(b.cmdline.len().cmp(&a.cmdline.len()))
            })
            .map(|e| e.cmdline.as_str())
    }

    /// Drop the lowest-ranked entries (never `keep`) until within the caps.
    fn evict(&mut self, keep: &str, now: u64) {
        while self.entries.len() > MAX_ENTRIES || self.bytes > MAX_HISTORY_BYTES {
            let victim = (0..self.entries.len())
                .filter(|&i| self.entries[i].cmdline != keep)
                .min_by(|&a, &b| {
                    self.entries[a]
                        .score(None, now)
                        .total_cmp(&self.entries[b].score(None, now))
                });
            let Some(victim) = victim else { break };
            let removed = self.entries.swap_remove(victim);
            self.bytes -= removed.cmdline.len();
            self.index.remove(&removed.cmdline);
            if let Some(moved) = self.entries.get(victim) {
                self.index.insert(moved.cmdline.clone(), victim);
            }
        }
    }

    /// `$XDG_STATE_HOME/nora/history`, else `~/.local/state/nora/history`.
    pub fn default_path() -> Option<PathBuf> {
        let state = std::env::var_os("XDG_STATE_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| Path::new(&home).join(".local").join("state"))
            })?;
        Some(state.join("nora").join("history"))
    }

    /// Load a history file. A missing or empty file is an empty history;
    /// lines that do not parse are skipped; the caps apply. A file that is
    /// not this format (not UTF-8, or another header) is an
    /// [`std::io::ErrorKind::InvalidData`] error, and [`History::save`]
    /// refuses to replace it.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(error) => return Err(error),
        };
        let mut history = Self::new();
        let mut lines = text.lines();
        match lines.next() {
            None => return Ok(history),
            Some(FILE_HEADER) => {}
            Some(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("not a \"{FILE_HEADER}\" file"),
                ));
            }
        }
        let mut newest = 0;
        for line in lines {
            if let Some(entry) = parse_line(line)
                && recordable(&entry.cmdline)
                && !history.index.contains_key(&entry.cmdline)
            {
                newest = newest.max(entry.last_used);
                history.bytes += entry.cmdline.len();
                history
                    .index
                    .insert(entry.cmdline.clone(), history.entries.len());
                history.entries.push(entry);
            }
        }
        history.evict("", newest);
        history.dirty = false;
        Ok(history)
    }

    /// Fold `other` (the file another nora process saved) into this
    /// history, then apply the caps. Per command line: the larger count
    /// with its failures (each process's count already includes what it
    /// loaded, so adding would count twice), the newest time, and per
    /// directory the larger count, keeping the most used.
    fn merge(&mut self, other: Self) {
        for theirs in other.entries {
            let Some(&i) = self.index.get(&theirs.cmdline) else {
                self.bytes += theirs.cmdline.len();
                self.index
                    .insert(theirs.cmdline.clone(), self.entries.len());
                self.entries.push(theirs);
                continue;
            };
            let mine = &mut self.entries[i];
            if (theirs.count, theirs.failures) > (mine.count, mine.failures) {
                mine.count = theirs.count;
                mine.failures = theirs.failures;
            }
            mine.last_used = mine.last_used.max(theirs.last_used);
            for (dir, n) in theirs.dirs {
                match mine.dirs.iter_mut().find(|(d, _)| *d == dir) {
                    Some((_, m)) => *m = (*m).max(n),
                    None => mine.dirs.push((dir, n)),
                }
            }
            if mine.dirs.len() > MAX_DIRS {
                mine.dirs.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
                mine.dirs.truncate(MAX_DIRS);
            }
        }
        let newest = self.entries.iter().map(|e| e.last_used).max().unwrap_or(0);
        self.evict("", newest);
    }

    /// Write the history file atomically (a temporary file of this
    /// process's own, renamed over it), readable by the user only, and
    /// mark the history clean. What the file holds now (another nora
    /// process may have saved since) is merged in first, so no process
    /// drops another's commands; a file [`History::load`] rejects is left
    /// alone and is an error.
    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        self.merge(Self::load(path)?);
        let mut text = String::with_capacity(self.bytes + self.entries.len() * 32);
        text.push_str(FILE_HEADER);
        text.push('\n');
        for e in &self.entries {
            text.push_str(&format!(
                "{}\t{}\t{}\t{}",
                e.last_used,
                e.count,
                e.failures,
                escape(&e.cmdline)
            ));
            for (dir, n) in &e.dirs {
                text.push_str(&format!("\t{n}:{}", escape(dir)));
            }
            text.push('\n');
        }
        let tmp = temp_path(path);
        let written =
            write_private(&tmp, text.as_bytes()).and_then(|()| std::fs::rename(&tmp, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written?;
        self.dirty = false;
        Ok(())
    }
}

/// A temporary file next to `path` that no other save uses: named after
/// this process and a per-process counter.
fn temp_path(path: &Path) -> PathBuf {
    static SAVES: AtomicU64 = AtomicU64::new(0);
    let n = SAVES.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{name}.{}.{n}.tmp", std::process::id()))
}

/// Create `path` (readable by the user only), write `bytes` and sync them.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Whether a (trailing-trimmed) command line may be stored.
fn recordable(cmdline: &str) -> bool {
    !cmdline.is_empty()
        && cmdline.len() <= MAX_CMDLINE_BYTES
        && !cmdline.starts_with(char::is_whitespace)
        && !cmdline.chars().any(char::is_control)
}

/// One history file line: `last_used \t count \t failures \t cmdline`
/// then `\t count:dir` per directory.
fn parse_line(line: &str) -> Option<Entry> {
    let mut fields = line.split('\t');
    let last_used = fields.next()?.parse().ok()?;
    let count = fields.next()?.parse().ok()?;
    let failures = fields.next()?.parse().ok()?;
    let cmdline = unescape(fields.next()?)?;
    let mut dirs = Vec::new();
    for field in fields.take(MAX_DIRS) {
        let (n, dir) = field.split_once(':')?;
        dirs.push((unescape(dir)?, n.parse().ok()?));
    }
    Some(Entry {
        cmdline,
        count,
        failures,
        last_used,
        dirs,
    })
}

/// Escape the bytes that would break a line: `%`, tab, CR, LF.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => out.push_str("%25"),
            '\t' => out.push_str("%09"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let code = rest.get(at + 1..at + 3)?;
        out.push(match code {
            "25" => '%',
            "09" => '\t',
            "0A" => '\n',
            "0D" => '\r',
            _ => return None,
        });
        rest = &rest[at + 3..];
    }
    out.push_str(rest);
    Some(out)
}

/// Most directory entries read for one completion (a huge directory must
/// not stall the pump).
pub const MAX_DIR_ENTRIES: usize = 4096;

/// A completion for the buffer's last word when history has none (ADR-013
/// sources b and c): a path relative to `cwd` (`~/` from `home`), or for
/// the first word without a `/`, an executable from `executables`
/// (sorted). Only an unambiguous extension is offered: the longest common
/// prefix of the matches, plus `/` for a single directory. Words with
/// quotes or backslashes get none (their shell meaning differs from the
/// text), nor do hidden names unless the word starts them with `.`, nor
/// paths in a directory of more than [`MAX_DIR_ENTRIES`] entries (the
/// matches past the cap are unknown).
pub fn complete_word(
    buffer: &str,
    cwd: Option<&str>,
    home: Option<&str>,
    executables: &[String],
) -> Option<String> {
    if buffer.ends_with(char::is_whitespace) {
        return None;
    }
    let start = buffer.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    let word = &buffer[start..];
    if word.is_empty() || word.contains(['\'', '"', '\\', '$', '`', '*', '?']) {
        return None;
    }
    let first_word = buffer[..start].trim().is_empty();
    if first_word && !word.contains('/') {
        let matches = executables.iter().filter(|e| e.starts_with(word));
        return common_extension(word, matches.map(String::as_str), false);
    }
    let (dir_part, name) = match word.rfind('/') {
        Some(i) => (&word[..=i], &word[i + 1..]),
        None => ("", word),
    };
    let dir = if let Some(rest) = dir_part.strip_prefix("~/") {
        std::path::Path::new(home?).join(rest)
    } else if dir_part.starts_with('/') {
        PathBuf::from(dir_part)
    } else {
        std::path::Path::new(cwd?).join(dir_part)
    };
    let mut names = Vec::new();
    let mut single_is_dir = false;
    for (read, entry) in std::fs::read_dir(&dir).ok()?.enumerate() {
        // A listing cut off at the cap may miss matches, so no extension
        // of what it saw is known to be unambiguous.
        if read >= MAX_DIR_ENTRIES {
            return None;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(entry_name) = entry.file_name().into_string() else {
            continue;
        };
        if !entry_name.starts_with(name) || (entry_name.starts_with('.') && !name.starts_with('.'))
        {
            continue;
        }
        // The entry's own type (no `stat`), following a symlink only.
        single_is_dir = match entry.file_type() {
            Ok(t) if t.is_symlink() => entry.path().is_dir(),
            Ok(t) => t.is_dir(),
            Err(_) => false,
        };
        names.push(entry_name);
    }
    let dir_slash = names.len() == 1 && single_is_dir;
    common_extension(name, names.iter().map(String::as_str), dir_slash)
}

/// What the longest common prefix of `matches` adds to `typed` (plus `/`
/// when asked), or `None` when it adds nothing.
fn common_extension<'a>(
    typed: &str,
    matches: impl Iterator<Item = &'a str>,
    slash: bool,
) -> Option<String> {
    let mut common: Option<&str> = None;
    for m in matches {
        common = Some(match common {
            None => m,
            Some(c) => {
                let len = c
                    .char_indices()
                    .zip(m.chars())
                    .find(|((_, a), b)| a != b)
                    .map_or(c.len().min(m.len()), |((i, _), _)| i);
                &c[..len]
            }
        });
    }
    let mut extension = common?.get(typed.len()..)?.to_string();
    if slash {
        extension.push('/');
    }
    (!extension.is_empty() && !extension.chars().any(char::is_control)).then_some(extension)
}

/// Executable file names in the directories of `path_var` (a `PATH`
/// value), sorted and deduplicated, at most [`MAX_DIR_ENTRIES`] read per
/// directory.
pub fn path_executables(path_var: &std::ffi::OsStr) -> Vec<String> {
    let mut names = Vec::new();
    for dir in std::env::split_paths(path_var) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.take(MAX_DIR_ENTRIES).flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if is_executable(&entry.path()) {
                names.push(name);
            }
        }
    }
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
