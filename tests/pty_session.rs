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
//!   and the expected exit code, and `close()` is idempotent;
//! - N06a: size replies resolve against each session's own window geometry
//!   and never cross sessions (stale in-flight replies stay in their own
//!   session's stream).
//!
//! Every wait is deadline-bounded (10 s) and polls every 10 ms on observable
//! markers only — no sleep guesses.

// Real Unix PTYs, /bin/sh and the Unix-only nora_child helper throughout.
#![cfg(unix)]

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::term::TermMode;
use nora::engine::Damage;
use nora::input::{BindingTable, InputAction, encode};
use nora::prompt_marker::MarkerKind;
use nora::render::term::is_noop_damage;
use nora::session::io::{WriterConfig, WriterError};
use nora::session::pty::{
    DRAIN_TIMEOUT, LaunchSpec, Lifecycle, PtyError, PtySession, SessionEvent,
};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey};

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
            // Trailing blanks trimmed so the message (a CI annotation)
            // keeps the content.
            let grid = last.trim_end();
            return Err(format!(
                "deadline exceeded; last grid ({} chars): {grid:?}; events: {events:?}",
                grid.len()
            ));
        }
        thread::sleep(POLL);
    }
}

/// Flush `limit` bytes, tolerating the `Err(WriterError::Closed)` a
/// session reports once its loop is dead and an in-flight reply can no
/// longer be delivered (N06a, fixture P1).
fn flush_tolerant(session: &mut PtySession, limit: usize) {
    match session.flush(limit) {
        Ok(_) | Err(WriterError::Closed) => {}
        Err(error) => panic!("flush: {error:?}"),
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

/// Regression (vendored `pty_read`): a child that prints and exits while
/// the terminal lock is held must not lose its output. The reader finds
/// the lock busy, reads again, gets EIO from the closed slave, and used to
/// return that error with the already-read bytes unparsed.
#[test]
fn output_of_exiting_child_survives_a_busy_terminal_lock() {
    for round in 0..5 {
        let mut session = PtySession::spawn(
            LaunchSpec {
                program: helper(),
                args: vec!["argv".to_string(), format!("LOCKED-{round}")],
                cwd: None,
                env: Vec::new(),
            },
            24,
            80,
            cap(),
        )
        .expect("helper spawns over a real PTY");
        let term = session.engine().term_handle();
        {
            // Hold the lock while the child writes and exits.
            let _guard = term.lock();
            thread::sleep(Duration::from_millis(300));
        }
        pump(&mut session, |text, events| {
            text.contains(&format!("A:1 LOCKED-{round}")) && exited_with(events, 0)
        })
        .expect("output written under a held lock reaches the grid");
        session.close();
    }
}

/// A child that prints and exits at once never loses its output, over
/// many runs (macOS dropped it intermittently: `cwd_is_honored` saw an
/// empty grid in CI).
#[test]
fn fast_exit_output_survives_repeatedly() {
    for round in 0..20 {
        let marker = format!("FAST-{round}");
        let mut session = PtySession::spawn(
            LaunchSpec {
                program: helper(),
                args: vec!["argv".to_string(), marker.clone()],
                cwd: None,
                env: Vec::new(),
            },
            24,
            80,
            cap(),
        )
        .expect("helper spawns over a real PTY");
        pump(&mut session, |text, events| {
            text.contains(&format!("A:1 {marker}")) && exited_with(events, 0)
        })
        .unwrap_or_else(|error| panic!("round {round}: {error}"));
        session.close();
    }
}

/// The first `Terminated` a poll reports already carries the clean exit
/// status: loop end is sampled before the drain, so a `ChildExit` recorded
/// just before the loop returns is never missed (a `None` there made
/// `nora -- sh -c 'exit 0'` sometimes exit 1).
#[test]
fn first_terminated_carries_the_exit_status() {
    for round in 0..30 {
        let mut session = PtySession::spawn(
            LaunchSpec {
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "exit 0".to_string()],
                cwd: None,
                env: Vec::new(),
            },
            24,
            80,
            cap(),
        )
        .expect("sh spawns over a real PTY");
        let deadline = Instant::now() + DEADLINE;
        let first = loop {
            if let Some(event) = session
                .poll_events()
                .into_iter()
                .find(|event| matches!(event, SessionEvent::Terminated { .. }))
            {
                break event;
            }
            assert!(Instant::now() < deadline, "round {round}: no Terminated");
            thread::sleep(Duration::from_millis(1));
        };
        assert!(
            exited_with(std::slice::from_ref(&first), 0),
            "round {round}: first Terminated lacks the status: {first:?}"
        );
        session.close();
    }
}

#[test]
fn cwd_is_honored() {
    // Canonical: macOS's temp dir is behind a symlink (/var -> /private/var)
    // and the child reports the resolved path.
    let temp = std::fs::canonicalize(std::env::temp_dir()).expect("temp dir resolves");
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

/// N05b: a child that stops reading must never make nora's own state grow
/// or block. The helper prints `STOPPED` only after receiving >= 4096
/// bytes, so once it is visible the PTY buffer is full and the pinned
/// loop's private write queue absorbs the rest — nora-side bytes must
/// stay at zero the whole time (the loop-internal queue is unbounded
/// inside the crate and is deliberately not asserted).
#[test]
fn backpressure_with_stop_reading_child() {
    let cap = WriterConfig {
        max_bytes: 64 * 1024,
        reserved_bytes: 16 * 1024,
    };
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["stop".to_string(), "4096".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap,
    )
    .expect("helper spawns over a real PTY");
    pump(&mut session, |text, _events| text.contains("READY"))
        .expect("helper ready (stdin raw) within the deadline");

    // 32 KiB + newline: ~4x the typical kernel PTY buffer, newline last so
    // canonical mode delivers the full line to the stopped reader.
    let mut payload = vec![b'A'; 32 * 1024];
    payload.push(b'\n');
    assert_eq!(
        session.feed(&payload),
        Ok(32769),
        "the whole payload must fit the app budget"
    );
    session
        .flush(cap.max_bytes)
        .expect("flush must forward the payload to the loop channel promptly");
    assert_eq!(session.outstanding(), 0);

    // The helper prints STOPPED only after receiving >= 4096 bytes, so this
    // also proves the loop kept delivering while the PTY filled up.
    pump(&mut session, |text, _events| text.contains("STOPPED"))
        .expect("helper stopped reading after 4096 bytes within the deadline");

    // Bounded 1 s stall window: flush must stay prompt and nora-side bytes
    // must stay at zero while the child merely stalls.
    let window_end = Instant::now() + Duration::from_secs(1);
    let mut stalled_events: Vec<SessionEvent> = Vec::new();
    while Instant::now() < window_end {
        session
            .flush(cap.max_bytes)
            .expect("flush must stay prompt while the child is stalled");
        assert_eq!(
            session.outstanding(),
            0,
            "nora-side bytes must stay bounded while the child stalls"
        );
        stalled_events.append(&mut session.poll_events());
        thread::sleep(POLL);
    }
    assert!(
        !stalled_events
            .iter()
            .any(|event| matches!(event, SessionEvent::Terminated { .. })),
        "a merely-stalled child must not surface as Terminated"
    );

    // close() ends the session; the helper then sees EOF on stdin and exits
    // 0. The post-close status is a documented race: close() sends
    // Msg::Shutdown, which can end the loop before the child's SIGCHLD is
    // observed, so accept Some(0) for a clean ChildExit or None when the
    // loop died first.
    session.close();
    let id = session.id();
    pump(&mut session, |_text, events| {
        events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::Terminated { session, status }
                    if *session == id
                        && (status.is_none()
                            || status.as_ref().and_then(|s| s.code()) == Some(0))
            )
        })
    })
    .expect("session must terminate after close within the deadline");

    assert_eq!(session.close(), 0, "second close must release nothing");
}

