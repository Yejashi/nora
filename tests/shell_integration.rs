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
        self.session.close();
        Transcript { markers, grid }
    }
}

/// What a finished shell session showed.
struct Transcript {
    /// The OSC 133 markers with the grid line each arrived on (the tests
    /// stay within one screen, so lines are comparable).
    markers: Vec<(MarkerKind, i32)>,
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
        vec![D, A, B, C, D, A, B, C],
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
        vec![D, A, B, C, D, A, B, C],
        "grid:\n{}",
        out.grid
    );
    assert!(
        out.commands_start_below_their_prompt(),
        "a later PROMPT_COMMAND element was taken for the command: {:?}",
        out.markers
    );
}
