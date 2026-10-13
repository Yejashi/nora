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

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::term::TermMode;
use nora::engine::Damage;
use nora::input::{BindingTable, InputAction, encode};
use nora::session::io::{WriterConfig, WriterError};
use nora::session::pty::{LaunchSpec, PtyError, PtySession, SessionEvent};
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
            return Err(format!(
                "deadline exceeded; last grid: {last:?}; saw {} event(s)",
                events.len()
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

/// N08b1: `PtySession::resize` must move the shared size and the
/// engine grid together, and the fresh `TextAreaSizeRequest` it
/// enqueues must credit this session's own ledger: the child's size
/// reply reflects the new 10×40 geometry (deadline-bounded, P1
/// cross-session test shape), and the engine's next snapshot reports
/// `Damage::Full` (pinned `Term::resize` ⇒ Full, alacritty_terminal
/// term/mod.rs:699-701).
#[test]
fn resize_updates_child_and_forces_full_damage() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec![
                "check".to_string(),
                "R".to_string(),
                // Empty query: the child writes nothing and waits on
                // stdin for the exact reply that `resize()` enqueues.
                String::new(),
                // `\x1b[4;160;320t`: 10×40 at the nominal 8 px × 16 px
                // cell size.
                "1b5b343b3136303b33323074".to_string(),
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
        .expect("helper ready within the deadline");

    session.resize(10, 40);

    // The engine's next snapshot reports Full damage (pinned
    // `Term::resize` ⇒ full damage, term/mod.rs:699-701) — asserted
    // before the loop below, whose per-tick `grid_text` snapshots and
    // resets damage.
    let snap = session.engine().snapshot();
    assert_eq!(
        snap.damage,
        Damage::Full,
        "engine resize must force Full damage"
    );
    assert_eq!((snap.rows, snap.cols), (10, 40));

    // The reply — resolved against the updated 10×40 size and credited
    // through the ledger — must reach the child, which verifies the
    // contiguous bytes and emits `CHECK:R` (deadline-bounded, P1 shape:
    // flush each tick so queued replies actually leave the master).
    let deadline = Instant::now() + DEADLINE;
    let mut events: Vec<SessionEvent> = Vec::new();
    let mut checked = false;
    while Instant::now() < deadline {
        flush_tolerant(&mut session, 256);
        let mut fresh = session.poll_events();
        events.append(&mut fresh);
        if grid_text(&mut session).contains("CHECK:R") {
            checked = true;
            break;
        }
        thread::sleep(POLL);
    }
    assert!(
        checked,
        "child observed the resized 10x40 geometry within the deadline; saw {} event(s)",
        events.len()
    );

    session.close();
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