/// N06a (fixture P1): a size reply must resolve against each session's
/// own window geometry and carry that session's own generation identity.
/// A (`dying 1b5b313474` at 24×80) issues a CSI 14t query and dies with
/// its `\x1b[4;384;640t` reply still in flight; B (`check B ...` at
/// 20×100) issues the same query and waits on its stdin for its own
/// `\x1b[4;320;800t` reply — it must see `CHECK:B`, never `CHECKFAIL`
/// (A's stale reply bytes must not leak into B's stream), and each
/// session's `Terminated` must carry its own `SessionId` with a clean
/// exit status, sticky on every poll after loop end.
#[test]
fn size_replies_routed_to_own_session_stale_generation() {
    let mut a = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["dying".to_string(), "1b5b313474".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("A spawns over a real PTY");
    let mut b = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec![
                "check".to_string(),
                "B".to_string(),
                "1b5b313474".to_string(),
                // `\x1b[4;320;800t`: B's own 20×100 geometry at the
                // nominal 8 px × 16 px cell size.
                "1b5b343b3332303b38303074".to_string(),
            ],
            cwd: None,
            env: Vec::new(),
        },
        20,
        100,
        cap(),
    )
    .expect("B spawns over a real PTY");

    let a_id = a.id();
    let b_id = b.id();
    assert_ne!(a_id, b_id, "spawn ids must differ per session");

    let deadline = Instant::now() + DEADLINE;
    let mut a_events: Vec<SessionEvent> = Vec::new();
    let mut b_events: Vec<SessionEvent> = Vec::new();
    let mut a_ready = false;
    let mut b_checked = false;
    let mut last_a = String::new();
    let mut last_b = String::new();

    while Instant::now() < deadline {
        // An in-flight reply hitting a dead loop reports `Closed`; both
        // sessions may reach that state in this window.
        flush_tolerant(&mut a, 256);
        flush_tolerant(&mut b, 256);

        a_events.append(&mut a.poll_events());
        b_events.append(&mut b.poll_events());

        last_a = grid_text(&mut a);
        last_b = grid_text(&mut b);
        if last_a.contains("READY") {
            a_ready = true;
        }
        if last_b.contains("CHECK:B") {
            b_checked = true;
        }
        assert!(
            !last_b.contains("CHECKFAIL"),
            "B's stream must not fail the check; B grid: {last_b:?}"
        );
        assert!(
            !last_a.contains("CHECK"),
            "A's grid must show only READY; A grid: {last_a:?}"
        );

        let a_terminated = a_events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::Terminated {
                    session,
                    status: Some(status),
                } if *session == a_id && status.code() == Some(0)
            )
        });
        let b_terminated = b_events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::Terminated {
                    session,
                    status: Some(status),
                } if *session == b_id && status.code() == Some(0)
            )
        });
        if a_ready && b_checked && a_terminated && b_terminated {
            break;
        }
        thread::sleep(POLL);
    }

    assert!(a_ready, "A's grid must show READY; last A grid: {last_a:?}");
    assert!(
        b_checked,
        "B must receive its own 20x100 size reply within the deadline; last B grid: {last_b:?}"
    );
    // No cross-tagging: every Terminated in each stream carries that
    // session's own id.
    assert!(a_events.iter().all(|event| {
        matches!(
            event,
            SessionEvent::Terminated { session, .. } if *session == a_id
        )
    }));
    assert!(b_events.iter().all(|event| {
        matches!(
            event,
            SessionEvent::Terminated { session, .. } if *session == b_id
        )
    }));

    // N05b contract: `Terminated` is a state report — sticky on every
    // poll after loop end.
    for _ in 0..3 {
        let a_poll = a.poll_events();
        assert!(a_poll.iter().any(|event| {
            matches!(
                event,
                SessionEvent::Terminated {
                    session,
                    status: Some(status),
                } if *session == a_id && status.code() == Some(0)
            )
        }));
        let b_poll = b.poll_events();
        assert!(b_poll.iter().any(|event| {
            matches!(
                event,
                SessionEvent::Terminated {
                    session,
                    status: Some(status),
                } if *session == b_id && status.code() == Some(0)
            )
        }));
        thread::sleep(POLL);
    }

    let _ = a.close();
    assert_eq!(a.close(), 0, "second close must release nothing");
    let _ = b.close();
    assert_eq!(b.close(), 0, "second close must release nothing");
}

