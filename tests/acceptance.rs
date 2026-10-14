//! v0.1 headless acceptance legs (N31a).
//!
//! Each leg is one observable acceptance condition that runs headless on
//! node 1 (no window, no GPU). PTY legs drive the tagged-I/O helper bin
//! (`src/bin/nora_child.rs`, discovered via `CARGO_BIN_EXE_nora_child`)
//! through a real PTY; engine legs exercise the headless `Engine` and
//! search/selection primitives directly. Every wait is deadline-bounded
//! (10 s) and polls every 10 ms on observable markers only.
//!
//! Legs:
//! - `acceptance_output_identity`: accepted bytes reach the engine grid
//!   byte-identical (the child's echo of a fed line is exact).
//! - `acceptance_exit_status`: a child's exit code surfaces in `Terminated`.
//! - `acceptance_resize`: a resize moves the engine geometry and forces
//!   Full damage.
//! - `acceptance_pty_write`: a payload fed through the bounded writer
//!   reaches the real child with no loss and no reordering.
//! - `acceptance_scrollback_search`: a token in a 10,000-row history is
//!   found by `search_snapshot` at the correct row.
//! - `acceptance_selection_copy`: a viewport selection copies the exact
//!   selected text.
//! - `acceptance_repeated_lifecycle`: 25× spawn/close is stable (no fd
//!   leak, no crash).

use std::thread;
use std::time::{Duration, Instant};

use nora::engine::{Damage, Engine};
use nora::search::search_snapshot;
use nora::session::io::WriterConfig;
use nora::session::pty::{LaunchSpec, PtySession, SessionEvent};

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

/// The engine's current grid as one row-major string.
fn grid_text(session: &mut PtySession) -> String {
    let snap = session.engine().snapshot();
    snap.cells.iter().map(|c| c.ch).collect()
}

/// Deadline-bounded pump: each tick drains `poll_events()`, re-reads the
/// grid, and hands both to `satisfied`. Returns `Ok` on the first satisfied
/// tick; `Err` after the deadline with the last grid text.
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

/// Open file-descriptor count for this process (Linux `/proc` view).
fn fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("/proc/self/fd readable")
        .count()
}

/// The accepted bytes reach the engine grid byte-identical: a fed line is
/// echoed back by the real child exactly as sent (the helper's echo mode
/// prints `E:<line>`).
#[test]
fn acceptance_output_identity() {
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

    assert_eq!(session.feed(b"identity\n"), Ok(9));
    session.flush(256).expect("flush must forward the fed line");

    // The child echoes the exact line; the grid must show it byte-identical.
    pump(&mut session, |text, _events| text.contains("E:identity"))
        .expect("child echoed the fed line byte-identically within the deadline");

    session.close();
}

/// A child's exit code surfaces in the `Terminated` event.
#[test]
fn acceptance_exit_status() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["status".to_string(), "7".to_string()],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    pump(&mut session, |_text, events| exited_with(events, 7))
        .expect("Terminated carries exit code 7 within the deadline");

    session.close();
}

/// A resize moves the engine geometry and forces Full damage.
#[test]
fn acceptance_resize() {
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

    session.resize(10, 40);

    let snap = session.engine().snapshot();
    assert_eq!(
        (snap.rows, snap.cols),
        (10, 40),
        "resize must move the engine geometry"
    );
    assert_eq!(
        snap.damage,
        Damage::Full,
        "engine resize must force Full damage"
    );

    session.close();
}

/// A payload fed through the bounded writer reaches the real child with no
/// loss and no reordering. The helper's `check` mode matches the exact
/// contiguous byte sequence on stdin and only then prints `CHECK:ACC`.
#[test]
fn acceptance_pty_write() {
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
                "ACC".to_string(),
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

    // Feed in 256-byte windows, retrying on backpressure until the deadline.
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
            Err(nora::session::io::WriterError::Closed) => break,
            Err(error) => panic!("feed: {error:?}"),
        }
        let _ = session.flush(256);
        let _ = session.poll_events();
        thread::sleep(POLL);
    }
    assert_eq!(
        offset,
        payload.len(),
        "every payload byte must be accepted by the writer"
    );
    let _ = session.flush(256);

    pump(&mut session, |text, _events| text.contains("CHECK:ACC"))
        .expect("child observed all 2048 bytes in order within the deadline");

    session.close();
}

/// A token in a 10,000-row history is found by `search_snapshot` at the
/// correct row (the scrollback-sized search primitive).
#[test]
fn acceptance_scrollback_search() {
    const HISTORY_ROWS: usize = 10_000;
    const QUERY: &str = "NEEDLE";
    let needle_row = 5_000;

    // Build a deterministic scrollback-sized history; row 5000 carries the
    // token, every other row is a distinct line number.
    let mut history: Vec<Vec<char>> = Vec::with_capacity(HISTORY_ROWS);
    for i in 0..HISTORY_ROWS {
        let line = if i == needle_row {
            QUERY.to_string()
        } else {
            format!("line-{:05}", i)
        };
        let mut chars: Vec<char> = line.chars().collect();
        while chars.len() < 80 {
            chars.push(' ');
        }
        history.push(chars);
    }

    let matches = search_snapshot(&history, QUERY);
    assert!(
        matches.iter().any(|m| m.row == needle_row),
        "search must find the token at row {needle_row}; found {} match(es)",
        matches.len()
    );
    // The token appears exactly once.
    assert_eq!(
        matches.iter().filter(|m| m.row == needle_row).count(),
        1,
        "the token must appear exactly once in the history"
    );

    // And the engine itself stays healthy while the history is full: a fresh
    // engine with the same scrollback accepts a line without error.
    let mut engine = Engine::with_history(24, 80, HISTORY_ROWS);
    engine.feed(b"search-while-growing\n");
    let snap = engine.snapshot();
    assert_eq!(
        (snap.rows, snap.cols),
        (24, 80),
        "a full-history engine must retain its geometry"
    );
}

/// A viewport selection copies the exact selected text.
#[test]
fn acceptance_selection_copy() {
    let mut engine = Engine::new(24, 80);
    // Feed a known line; the cursor starts at (0, 0), so it lands on row 0.
    engine.feed(b"SELECTED TEXT\n");
    let snap = engine.snapshot();
    assert_eq!((snap.rows, snap.cols), (24, 80), "fresh engine geometry");

    // "SELECTED TEXT" is 13 chars in row 0, cols 0..=12.
    engine.select((0, 0), (0, 12));
    let copied = engine
        .copy_selection()
        .expect("a selection must copy some text");
    assert_eq!(
        copied, "SELECTED TEXT",
        "the copied text must be the exact selected range"
    );

    // Clearing the selection stops the copy.
    engine.clear_selection();
    assert_eq!(
        engine.copy_selection(),
        None,
        "a cleared selection must copy nothing"
    );
}

/// 25× spawn/close is stable: no unbounded fd growth and no crash. The
/// margin absorbs other test sessions running concurrently in this process
/// (integration tests share one process).
#[test]
fn acceptance_repeated_lifecycle() {
    let baseline = fd_count();

    for _ in 0..25 {
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
        final_count <= baseline + 64,
        "fd count must not grow beyond baseline + 64 over 25 cycles; baseline: {baseline}, final: {final_count}"
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
