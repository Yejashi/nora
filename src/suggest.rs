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

    /// Load a history file. A missing file is an empty history; lines
    /// that do not parse are skipped; the caps apply.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(error) => return Err(error),
        };
        let mut history = Self::new();
        let mut lines = text.lines();
        if lines.next() != Some(FILE_HEADER) {
            return Ok(history);
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

    /// Write the history file atomically (a temporary file renamed over
    /// it), readable by the user only, and mark the history clean.
    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
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
        let tmp = path.with_extension("tmp");
        {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut file = options.open(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        self.dirty = false;
        Ok(())
    }
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