/// N05b: the spawn window size must reach the child's actual TIOCGWINSZ.
#[test]
fn window_size_reaches_child() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["ws".to_string(), "24".to_string(), "80".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        nora::session::pty::DEFAULT_CAP,
    )
    .expect("helper spawns over a real PTY");

    pump(&mut session, |text, _events| text.contains("WS:24;80"))
        .expect("child observed the 24x80 window size within the deadline");

    pump(&mut session, |_text, events| exited_with(events, 0))
        .expect("helper exited 0 within the deadline");

    session.close();
}

/// N06b: a child that sets its window title over the real PTY surfaces
/// the title through `session.title()` (folded on the main thread by
/// `poll_events`), and the child still exits 0 (deadline-bounded, P1
/// shape).
#[test]
fn child_title_surfaces_as_session_title() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["title".to_string(), "nora child".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    // The title arrives between poll ticks; poll until the fold surfaces
    // it (deadline-bounded, P1 shape).
    let deadline = Instant::now() + DEADLINE;
    while session.title().is_none() {
        if Instant::now() > deadline {
            panic!("deadline exceeded waiting for the child title");
        }
        session.poll_events();
        thread::sleep(POLL);
    }
    assert_eq!(session.title(), Some("nora child".to_string()));

    // The child exits 0 after sending the title; observe `Terminated`
    // through the same deadline-bounded pump shape.
    let deadline = Instant::now() + DEADLINE;
    let mut exited = false;
    while !exited {
        if Instant::now() > deadline {
            panic!("deadline exceeded waiting for the child exit");
        }
        exited = session
            .poll_events()
            .iter()
            .any(|event| matches!(event, SessionEvent::Terminated { status: Some(status), .. } if status.code() == Some(0)));
        thread::sleep(POLL);
    }

    session.join();
    session.close();
}

/// `PtySession::resize` moves the engine grid and the child's window
/// together: the engine's next snapshot reports `Damage::Full` at the new
/// geometry (pinned `Term::resize`), the child receives SIGWINCH and reads
/// the new size from the kernel (`TIOCGWINSZ`), and nothing is written to
/// the child's input (audit P1: an unsolicited `CSI 4 ; h ; w t` used to
/// arrive as typed bytes).
#[test]
fn resize_updates_child_and_forces_full_damage() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["winch".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    pump(&mut session, |text, _events| text.contains("READY"))
        .expect("helper ready within the deadline");

    session.resize(10, 40);

    let snap = session.engine().snapshot();
    assert_eq!(
        snap.damage,
        Damage::Full,
        "engine resize must force Full damage"
    );
    assert_eq!((snap.rows, snap.cols), (10, 40));

    pump(&mut session, |text, _events| text.contains("PENDING:"))
        .expect("child reports SIGWINCH within the deadline");
    let text = grid_text(&mut session);
    assert!(
        text.contains("WINCH WS:10;40"),
        "child must read the new size from the kernel; grid: {text:?}"
    );
    assert!(
        text.contains("PENDING:0"),
        "resize must not write to the child's input; grid: {text:?}"
    );
}

/// Lower-case hex for the helper's `check` argv (the helper decodes
/// two-digit pairs; the payload is strictly hex, so nothing is dropped).
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// N09a: the input encoder's pinned rows become the typed bytes that
/// reach the real child in order. `a`, `b` and the multi-byte `é`
/// (UTF-8 must arrive intact through the PTY) plus Enter (`0x0d`) are
/// encoded with `nora::input::encode` (default `BindingTable`,
/// `TermMode::SHOW_CURSOR`, fresh press); the helper's `check` mode
/// matches the exact contiguous byte sequence on stdin and only then
/// prints `CHECK:N09TYPED` (deadline-bounded, P1 shape). The raw slave
/// does no ICRNL/ECHO translation, so the sequence carries no line
/// terminator of its own — `check` needs none.
#[test]
fn typed_bytes_reach_the_child_in_order() {
    // The N09a pinned rows: plain characters (ASCII and multi-byte
    // UTF-8) and Enter, each a fresh press with no modifiers.
    let bindings = BindingTable::default();
    let rows = [
        (Key::Character("a".into()), PhysicalKey::Code(KeyCode::KeyA)),
        (
            Key::Character("b".into()),
            PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
        ),
        (
            Key::Character("é".into()),
            PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
        ),
        (
            Key::Named(NamedKey::Enter),
            PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
        ),
    ];
    let mut input: Vec<u8> = Vec::new();
    for (logical, physical) in rows {
        let action = encode(
            &logical,
            &physical,
            &ModifiersState::default(),
            false,
            &bindings,
            TermMode::SHOW_CURSOR,
        )
        .expect("pinned N09a row must encode");
        let InputAction::Send(bytes) = action else {
            panic!("pinned N09a row must encode to InputAction::Send");
        };
        input.extend(bytes);
    }
    assert_eq!(input, vec![0x61, 0x62, 0xc3, 0xa9, 0x0d]);

    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec![
                "check".to_string(),
                "N09TYPED".to_string(),
                String::new(),
                hex(&input),
            ],
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

    assert_eq!(session.feed(&input), Ok(input.len()));
    session
        .flush(256)
        .expect("flush must forward the typed bytes");

    // Settle before inspecting: the pinned loop's `pty_read` parses master
    // bytes only under `try_lock_unfair` (alacritty_terminal-0.26.0
    // `event_loop.rs`), and a missed try-lock followed by the exited
    // child's EIO read drops the buffered bytes with the loop's state.
    // This check child matches and exits within microseconds of the
    // flush, so the flush tick's own `snapshot` (main-thread term lock)
    // deterministically contends in that window (Round 26 diagnostic:
    // verdict parsed without a main pump, dropped with one). Sleeping
    // here lets match → verdict → exit → parse → terminate complete
    // with the main thread holding no term lock; 100 ms is three orders
    // of magnitude over the observed microsecond-scale window.
    thread::sleep(Duration::from_millis(100));

    pump(&mut session, |text, events| {
        text.contains("CHECK:N09TYPED") && exited_with(events, 0)
    })
    .expect("child matched the exact typed bytes and exited cleanly within the deadline");

    session.close();
}

