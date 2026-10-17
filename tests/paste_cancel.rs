//! Ctrl+C or Escape cancels the rest of a paste a child is reading slowly
//! or not at all: the pump hands a session at most `PASTE_WINDOW` paste
//! bytes at a time, keeps the rest, and drops it when such a key arrives;
//! the key follows the paste's end marker.

use std::time::Duration;

use nora::session::pty::{DEFAULT_CAP, LaunchSpec, PtySession};
use nora::session::pump::{
    PASTE_WINDOW, PasteBlock, apply_paste, feed_pending, is_paste_cancel_key, pump_tick,
    queue_key_input,
};

#[test]
fn cancel_keys() {
    for key in [
        &b"\x03"[..],
        b"\x1b",
        b"\x1b[99;5u",
        b"\x1b[99;5:1u",
        b"\x1b[99;69u", // Caps Lock on
        b"\x1b[27u",
        b"\x1b[27;1u",
        b"\x1b[27;1:1u",
    ] {
        assert!(is_paste_cancel_key(key), "{key:?} cancels");
    }
    for key in [
        &b"c"[..],
        b"\x1bc",         // Alt+C
        b"\x1b[99;6u",    // Ctrl+Shift+C
        b"\x1b[99;5:3u",  // release
        b"\x1b[27;1:2u",  // repeat
        b"\x1b[27;3u",    // Alt+Escape
        b"\x1b[13u",      // Enter
        b"\x1b[99;5;99u", // text field
        b"\x1b[A",
    ] {
        assert!(!is_paste_cancel_key(key), "{key:?} does not cancel");
    }
}

#[test]
fn ctrl_c_cancels_the_rest_and_follows_the_end_marker() {
    let mut pending = Vec::new();
    let mut block: Option<PasteBlock> = None;
    apply_paste(&mut pending, &mut block, "hello world", true);
    // The start marker and "he" reached the session.
    pending.drain(..8);
    nora::session::pump::advance_paste_block(&mut block, 8);
    assert!(queue_key_input(&mut pending, &mut block, b"k"));
    assert!(block.is_some(), "an ordinary key does not cancel");
    assert!(queue_key_input(&mut pending, &mut block, b"\x03"));
    assert_eq!(block, None);
    assert_eq!(pending, b"\x1b[201~k\x03".to_vec());
}

#[test]
fn a_paste_to_a_child_that_does_not_read_stays_cancellable() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            // Raw mode: the line discipline stops taking input once its
            // buffer is full (canonical mode keeps taking and drops it).
            program: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "stty raw -echo; echo READY; sleep 30".to_string(),
            ],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        DEFAULT_CAP,
    )
    .expect("spawn sh over a real PTY");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let outcome = pump_tick(&mut session);
        let text: Option<String> = outcome
            .snapshot
            .map(|snap| snap.cells.iter().map(|c| c.ch).collect());
        if text.is_some_and(|t| t.contains("READY")) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the child got ready");
        std::thread::sleep(Duration::from_millis(10));
    }

    let text = "x".repeat(900 * 1024);
    let mut pending = Vec::new();
    let mut block: Option<PasteBlock> = None;
    apply_paste(&mut pending, &mut block, &text, true);
    let queued = pending.len();
    for _ in 0..30 {
        feed_pending(&mut session, &mut pending, &mut block);
        let _ = session.flush(1024 * 1024);
        std::thread::sleep(Duration::from_millis(5));
    }
    let handed = queued - pending.len();
    assert!(
        session.outstanding() + session.in_flight() <= PASTE_WINDOW,
        "the session holds at most the paste window"
    );
    // What left the pump is the window plus what the kernel took.
    assert!(handed < 400 * 1024, "{handed} byte(s) left the pump");

    assert!(queue_key_input(&mut pending, &mut block, b"\x03"));
    assert_eq!(block, None);
    assert_eq!(
        pending,
        b"\x1b[201~\x03".to_vec(),
        "end marker, then the key"
    );
    session.close();
}
