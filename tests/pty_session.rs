//! N05a regression suite for the real-PTY session (`nora::session::pty`).
//!
//! Each test launches the tagged-I/O helper bin (`src/bin/nora_child.rs`,
//! discovered via `CARGO_BIN_EXE_nora_child`) through a real PTY and asserts
//! the card's acceptance lines on observable markers in the engine grid and
//! on `Terminated` events:
//! - accepted app bytes reach the real child in order (READY marker + echoed
//!   line visible in engine cells);
//! - exact argv with spaces and metacharacters survives (no shell);
//! - a pre-validated cwd is honored;
//! - a failed launch and a bad cwd are clean `PtyError`s (no panic, no hang);
//! - loop end surfaces as `Terminated` carrying the session's own `SessionId`
//!   and the expected exit code, and `close()` is idempotent.
//!
//! Every wait is deadline-bounded (10 s) and polls every 10 ms on observable
//! markers only — no sleep guesses.

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use nora::session::io::WriterConfig;
use nora::session::pty::{LaunchSpec, PtyError, PtySession, SessionEvent};

/// One poll tick inside the deadline-bounded pumps.
const POLL: Duration = Duration::from_millis(10);
/// Hard deadline for every wait in this suite.
const DEADLINE: Duration = Duration::from_secs(10);

/// The helper bin path, auto-discovered by cargo for integration tests.
fn helper() -> String {
    std::env::var("CARGO_BIN_EXE_nora_child")
        .expect("CARGO_BIN_EXE_nora_child must be set by cargo for integration tests")
}

fn cap() -> WriterConfig {
    WriterConfig {
        max_bytes: 256,
        reserved_bytes: 64,
    }
}

/// The engine's current grid as one row-major string, so any visible line is
/// a contiguous substring (wide-char spacers keep their stored char).
fn grid_text(session: &mut PtySession) -> String {
    let snap = session.engine().snapshot();
    snap.cells.iter().map(|c| c.ch).collect()
}

/// Deadline-bounded pump: each tick drains `poll_events()`, re-reads the
/// grid, and hands both to `satisfied`. Returns `Ok` on the first satisfied
/// tick; `Err` after the 10 s deadline with the last grid text.
fn pump(
    session: &mut PtySession,
    mut satisfied: impl FnMut(&str, &[SessionEvent]) -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + DEADLINE;
    let mut events: Vec<SessionEvent> = Vec::new();
    let mut last;
    loop {
        let mut fresh = session.poll_events();
        events.append(&mut fresh);
        last = grid_text(session);
        if satisfied(&last, &events) {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!(
                "deadline exceeded; last grid: {last:?}; saw {} event(s)",
                events.len()
            ));
        }
        thread::sleep(POLL);
    }
}

/// True if `events` contains a `Terminated` whose exit status has code `code`.
fn exited_with(events: &[SessionEvent], code: i32) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            SessionEvent::Terminated { status: Some(status), .. }
                if status.code() == Some(code)
        )
    })
}

#[test]
fn real_child_ready_echo_roundtrip() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    pump(&mut session, |text, _events| text.contains("READY"))
        .expect("child printed READY within the deadline");

    assert_eq!(session.feed(b"hello\n"), Ok(6));
    assert_eq!(session.flush(256), Ok(6));

    pump(&mut session, |text, _events| text.contains("E:hello"))
        .expect("child echoed hello within the deadline");

    session.close();
}

#[test]
fn argv_with_spaces_and_metacharacters_arrive_exact() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec![
                "argv".to_string(),
                "a b c".to_string(),
                "x;y".to_string(),
                "z*".to_string(),
            ],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    let markers = ["A:1 a b c", "A:2 x;y", "A:3 z*"];
    pump(&mut session, |text, events| {
        markers.iter().all(|m| text.contains(m)) && exited_with(events, 0)
    })
    .expect("exact argv lines visible and child exited 0 within the deadline");

    session.close();
}

#[test]
fn cwd_is_honored() {
    let temp = std::env::temp_dir();
    let marker = format!("PWD:{}", temp.display());
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["pwd".to_string()],
            cwd: Some(temp),
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns in the temp dir over a real PTY");

    pump(&mut session, |text, events| {
        text.contains(marker.as_str()) && exited_with(events, 0)
    })
    .expect("PWD line in the temp dir and child exited 0 within the deadline");

    session.close();
}

#[test]
fn failed_launch_and_bad_cwd_are_clean_errors() {
    let missing = LaunchSpec {
        program: "/nonexistent/nora-test-program-xyz".to_string(),
        args: Vec::new(),
        cwd: Some(std::env::temp_dir()),
        env: Vec::new(),
    };
    assert!(
        matches!(
            PtySession::spawn(missing, 24, 80, cap()),
            Err(PtyError::Spawn(_))
        ),
        "a missing program must fail with PtyError::Spawn"
    );

    let bad_cwd = PathBuf::from("/nonexistent-nora-cwd-dir");
    let spec = LaunchSpec {
        program: helper(),
        args: Vec::new(),
        cwd: Some(bad_cwd.clone()),
        env: Vec::new(),
    };
    match PtySession::spawn(spec, 24, 80, cap()) {
        Err(PtyError::BadCwd(path)) => assert_eq!(path, bad_cwd),
        Err(PtyError::Spawn(io)) => panic!("expected BadCwd, got Spawn: {io}"),
        Ok(_) => panic!("expected BadCwd, got Ok"),
    }
}

#[test]
fn termination_event_carries_generation_identity() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["status".to_string(), "3".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    let id = session.id();
    pump(&mut session, |_text, events| {
        events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::Terminated {
                    session: sid,
                    status: Some(status),
                } if *sid == id && status.code() == Some(3)
            )
        })
    })
    .expect("Terminated with this session's id and exit code 3 within the deadline");

    let _first = session.close();
    assert_eq!(session.close(), 0, "second close must release nothing");
}