/// N09a: a 2048-byte payload fed through the bounded writer in 256-byte
/// chunks must reach the real child with no loss and no reordering under
/// backpressure. The helper's `check` mode matches the exact contiguous
/// sequence on stdin and only then prints `CHECK:N09B64`; a dropped or
/// reordered byte fails the match and ends in `CHECKFAIL` on close
/// (deadline-bounded, P1 shape).
#[test]
fn slow_child_input_no_loss_in_order() {
    // 128 × 16 bytes = 2048: deterministic, and exactly eight 256-byte
    // feed windows.
    let unit = "nora-0123456789\n";
    let mut payload: Vec<u8> = Vec::new();
    for _ in 0..128 {
        payload.extend_from_slice(unit.as_bytes());
    }
    assert_eq!(payload.len(), 2048);
    let expect_hex = hex(&payload);

    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec![
                "check".to_string(),
                "N09B64".to_string(),
                String::new(),
                expect_hex,
            ],
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

    // Feed in 256-byte windows. The app budget (192 bytes at the cap)
    // is smaller than a window, so backpressure surfaces as `Ok(0)` or
    // a partial count; the outstanding remainder is retried each tick
    // until the deadline — never dropped.
    let mut offset = 0;
    let deadline = Instant::now() + DEADLINE;
    while offset < payload.len() {
        if Instant::now() > deadline {
            panic!(
                "deadline exceeded feeding the payload; accepted {offset} of {} bytes",
                payload.len()
            );
        }
        let end = (offset + 256).min(payload.len());
        match session.feed(&payload[offset..end]) {
            Ok(accepted) => offset += accepted,
            // The session ending is a test failure the final pump
            // reports with the grid; stop retrying and fail there.
            Err(WriterError::Closed) => break,
            Err(error) => panic!("feed: {error:?}"),
        }
        flush_tolerant(&mut session, 256);
        let _ = session.poll_events();
        thread::sleep(POLL);
    }
    assert_eq!(
        offset,
        payload.len(),
        "every payload byte must be accepted by the writer"
    );
    flush_tolerant(&mut session, 256);

    pump(&mut session, |text, _events| text.contains("CHECK:N09B64"))
        .expect("child observed all 2048 bytes in order within the deadline");

    session.close();
}

/// N11b: a zero resize never reaches the engine — `resize(0, 0)`
/// retains the last valid geometry per axis (no engine resize, no
/// event), so the grid stays at the spawn dimensions on every later
/// snapshot. The first snapshot on a fresh session reports the pinned
/// fresh `Term` damage (alacritty_terminal-0.26.0
/// `TermDamageState::new` is `full: true`; the flag is only cleared by
/// `reset_damage` inside `Engine::snapshot`), so it is Full at the
/// spawn dimensions.
#[test]
fn zero_resize_retains_last_valid_geometry() {
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

    // The first snapshot this session ever takes: the fresh full flag
    // is still set, and a racing `READY` write only expands line
    // spans (it cannot clear the flag), so this fetch is Full at the
    // spawn dimensions.
    let snap = session.engine().snapshot();
    assert_eq!(
        (snap.rows, snap.cols),
        (24, 80),
        "spawn dimensions on the first fetched snapshot"
    );
    assert_eq!(
        snap.damage,
        Damage::Full,
        "fresh term damage is non-noop (Full)"
    );

    // The no-args helper blocks on stdin after READY — ready to echo.
    pump(&mut session, |text, _events| text.contains("READY"))
        .expect("helper printed READY within the deadline");

    session.resize(0, 0);

    // A non-noop tick after the zero resize: the helper echoes the fed
    // line, and the next fetched snapshot must still be at the spawn
    // dimensions — the zero was retained, not applied.
    assert_eq!(session.feed(b"x\n"), Ok(2));
    session
        .flush(256)
        .expect("flush must forward the fed bytes");

    pump(&mut session, |text, _events| text.contains("E:x"))
        .expect("helper echoed the fed line within the deadline");

    let snap = session.engine().snapshot();
    assert_eq!(
        (snap.rows, snap.cols),
        (24, 80),
        "the zero resize retained the last valid geometry"
    );

    session.close();
}

/// N12a: the session lifecycle is a deterministic state machine over the
/// observable surfaces: `spawn` → `Running`; the terminated fold (clean
/// child exit) → `Exiting`; `close()` → `Closed` (idempotent); and the
/// reap-once leg — repeated `poll_events()`/`join()` keep reporting the
/// same `Some(0)` status, never `None` and never another code
/// (deadline-bounded; 100 ms settle before inspecting per the pinned
/// `pty_read` gap).
#[test]
fn lifecycle_transitions_deterministic() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["status".to_string(), "0".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    assert_eq!(
        session.lifecycle(),
        Lifecycle::Running,
        "spawn must be Running"
    );

    pump(&mut session, |_text, events| exited_with(events, 0))
        .expect("terminated fold with status Some(0) within the deadline");

    // Settle before inspecting: the pinned loop's `pty_read` parses master
    // bytes only under a try-lock; a missed try-lock followed by the exited
    // child's EIO read drops buffered bytes with the loop's state.
    thread::sleep(Duration::from_millis(100));

    assert_eq!(
        session.lifecycle(),
        Lifecycle::Exiting,
        "the terminated fold must be visible as Exiting"
    );

    session.close();
    assert_eq!(
        session.lifecycle(),
        Lifecycle::Closed,
        "close must land on Closed"
    );

    // Idempotent: a second close is a no-op that stays `Closed`.
    session.close();
    assert_eq!(
        session.lifecycle(),
        Lifecycle::Closed,
        "a second close must stay Closed (idempotent)"
    );

    // Reap-once leg: the status is a state report — repeated polls and
    // joins keep reporting the same `Some(0)`, never `None` or another code.
    let id = session.id();
    for _ in 0..3 {
        let events = session.poll_events();
        assert!(
            events.iter().any(|event| {
                matches!(
                    event,
                    SessionEvent::Terminated {
                        session: sid,
                        status: Some(status),
                    } if *sid == id && status.code() == Some(0)
                )
            }),
            "Terminated must stay a state report with status Some(0) after close"
        );
        let joined = session.join();
        assert_eq!(
            joined.as_ref().and_then(|status| status.code()),
            Some(0),
            "repeated join() must keep reporting the same status"
        );
        thread::sleep(POLL);
    }
}

