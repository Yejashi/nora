//! Regression tests for the opt-in shell integration scripts
//! (`shell/nora-bash.sh`, `shell/nora-zsh.zsh`) against a real bash or
//! zsh over a real PTY. Each test skips (passes with a note) when its
//! shell is not installed. Every wait is deadline-bounded.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use nora::prompt_marker::MarkerKind;
use nora::session::io::WriterConfig;
use nora::session::pty::{LaunchSpec, PtySession, SessionEvent};

use MarkerKind::{A, B, C, D};

const POLL: Duration = Duration::from_millis(10);
const DEADLINE: Duration = Duration::from_secs(10);

/// The first existing path among `candidates`.
fn find_shell(candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find(|path| Path::new(path).exists())
        .map(|path| path.to_string())
}

fn bash() -> Option<String> {
    find_shell(&[
        "/run/current-system/sw/bin/bash",
        "/usr/bin/bash",
        "/bin/bash",
    ])
}

fn zsh() -> Option<String> {
    find_shell(&["/usr/bin/zsh", "/bin/zsh", "/run/current-system/sw/bin/zsh"])
}

fn script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("shell")
        .join(name)
}

/// A per-test scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir()
            .canonicalize()
            .expect("the temp dir resolves")
            .join(format!("nora-shell-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch dir");
        Scratch(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A shell running over a real PTY.
struct Shell {
    session: PtySession,
    events: Vec<SessionEvent>,
}

impl Shell {
    fn spawn(program: String, args: Vec<String>, cwd: &Path, env: Vec<(String, String)>) -> Self {
        let session = PtySession::spawn(
            LaunchSpec {
                program,
                args,
                cwd: Some(cwd.to_path_buf()),
                env,
            },
            24,
            100,
            WriterConfig {
                max_bytes: 256,
                reserved_bytes: 64,
            },
        )
        .expect("the shell spawns over a real PTY");
        Shell {
            session,
            events: Vec::new(),
        }
    }

    /// Interactive bash with `rc` (which should source the script) as its
    /// rcfile, in `dir`.
    fn bash(program: String, dir: &Scratch, rc: &str) -> Self {
        let rcfile = dir.path("bashrc");
        std::fs::write(&rcfile, rc).expect("write the rcfile");
        Shell::spawn(
            program,
            vec![
                "--rcfile".to_string(),
                rcfile.to_string_lossy().into_owned(),
                "-i".to_string(),
            ],
            &dir.0,
            vec![("HISTFILE".to_string(), "/dev/null".to_string())],
        )
    }

    /// Interactive zsh with `rc` as its `.zshrc` (`-d`: no global rc
    /// files), in `dir`.
    fn zsh(program: String, dir: &Scratch, rc: &str) -> Self {
        std::fs::write(dir.path(".zshrc"), rc).expect("write the zshrc");
        Shell::spawn(
            program,
            vec!["-d".to_string(), "-i".to_string()],
            &dir.0,
            vec![
                ("ZDOTDIR".to_string(), dir.0.to_string_lossy().into_owned()),
                ("HISTFILE".to_string(), "/dev/null".to_string()),
            ],
        )
    }

    fn grid(&mut self) -> String {
        let snap = self.session.engine().snapshot();
        let cols = snap.cols.max(1);
        let chars: Vec<char> = snap.cells.iter().map(|c| c.ch).collect();
        chars
            .chunks(cols)
            .map(|row| row.iter().collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Pump until `done` holds for the grid text, or panic at the deadline.
    fn wait(&mut self, what: &str, mut done: impl FnMut(&str, &[SessionEvent]) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let mut fresh = self.session.poll_events();
            self.events.append(&mut fresh);
            let _ = self.session.flush(256);
            let grid = self.grid();
            if done(&grid, &self.events) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{what} within the deadline; grid:\n{grid}"
            );
            thread::sleep(POLL);
        }
    }

    /// Type `bytes`, waiting for the writer to accept all of them.
    fn send(&mut self, bytes: &[u8]) {
        let deadline = Instant::now() + DEADLINE;
        let mut rest = bytes;
        while !rest.is_empty() {
            let accepted = self.session.feed(rest).expect("feed the shell");
            rest = &rest[accepted..];
            self.session.flush(256).expect("flush to the shell");
            assert!(Instant::now() < deadline, "the shell accepts input");
            if accepted == 0 {
                thread::sleep(POLL);
            }
        }
    }

    /// Run one command line and wait until `marker` is on screen.
    fn run(&mut self, line: &str, marker: &str) {
        self.send(format!("{line}\n").as_bytes());
        self.wait(marker, |grid, _| grid.contains(marker));
    }

    /// `exit`, wait for the shell to end, and return the markers seen.
    fn exit(mut self) -> Transcript {
        self.send(b"exit\n");
        self.wait("the shell exits", |_, events| {
            events
                .iter()
                .any(|event| matches!(event, SessionEvent::Terminated { .. }))
        });
        let grid = self.grid();
        let markers = self
            .session
            .engine()
            .drain_markers()
            .iter()
            .map(|marker| (marker.event.kind, marker.line))
            .collect();
        let executed = self
            .session
            .engine()
            .take_executed()
            .into_iter()
            .map(|command| command.cmdline)
            .collect();
        self.session.close();
        Transcript {
            markers,
            executed,
            grid,
        }
    }
}

/// What a finished shell session showed.
struct Transcript {
    /// The OSC 133 markers with the grid line each arrived on (the tests
    /// stay within one screen, so lines are comparable).
    markers: Vec<(MarkerKind, i32)>,
    /// The command lines the shell reported (`133;C;cmdline_url=`).
    executed: Vec<String>,
    /// The final screen.
    grid: String,
}

impl Transcript {
    fn kinds(&self) -> Vec<MarkerKind> {
        self.markers.iter().map(|(kind, _)| *kind).collect()
    }

    /// Every `C` arrived below the `A` before it: the command started after
    /// the prompt line was entered, not while the prompt was drawn.
    fn commands_start_below_their_prompt(&self) -> bool {
        let mut prompt_line = None;
        self.markers.iter().all(|(kind, line)| match kind {
            A => {
                prompt_line = Some(*line);
                true
            }
            C => prompt_line.is_none_or(|prompt| *line > prompt),
            _ => true,
        })
    }
}

/// A PROMPT_COMMAND that ends in `;` (or `&`) still runs, and nora's hook
/// after it: joining with `"; "` made every prompt a syntax error.
#[test]
fn bash_prompt_command_ending_in_semicolon() {
    let Some(bash) = bash() else {
        eprintln!("bash not found; skipping");
        return;
    };
    let dir = Scratch::new("bash-pc-semicolon");
    let rc = format!(
        "PS1='$ '\nPROMPT_COMMAND='history -a; '\nsource {}\n",
        script("nora-bash.sh").display()
    );
    let mut shell = Shell::bash(bash, &dir, &rc);
    shell.wait("first prompt", |grid, _| grid.contains('$'));
    shell.run("echo first-$((1 + 1))", "first-2");
    let out = shell.exit();
    assert!(!out.grid.contains("syntax error"), "grid:\n{}", out.grid);
    assert_eq!(
        out.kinds(),
        vec![A, B, C, D, A, B, C],
        "grid:\n{}",
        out.grid
    );
}

/// An array PROMPT_COMMAND (bash 5.1+) gets nora's hook as a new last
/// element, so the user's later elements do not run after the prompt
/// start (where the DEBUG trap took them for the user's command: a
/// spurious `B C` right after every `A`).
#[test]
fn bash_array_prompt_command_keeps_hook_last() {
    let Some(bash) = bash() else {
        eprintln!("bash not found; skipping");
        return;
    };
    let dir = Scratch::new("bash-pc-array");
    let rc = format!(
        "PS1='$ '\n\
         if (( BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1] < 501 )); then echo OLD-BASH; fi\n\
         PROMPT_COMMAND=('history -a' 'pc_runs=$((pc_runs + 1))')\n\
         source {}\n",
        script("nora-bash.sh").display()
    );
    let mut shell = Shell::bash(bash, &dir, &rc);
    shell.wait("first prompt", |grid, _| grid.contains('$'));
    if shell.grid().contains("OLD-BASH") {
        eprintln!("bash older than 5.1 has no array PROMPT_COMMAND; skipping");
        return;
    }
    shell.run("echo \"runs=$pc_runs\"", "runs=1");
    let out = shell.exit();
    assert_eq!(
        out.kinds(),
        vec![A, B, C, D, A, B, C],
        "grid:\n{}",
        out.grid
    );
    assert!(
        out.commands_start_below_their_prompt(),
        "a later PROMPT_COMMAND element was taken for the command: {:?}",
        out.markers
    );
}

/// The DEBUG trap leaves `$_` alone, with and without a DEBUG trap set
/// before the script (chained): `mkdir -p d && cd $_` used to `cd` into
/// `__nora_command_start`.
#[test]
fn bash_debug_trap_keeps_last_argument() {
    let Some(bash) = bash() else {
        eprintln!("bash not found; skipping");
        return;
    };
    let prior = "trap 'prev=$((prev + 1))' DEBUG\n";
    for (name, prior_trap) in [("plain", ""), ("chained", prior)] {
        let dir = Scratch::new(&format!("bash-underscore-{name}"));
        let rc = format!(
            "PS1='$ '\n{prior_trap}source {}\n",
            script("nora-bash.sh").display()
        );
        let mut shell = Shell::bash(bash.clone(), &dir, &rc);
        shell.wait("first prompt", |grid, _| grid.contains('$'));
        shell.run("mkdir -p d1 && cd $_ && echo \"IN=${PWD##*/}\"", "IN=d1");
        shell.run("cd ..; echo one two; echo \"U=$_\"", "U=two");
        // `$_` also survives the prompt between two command lines.
        shell.run("mkdir -p d2; echo made-$((1 + 1))", "made-2");
        shell.run("mkdir -p d3", "$ mkdir -p d3");
        shell.run("cd $_ && echo \"IN3=${PWD##*/}\"", "IN3=d3");
        if !prior_trap.is_empty() {
            shell.run("[ \"$prev\" -gt 0 ] && echo prev-$((1 + 1))", "prev-2");
        }
        let out = shell.exit();
        assert!(!out.grid.contains("No such file"), "{name}:\n{}", out.grid);
    }
}

/// The bash markers go to the terminal even when the command line's first
/// command runs under a redirection (the DEBUG trap runs inside it), and
/// the terminal fd they use is not inherited by the user's commands.
#[test]
fn bash_markers_bypass_redirections_and_stay_private() {
    let Some(bash) = bash() else {
        eprintln!("bash not found; skipping");
        return;
    };
    let dir = Scratch::new("bash-redirect");
    let rc = format!("PS1='$ '\nsource {}\n", script("nora-bash.sh").display());
    let mut shell = Shell::bash(bash, &dir, &rc);
    shell.wait("first prompt", |grid, _| grid.contains('$'));
    shell.run(
        "{ echo a; echo b; } > group.txt; if true; then echo c; fi > if.txt; echo ran-$((1 + 1))",
        "ran-2",
    );
    // A child lists where its fds point: stdin and stderr are the
    // terminal, stdout is the file; a third terminal fd would be nora's.
    shell.run(
        "sh -c 'for f in /proc/$$/fd/*; do readlink \"$f\"; done' > fds.txt; echo ran-$((2 + 1))",
        "ran-3",
    );
    let out = shell.exit();
    assert_eq!(
        std::fs::read_to_string(dir.path("group.txt")).expect("group.txt"),
        "a\nb\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path("if.txt")).expect("if.txt"),
        "c\n"
    );
    assert_eq!(
        out.kinds(),
        vec![A, B, C, D, A, B, C, D, A, B, C],
        "grid:\n{}",
        out.grid
    );
    let fds = std::fs::read_to_string(dir.path("fds.txt")).expect("fds.txt");
    let terminal_fds = fds
        .lines()
        .filter(|line| line.starts_with("/dev/pts/"))
        .count();
    assert_eq!(terminal_fds, 2, "the child's fds:\n{fds}");
}

/// bash sends `D` only after a command line ran, carrying its exit status
/// (`133;D;<status>`), even with a PROMPT_COMMAND entry before nora's that
/// resets `$?`; an empty or blank Enter gets a new prompt (`A`) only.
#[test]
fn bash_command_end_carries_status_and_skips_empty_lines() {
    let Some(bash) = bash() else {
        eprintln!("bash not found; skipping");
        return;
    };
    let dir = Scratch::new("bash-status");
    let log = dir.path("markers.log");
    // Log every marker's parameters (then send it as before).
    let rc = format!(
        "PS1='$ '\nPROMPT_COMMAND='history -a'\nsource {}\n\
         eval \"__nora_test_osc133() $(declare -f __nora_osc133 | tail -n +2)\"\n\
         __nora_osc133() {{ printf '%s\\n' \"$1\" >> {}; __nora_test_osc133 \"$1\"; }}\n",
        script("nora-bash.sh").display(),
        log.display()
    );
    let mut shell = Shell::bash(bash, &dir, &rc);
    shell.wait("first prompt", |grid, _| grid.contains('$'));
    shell.send(b"false\n\n   \n(exit 7)\ntrue\n");
    let out = shell.exit();
    let logged = std::fs::read_to_string(&log).expect("markers.log");
    let logged: Vec<&str> = logged.lines().collect();
    assert_eq!(
        logged,
        [
            "A", // first prompt
            "B", "C", "D;1", "A", // false
            "A", "A", // empty and blank lines
            "B", "C", "D;7", "A", // (exit 7)
            "B", "C", "D;0", "A", // true
            "B", "C", // exit
        ],
        "grid:\n{}",
        out.grid
    );
    assert_eq!(
        out.kinds(),
        vec![A, B, C, D, A, A, A, B, C, D, A, B, C, D, A, B, C]
    );
}

/// zsh: HISTORY_IGNORE is matched with the user's options, so a pattern
/// that needs the user's EXTENDED_GLOB (`(#i)`) keeps a command
/// unreported, as it keeps it out of the history file.
#[test]
fn zsh_history_ignore_uses_the_users_extended_glob() {
    let Some(zsh) = zsh() else {
        eprintln!("zsh not found; skipping");
        return;
    };
    let dir = Scratch::new("zsh-histignore-extglob");
    let rc = format!(
        "PS1='$ '\nsetopt extendedglob\nHISTORY_IGNORE='(#i)*secret*'\nsource {}\n",
        script("nora-zsh.zsh").display()
    );
    let mut shell = Shell::zsh(zsh, &dir, &rc);
    shell.wait("first prompt", |grid, _| grid.contains('$'));
    shell.run("echo MY-SECRET-$((1 + 1))", "MY-SECRET-2");
    shell.run("echo kept-$((1 + 1))", "kept-2");
    let out = shell.exit();
    assert!(
        out.executed
            .iter()
            .any(|line| line.starts_with("echo kept")),
        "a normal command is reported; got {:?}",
        out.executed
    );
    assert!(
        !out.executed.iter().any(|line| line.contains("SECRET")),
        "an ignored command is not reported; got {:?}",
        out.executed
    );
}

/// zsh with the user's KSH_ARRAYS still runs the line-init hook: every
/// prompt gets its `B` (and the buffer reports keep working).
#[test]
fn zsh_ksh_arrays_keeps_the_line_hooks() {
    let Some(zsh) = zsh() else {
        eprintln!("zsh not found; skipping");
        return;
    };
    let dir = Scratch::new("zsh-ksh-arrays");
    let rc = format!(
        "PS1='$ '\nsetopt ksh_arrays\nsource {}\n",
        script("nora-zsh.zsh").display()
    );
    let mut shell = Shell::zsh(zsh, &dir, &rc);
    shell.wait("first prompt", |grid, _| grid.contains('$'));
    shell.run("echo ran-$((1 + 1))", "ran-2");
    let out = shell.exit();
    assert_eq!(
        out.kinds(),
        vec![A, B, C, D, A, B, C],
        "grid:\n{}",
        out.grid
    );
    assert!(
        out.executed.iter().any(|line| line.starts_with("echo ran")),
        "got {:?}",
        out.executed
    );
}

/// zsh's terminal fd for the line-editor hooks is open only while editing:
/// a command (or a daemon it starts) inherits only its own terminal fds,
/// and the markers and buffer reports still arrive.
#[test]
fn zsh_commands_do_not_inherit_the_marker_fd() {
    let Some(zsh) = zsh() else {
        eprintln!("zsh not found; skipping");
        return;
    };
    let dir = Scratch::new("zsh-fd");
    let rc = format!("PS1='$ '\nsource {}\n", script("nora-zsh.zsh").display());
    let mut shell = Shell::zsh(zsh, &dir, &rc);
    shell.wait("first prompt", |grid, _| grid.contains('$'));
    // stdin and stderr are the terminal, stdout is the file; a third
    // terminal fd would be nora's.
    shell.run(
        "sh -c 'for f in /proc/$$/fd/*; do readlink \"$f\"; done' > fds.txt; echo ran-$((1 + 1))",
        "ran-2",
    );
    let out = shell.exit();
    let fds = std::fs::read_to_string(dir.path("fds.txt")).expect("fds.txt");
    let terminal_fds = fds
        .lines()
        .filter(|line| line.starts_with("/dev/pts/"))
        .count();
    assert_eq!(terminal_fds, 2, "the child's fds:\n{fds}");
    assert_eq!(
        out.kinds(),
        vec![A, B, C, D, A, B, C],
        "grid:\n{}",
        out.grid
    );
    assert!(
        out.executed.iter().any(|line| line.starts_with("sh -c")),
        "got {:?}",
        out.executed
    );
}