/// N12a: final emitted bytes must be visible before normal close. The
/// helper's `argv` mode prints its token line (`A:1 N12FINAL`) and exits
/// 0; after the terminated fold + 100 ms settle, the token must be in the
/// grid — proven by a fresh guard-protected snapshot when there is new
/// damage, else by the grid text the pump last fetched — and `close()`
/// must return well within the drain bound without hanging.
#[test]
fn final_bytes_before_close() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["argv".to_string(), "N12FINAL".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    // The pump's satisfying tick is the last grid text fetched; it must
    // have seen both the token and the exit.
    let mut last = String::new();
    pump(&mut session, |text, events| {
        last = text.to_string();
        text.contains("N12FINAL") && exited_with(events, 0)
    })
    .expect("final token visible and child exited 0 within the deadline");

    // Settle before inspecting (pinned `pty_read` try-lock gap).
    thread::sleep(Duration::from_millis(100));

    // Term-guard idiom: `probe` (non-resetting) gates the next `snapshot`
    // (consumes + resets the damage), the same pair the live pump ticks.
    let (damage, cursor) = session.engine().probe();
    let text = if is_noop_damage(&damage, cursor) {
        // No new damage since the pump's last tick: the grid text it
        // fetched on its satisfying tick already shows the token.
        last
    } else {
        let snap = session.engine().snapshot();
        snap.cells.iter().map(|cell| cell.ch).collect()
    };
    assert!(
        text.contains("N12FINAL"),
        "the grid must show the final token; grid: {text:?}"
    );

    let started = Instant::now();
    session.close();
    let elapsed = started.elapsed();
    assert!(
        elapsed < DRAIN_TIMEOUT,
        "close must return within the drain bound (no hang); elapsed: {elapsed:?}"
    );
    assert_eq!(
        session.lifecycle(),
        Lifecycle::Closed,
        "close must land on Closed"
    );
}

/// N12a: a descendant that inherits the slave fd and holds it must not
/// hang the session. `close()` right after the child exits waits for the
/// pinned `drain_on_exit` read, which stays blocked until the grandchild
/// releases the slave at ~0.3 s — the wait must return well under the
/// drain bound, land on `Closed`, and the grandchild's marker line must
/// reach the grid.
#[test]
fn descendant_bounded_drain() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["descendant".to_string(), "300".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    pump(&mut session, |text, _events| text.contains("READY"))
        .expect("helper ready within the deadline");

    // Close immediately: the loop is still inside the pinned
    // `drain_on_exit` read until the grandchild releases the slave at
    // ~0.3 s, so this exercises the bounded wait inside `close()`.
    let started = Instant::now();
    session.close();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "close must return well under DRAIN_TIMEOUT ({:?}); elapsed: {elapsed:?}",
        DRAIN_TIMEOUT
    );
    assert_eq!(
        session.lifecycle(),
        Lifecycle::Closed,
        "close must land on Closed"
    );

    // The helper is still inside its 0.3 s hold when `close()` runs, so
    // whether `DESCENDANT-DONE` (written after the hold) reaches the grid
    // races the shutdown: output produced after an explicit close is
    // legitimately discarded. It reached the grid locally but not on a
    // GitHub runner (CI run 38792616363), so it is not asserted; the
    // bounded wait and the `Closed` landing above are the contract.
}

/// Open file-descriptor count for this process (`/proc/self/fd` on Linux,
/// `/dev/fd` on macOS and the BSDs).
fn fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .or_else(|_| std::fs::read_dir("/dev/fd"))
        .expect("an fd directory is readable")
        .count()
}

/// N12a: repeated spawn/close must not leak file descriptors. The
/// property checked is no unbounded growth per spawn/close — the margin
/// absorbs other test sessions running concurrently in this process
/// (integration tests share one process, and this binary's ~17 PTY tests
/// run in parallel, each holding its PTY fds; one parallel run observed
/// a net +26 within the window), not an exact per-cycle count.
#[test]
fn repeated_spawn_close_no_fd_leak() {
    let baseline = fd_count();

    for _ in 0..3 {
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
            .expect("helper ready within the deadline");

        session.close();
    }

    let final_count = fd_count();
    assert!(
        final_count <= baseline + 48,
        "fd count must not grow beyond baseline + 48; baseline: {baseline}, final: {final_count}"
    );
}

/// N12a: `close()` before the session is dropped must be clean — no
/// panic, no hang; this is the headless approximation of the live
/// stop-arm leg (the pump's close-before-break path).
#[test]
fn close_before_drop_clean() {
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

    session.close();
    assert_eq!(
        session.lifecycle(),
        Lifecycle::Closed,
        "close must land on Closed"
    );

    // Explicit drop after close: must not panic or block on a closed session.
    drop(session);
}

/// ADR-014: a kitty graphics command from a child over a real PTY is
/// handled on the reader thread at its place in the stream, before the
/// `APC-DONE` text after it is parsed: the image is stored and placed.
#[test]
fn pty_graphics_command_reaches_the_state() {
    // `/wAA/w==` is one red RGBA pixel.
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["apc".to_string(), "/wAA/w==".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    // Wait for the APC-DONE marker. By the time the marker is visible the
    // APC has already been filtered (the filter runs before the parser in
    // the same or an earlier `pty_read`), so the event is in the queue.
    pump(&mut session, |text, _events| text.contains("APC-DONE"))
        .expect("APC-DONE marker observed within the deadline");

    let engine = session.engine();
    let graphics = engine.graphics();
    let image = graphics
        .image(7)
        .expect("image 7 stored by the reader thread");
    assert_eq!(&*image.pixels, &[255, 0, 0, 255]);
    assert_eq!(graphics.placements().count(), 1, "a=T displayed it");
    drop(graphics);

    session.close();
}

/// N35: a PTY session whose child emits shell-integration frames (OSC 133
/// A/B/C/D markers, an OSC 7 cwd, an OSC 8 hyperlink) produces the events
/// in the shared state: the four markers in the marker queue in order, the
/// decoded path in the shared cwd slot, and the URI in the hyperlink queue.
/// The `PromptMarkerFilter` runs on the PTY reader thread *before* the
/// parser, so by the time `OSC-DONE` is visible in the grid every event
/// has already been queued.
/// ADR-013 step 1 over a real PTY: the editor buffer is current while the
/// prompt waits for input; the executed command line arrives with the cwd
/// and exit status the shell reported.
#[test]
fn pty_shell_input_reports_reach_shared_state() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["cmdline".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    pump(&mut session, |text, _events| text.contains("BUF-SHOWN"))
        .expect("BUF-SHOWN within the deadline");
    let buffer = session.engine().shell_buffer().expect("buffer is current");
    assert_eq!((buffer.text.as_str(), buffer.cursor), ("make all", 8));

    pump(&mut session, |text, _events| text.contains("CMD-DONE"))
        .expect("CMD-DONE within the deadline");
    assert_eq!(session.engine().shell_buffer(), None);
    let executed = session.engine().take_executed();
    assert_eq!(executed.len(), 1, "{executed:?}");
    assert_eq!(executed[0].cmdline, "make all");
    assert_eq!(executed[0].cwd.as_deref(), Some("/tmp"));
    assert_eq!(executed[0].exit, Some(2));

    session.close();
}

#[test]
fn pty_shell_events_reach_shared_state() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["osc".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    // Wait for the OSC-DONE marker. By the time the marker is visible the
    // frames have already been filtered (the filter runs before the parser
    // in the same or an earlier `pty_read`), so the events are queued.
    pump(&mut session, |text, _events| text.contains("OSC-DONE"))
        .expect("OSC-DONE marker observed within the deadline");

    let markers = session.engine().drain_markers();
    let kinds: Vec<MarkerKind> = markers.iter().map(|marker| marker.event.kind).collect();
    assert_eq!(
        kinds,
        vec![MarkerKind::A, MarkerKind::B, MarkerKind::C, MarkerKind::D],
        "expected markers A, B, C, D in order; got {kinds:?}"
    );

    assert_eq!(
        session.engine().cwd(),
        Some("/tmp".to_string()),
        "OSC 7 file:///tmp must land in the shared cwd slot"
    );

    let links = session.engine().drain_hyperlinks();
    assert!(
        links.iter().any(|link| link.uri == "https://example.com/x"),
        "expected hyperlink https://example.com/x; got {links:?}"
    );

    session.close();
}

/// N35a: the opt-in `shell/nora-bash.sh` integration must produce a clean
/// OSC 133 sequence for a real interactive bash session: `D A` around each
/// prompt (from PROMPT_COMMAND) and `B C` before each user command (from
/// the DEBUG trap) — and no spurious `B C` pair on the prompt line between
/// `D` and `A`. bash fires the DEBUG trap for PROMPT_COMMAND entries too;
/// the script must skip those, otherwise the engine's C-to-D
/// previous-command selection (N35c) would select the prompt line instead
/// of the command. For the two commands sent here the full sequence is
/// `D A` (first prompt) `B C` (first command) `D A` (second prompt)
/// `B C` (second command).
#[test]
fn pty_bash_shell_integration_marker_order() {
    let script = std::env::current_dir()
        .expect("test CWD is the package root")
        .join("shell/nora-bash.sh");
    let rc = std::env::temp_dir().join(format!("nora-bash-rc-{}", std::process::id()));
    std::fs::write(&rc, format!("source {}\n", script.display())).expect("write the rcfile");

    // bash is in the nix store on this cluster; fall back to PATH.
    let bash = [
        "/run/current-system/sw/bin/bash",
        "/usr/bin/bash",
        "/bin/bash",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).exists())
    .map(|path| path.to_string())
    .unwrap_or_else(|| "bash".to_string());

    let mut session = PtySession::spawn(
        LaunchSpec {
            program: bash,
            // `--rcfile` must precede `-i`: bash 5.3 rejects `--rcfile`
            // after `-i` ("--: invalid option").
            args: vec![
                "--rcfile".to_string(),
                rc.to_string_lossy().into_owned(),
                "-i".to_string(),
            ],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("bash spawns over a real PTY");

    // Wait until the first prompt is visible before feeding, so the command
    // is read as the first line after the first prompt rather than racing
    // bash's startup (sourcing /etc/bashrc, drawing the prompt).
    pump(&mut session, |text, _events| !text.trim().is_empty())
        .expect("first prompt visible within the deadline");

    // The rcfile sourced the integration before the first prompt, so the
    // first prompt already carries the D A pair. Send one command, then
    // exit.
    session
        .feed(b"echo marker-test-output\n")
        .expect("feed the test command");
    session.flush(256).expect("flush the test command to bash");
    pump(&mut session, |text, _events| {
        text.contains("marker-test-output")
    })
    .expect("test command output observed within the deadline");
    session.feed(b"exit\n").expect("feed exit");
    session.flush(256).expect("flush exit to bash");
    pump(&mut session, |_text, events| exited_with(events, 0))
        .expect("bash exits cleanly within the deadline");

    let markers = session.engine().drain_markers();
    let kinds: Vec<MarkerKind> = markers.iter().map(|marker| marker.event.kind).collect();
    assert_eq!(
        kinds,
        vec![
            MarkerKind::D,
            MarkerKind::A,
            MarkerKind::B,
            MarkerKind::C,
            MarkerKind::D,
            MarkerKind::A,
            MarkerKind::B,
            MarkerKind::C
        ],
        "expected D A (first prompt), B C before the first command, D A \
         (second prompt), B C before the second command — with no spurious \
         B C pair between D and A; got {kinds:?}"
    );

    session.close();
    let _ = std::fs::remove_file(&rc);
}

/// `shell/nora-bash.sh` emits one `B C` pair per command line: a loop
/// piped into another command (many simple commands) gets one pair; a
/// user command whose text also appears in PROMPT_COMMAND (`history -a`)
/// still gets its pair; and a DEBUG trap installed before the script
/// keeps running (chained, not replaced).
#[test]
fn pty_bash_one_marker_pair_per_command_line() {
    let script = std::env::current_dir()
        .expect("test CWD is the package root")
        .join("shell/nora-bash.sh");
    let rc = std::env::temp_dir().join(format!("nora-bash-rc-pairs-{}", std::process::id()));
    std::fs::write(
        &rc,
        format!(
            "PROMPT_COMMAND='history -a'\n\
             trap 'prev_count=$((prev_count + 1))' DEBUG\n\
             source {}\n",
            script.display()
        ),
    )
    .expect("write the rcfile");
    let bash = [
        "/run/current-system/sw/bin/bash",
        "/usr/bin/bash",
        "/bin/bash",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).exists())
    .unwrap_or("bash")
    .to_string();
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: bash,
            args: vec![
                "--rcfile".to_string(),
                rc.to_string_lossy().into_owned(),
                "-i".to_string(),
            ],
            cwd: None,
            // No user history file is touched by `history -a`.
            env: vec![("HISTFILE".to_string(), "/dev/null".to_string())],
        },
        24,
        80,
        cap(),
    )
    .expect("bash spawns over a real PTY");
    pump(&mut session, |text, _events| !text.trim().is_empty()).expect("first prompt");

    // Each command with the text its output adds and how often that text
    // is on screen once it ran (the echoed command line may contain it).
    for (line, done, count) in [
        (
            &b"for i in 1 2 3; do echo loop-$i; done | cat\n"[..],
            "loop-3",
            1,
        ),
        (&b"history -a\n"[..], "", 0),
        (
            &b"prev_count=0; [ \"${prev_count:-0}\" -gt 0 ] && echo PREV-TRAP-OK\n"[..],
            "PREV-TRAP-OK",
            2,
        ),
    ] {
        session.feed(line).expect("feed a command");
        session.flush(256).expect("flush the command to bash");
        if count > 0 {
            pump(&mut session, |text, _events| {
                text.matches(done).count() >= count
            })
            .unwrap_or_else(|error| panic!("{done}: {error}"));
        }
    }
    session.feed(b"exit\n").expect("feed exit");
    session.flush(256).expect("flush exit to bash");
    pump(&mut session, |_text, events| exited_with(events, 0))
        .expect("bash exits cleanly within the deadline");

    let kinds: Vec<MarkerKind> = session
        .engine()
        .drain_markers()
        .iter()
        .map(|marker| marker.event.kind)
        .collect();
    use MarkerKind::{A, B, C, D};
    assert_eq!(
        kinds,
        vec![D, A, B, C, D, A, B, C, D, A, B, C, D, A, B, C],
        "one D A per prompt and one B C per command line; got {kinds:?}"
    );
    session.close();
    let _ = std::fs::remove_file(&rc);
}

/// Audit P4: input to a child that never reads stdin stays bounded end to
/// end: the session writer holds at most its budget and at most
/// `MAX_IN_FLIGHT` bytes sit between nora and the PTY, however much is
/// offered (before, every flush released the budget into an unbounded
/// channel and write list).
#[test]
fn input_to_a_non_reading_child_is_bounded() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: "/bin/sh".to_string(),
            // Raw mode: the kernel applies backpressure instead of
            // discarding overlong canonical lines.
            args: vec!["-c".to_string(), "stty raw -echo; sleep 20".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        nora::session::pty::DEFAULT_CAP,
    )
    .expect("sh spawns over a real PTY");
    thread::sleep(Duration::from_millis(300));
    let chunk = vec![b'x'; 64 * 1024];
    let offered = 16 * 1024 * 1024;
    let mut accepted = 0usize;
    for _ in 0..(offered / chunk.len()) {
        accepted += session.feed(&chunk).unwrap_or(0);
        let _ = session.flush(usize::MAX);
        thread::sleep(Duration::from_millis(1));
    }
    let in_flight = session.in_flight();
    assert!(
        in_flight <= nora::session::pty::MAX_IN_FLIGHT,
        "in flight {in_flight} exceeds the cap"
    );
    // Writer budget + in-flight cap + what the kernel buffered: far below
    // the 16 MiB offered.
    assert!(
        accepted <= 4 * 1024 * 1024,
        "accepted {accepted} of {offered}: input was not held back"
    );
    session.close();
}

/// ADR-013: password input is canonical mode with echo off (`read -s`,
/// `sudo`); cooked mode and a line editor's raw mode without echo (zsh's
/// ZLE runs in `-icanon -echo`) are not, and a closed session reports it
/// as unknown, which counts as password input.
#[test]
#[cfg(unix)]
fn password_input_follows_the_child_line_discipline() {
    for (setup, expected) in [
        ("stty -echo", true),
        ("stty echo", false),
        ("stty -icanon -echo", false),
    ] {
        let mut session = PtySession::spawn(
            LaunchSpec {
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), format!("{setup}; echo READY; sleep 20")],
                cwd: None,
                env: Vec::new(),
            },
            24,
            80,
            cap(),
        )
        .expect("sh spawns over a real PTY");
        pump(&mut session, |text, _events| text.contains("READY"))
            .unwrap_or_else(|e| panic!("{setup}: child ready: {e}"));
        assert_eq!(session.password_input(), expected, "after `{setup}`");
        session.close();
        assert!(session.password_input(), "closed session after `{setup}`");
    }
}

/// ADR-013 against a real zsh with `shell/nora-zsh.zsh`: while ZLE edits
/// a reported buffer the terminal is not in password input (ZLE turns echo
/// off but leaves canonical mode), so suggestions stay on; `read -s` puts
/// it in password input with no buffer current. Skipped without zsh.
#[test]
#[cfg(unix)]
fn pty_zsh_editing_is_not_password_input() {
    let Some(zsh) = ["/usr/bin/zsh", "/bin/zsh", "/run/current-system/sw/bin/zsh"]
        .into_iter()
        .find(|path| std::path::Path::new(path).exists())
    else {
        eprintln!("zsh not found; skipping");
        return;
    };
    let script = std::env::current_dir()
        .expect("test CWD is the package root")
        .join("shell/nora-zsh.zsh");
    let zdotdir = std::env::temp_dir().join(format!("nora-zsh-rc-{}", std::process::id()));
    std::fs::create_dir_all(&zdotdir).expect("create the ZDOTDIR");
    std::fs::write(
        zdotdir.join(".zshrc"),
        format!("PS1='$ '\nsource {}\n", script.display()),
    )
    .expect("write the zshrc");

    let mut session = PtySession::spawn(
        LaunchSpec {
            program: zsh.to_string(),
            // `-d`: no global rc files, only the ZDOTDIR one.
            args: vec!["-d".to_string(), "-i".to_string()],
            cwd: None,
            env: vec![(
                "ZDOTDIR".to_string(),
                zdotdir.to_string_lossy().into_owned(),
            )],
        },
        24,
        80,
        cap(),
    )
    .expect("zsh spawns over a real PTY");

    let wait = |session: &mut PtySession, what: &str, done: &dyn Fn(&mut PtySession) -> bool| {
        let deadline = Instant::now() + DEADLINE;
        while !done(session) {
            let _ = session.poll_events();
            let _ = session.flush(256);
            assert!(
                Instant::now() < deadline,
                "{what} within the deadline; grid: {:?}",
                grid_text(session).trim_end()
            );
            thread::sleep(POLL);
        }
    };
    wait(&mut session, "prompt", &|s| grid_text(s).contains("$ "));
    session.feed(b"ech").expect("feed a partial command");
    wait(&mut session, "buffer report", &|s| {
        s.engine().shell_buffer().is_some_and(|b| b.text == "ech")
    });
    assert!(
        !session.password_input(),
        "ZLE editing is not password input"
    );

    // Ctrl+U clears the line; `read -s` then waits for a hidden line.
    session.feed(b"\x15read -s x\n").expect("feed read -s");
    wait(&mut session, "password input", &|s| {
        s.engine().shell_buffer().is_none() && s.password_input()
    });
    session.feed(b"pw\nexit\n").expect("feed the line and exit");
    session.flush(256).expect("flush the line and exit to zsh");
    pump(&mut session, |_text, events| exited_with(events, 0))
        .expect("zsh exits cleanly within the deadline");
    session.close();
    let _ = std::fs::remove_dir_all(&zdotdir);
}

/// `shell/nora-zsh.zsh` honors `HISTORY_IGNORE`: an ignored command is
/// marked (`C`) without its command line, so nora never records it, while
/// other commands are still reported. Skipped without zsh.
#[test]
#[cfg(unix)]
fn pty_zsh_history_ignore_keeps_commands_unreported() {
    let Some(zsh) = ["/usr/bin/zsh", "/bin/zsh", "/run/current-system/sw/bin/zsh"]
        .into_iter()
        .find(|path| std::path::Path::new(path).exists())
    else {
        eprintln!("zsh not found; skipping");
        return;
    };
    let script = std::env::current_dir()
        .expect("test CWD is the package root")
        .join("shell/nora-zsh.zsh");
    let zdotdir = std::env::temp_dir().join(format!("nora-zsh-histignore-{}", std::process::id()));
    std::fs::create_dir_all(&zdotdir).expect("create the ZDOTDIR");
    std::fs::write(
        zdotdir.join(".zshrc"),
        format!(
            "PS1='$ '\nHISTORY_IGNORE='(echo hidden*|ls)'\nsource {}\n",
            script.display()
        ),
    )
    .expect("write the zshrc");
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: zsh.to_string(),
            args: vec!["-d".to_string(), "-i".to_string()],
            cwd: None,
            env: vec![(
                "ZDOTDIR".to_string(),
                zdotdir.to_string_lossy().into_owned(),
            )],
        },
        24,
        80,
        cap(),
    )
    .expect("zsh spawns over a real PTY");
    pump(&mut session, |text, _events| text.contains("$ ")).expect("prompt");
    session
        .feed(b"echo hidden-one\necho kept-one\nexit\n")
        .expect("feed the commands");
    session.flush(256).expect("flush the commands to zsh");
    pump(&mut session, |_text, events| exited_with(events, 0))
        .expect("zsh exits cleanly within the deadline");
    let cmdlines: Vec<String> = session
        .engine()
        .take_executed()
        .into_iter()
        .map(|command| command.cmdline)
        .collect();
    assert!(
        cmdlines.iter().any(|line| line == "echo kept-one"),
        "a normal command is reported; got {cmdlines:?}"
    );
    assert!(
        !cmdlines.iter().any(|line| line.contains("hidden")),
        "an ignored command is not reported; got {cmdlines:?}"
    );
    session.close();
    let _ = std::fs::remove_dir_all(&zdotdir);
}

/// Audit P6: a child that ignores SIGHUP used to block the loop's PTY
/// teardown forever (the loop thread, the child and the PTY fds leaked).
/// `close` now kills its process group after `CLOSE_GRACE` and returns
/// with the child gone.
#[test]
fn close_kills_a_child_that_ignores_sighup() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "trap '' HUP; echo HUP-IGNORED; sleep 60".to_string(),
            ],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("sh spawns over a real PTY");
    pump(&mut session, |text, _events| text.contains("HUP-IGNORED"))
        .expect("the child installed its trap");
    let pid = session.child_pid();
    let started = Instant::now();
    session.close();
    let elapsed = started.elapsed();
    assert_eq!(session.lifecycle(), Lifecycle::Closed);
    assert!(
        elapsed < nora::session::pty::CLOSE_GRACE + Duration::from_secs(2),
        "close took {elapsed:?}"
    );
    thread::sleep(Duration::from_millis(200));
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "the HUP-ignoring child {pid} must be gone"
    );
}
