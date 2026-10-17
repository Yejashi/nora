//! N17b regression suite for the paste pump wiring
//! (`nora::session::pump::{apply_paste, cancel_paste,
//! advance_paste_block, feed_pending, PasteApply}`).
//!
//! The pure state helpers are headless (no winit, no PTY, no clocks). One
//! end-to-end test drives a validated paste through a real PTY session
//! (`feed_pending`) and asserts the child receives the exact bytes in
//! order.
//!
//! Pinned behavior (verified against `src/session/pump.rs`):
//! - `apply_paste` validates with the N17a policy and appends the bytes to
//!   the pending input, recording the in-flight block as a half-open range;
//!   a new paste replaces the previous block (only the most recent is
//!   cancellable);
//! - `cancel_paste` removes the block's bytes and leaves keystrokes on
//!   either side untouched;
//! - `advance_paste_block` shifts the block left as bytes drain, and
//!   clears it when fully drained;
//! - `feed_pending` feeds the pending input in order through the bounded
//!   writer, advancing the block as bytes drain.

use std::thread;
use std::time::{Duration, Instant};

use nora::engine::Snapshot;
use nora::input::paste::MAX_PASTE_SIZE;
use nora::session::io::WriterConfig;
use nora::session::pty::{LaunchSpec, PtySession};
use nora::session::pump::{
    PasteApply, PasteBlock, advance_paste_block, apply_paste, cancel_paste, feed_pending, pump_tick,
};

// --- pure state helpers ---------------------------------------------------

/// The block's pending span `[start, end)`.
fn span(block: &Option<PasteBlock>) -> Option<(usize, usize)> {
    block.map(|b| (b.start, b.end))
}

/// An unbracketed block nothing of which was fed yet.
fn unfed(start: usize, end: usize) -> PasteBlock {
    PasteBlock {
        start,
        end,
        fed: 0,
        bracketed: false,
    }
}

/// Cancelling a bracketed paste whose start marker was already fed keeps
/// the end marker queued, so the application leaves paste mode.
#[test]
fn cancel_started_bracketed_paste_keeps_the_end_marker() {
    let mut pending: Vec<u8> = Vec::new();
    let mut block: Option<PasteBlock> = None;
    apply_paste(&mut pending, &mut block, "hello world", true);
    pending.extend_from_slice(b"K");
    // The start marker and "he" reached the child.
    pending.drain(..8);
    advance_paste_block(&mut block, 8);
    let dropped = cancel_paste(&mut pending, &mut block);
    assert_eq!(dropped, "llo world".len());
    assert_eq!(pending, b"\x1b[201~K".to_vec());
    assert_eq!(block, None);
}

/// A cancel never cuts a UTF-8 character the fed part started, and keeps
/// the rest of a partly fed start marker.
#[test]
fn cancel_completes_a_cut_character_and_marker() {
    let mut pending: Vec<u8> = Vec::new();
    let mut block: Option<PasteBlock> = None;
    apply_paste(&mut pending, &mut block, "\u{e9}tude", true);
    // ESC [ 200 ~ (6) + the first byte of U+00E9 (2 bytes).
    pending.drain(..7);
    advance_paste_block(&mut block, 7);
    cancel_paste(&mut pending, &mut block);
    assert_eq!(pending, [&[0xA9u8][..], b"\x1b[201~"].concat());

    let mut pending: Vec<u8> = Vec::new();
    let mut block: Option<PasteBlock> = None;
    apply_paste(&mut pending, &mut block, "abc", true);
    pending.drain(..3);
    advance_paste_block(&mut block, 3);
    cancel_paste(&mut pending, &mut block);
    assert_eq!(pending, b"00~\x1b[201~".to_vec());
}

/// An unbracketed paste cut mid-character keeps the character's rest.
#[test]
fn cancel_unbracketed_keeps_the_cut_character() {
    let mut pending: Vec<u8> = Vec::new();
    let mut block: Option<PasteBlock> = None;
    apply_paste(&mut pending, &mut block, "a\u{20ac}b", false);
    pending.drain(..2);
    advance_paste_block(&mut block, 2);
    let dropped = cancel_paste(&mut pending, &mut block);
    assert_eq!(dropped, 1);
    assert_eq!(pending, vec![0x82, 0xAC]);
}

/// `apply_paste` with `bracketed=false`: the bytes are appended to the
/// pending input and the block spans exactly them.
#[test]
fn apply_paste_queues_and_tracks_block() {
    let mut pending: Vec<u8> = Vec::new();
    let mut block: Option<PasteBlock> = None;
    let applied = apply_paste(&mut pending, &mut block, "hello", false);
    assert_eq!(applied, PasteApply::Queued(5));
    assert_eq!(pending, b"hello".to_vec());
    assert_eq!(span(&block), Some((0, 5)));
}

/// `apply_paste` with `bracketed=true`: the bracket markers are part of the
/// queued bytes and the block spans them.
#[test]
fn apply_paste_bracketed_includes_markers() {
    let mut pending: Vec<u8> = Vec::new();
    let mut block: Option<PasteBlock> = None;
    let applied = apply_paste(&mut pending, &mut block, "hello", true);
    let expected = [
        b"\x1b[200~".as_ref(),
        b"hello".as_ref(),
        b"\x1b[201~".as_ref(),
    ]
    .concat();
    assert_eq!(applied, PasteApply::Queued(expected.len()));
    assert_eq!(pending, expected);
    assert_eq!(span(&block), Some((0, expected.len())));
}

/// `apply_paste` appends after existing keystrokes; the block starts after
/// them.
#[test]
fn apply_paste_appends_after_keystrokes() {
    let mut pending: Vec<u8> = b"AB".to_vec();
    let mut block: Option<PasteBlock> = None;
    let applied = apply_paste(&mut pending, &mut block, "hello", false);
    assert_eq!(applied, PasteApply::Queued(5));
    assert_eq!(pending, b"ABhello".to_vec());
    assert_eq!(span(&block), Some((2, 7)));
}

/// An oversized paste is rejected and leaves the state unchanged.
#[test]
fn apply_paste_too_large_leaves_state_unchanged() {
    let mut pending: Vec<u8> = b"AB".to_vec();
    let mut block: Option<PasteBlock> = None;
    let big = "x".repeat(MAX_PASTE_SIZE + 1);
    let applied = apply_paste(&mut pending, &mut block, &big, false);
    assert_eq!(
        applied,
        PasteApply::TooLarge {
            size: MAX_PASTE_SIZE + 1,
            max: MAX_PASTE_SIZE,
        }
    );
    assert_eq!(pending, b"AB".to_vec(), "rejected paste must not enqueue");
    assert_eq!(block, None, "rejected paste must not set a block");
}

/// A new paste replaces the previous block: only the most recent paste is
/// tracked.
#[test]
fn apply_paste_replaces_previous_block() {
    let mut pending: Vec<u8> = Vec::new();
    let mut block: Option<PasteBlock> = None;
    apply_paste(&mut pending, &mut block, "first", false);
    assert_eq!(span(&block), Some((0, 5)));
    apply_paste(&mut pending, &mut block, "second", false);
    assert_eq!(pending, b"firstsecond".to_vec());
    assert_eq!(
        span(&block),
        Some((5, 11)),
        "the new block spans only the new paste"
    );
}

/// `cancel_paste` removes the block's bytes and leaves keystrokes on either
/// side untouched.
#[test]
fn cancel_paste_drops_only_the_paste() {
    let mut pending: Vec<u8> = b"AB".to_vec();
    let mut block: Option<PasteBlock> = None;
    apply_paste(&mut pending, &mut block, "hello", false);
    pending.extend_from_slice(b"CD");
    assert_eq!(pending, b"ABhelloCD".to_vec());
    assert_eq!(span(&block), Some((2, 7)));

    let dropped = cancel_paste(&mut pending, &mut block);
    assert_eq!(dropped, 5);
    assert_eq!(pending, b"ABCD".to_vec(), "keystrokes survive the cancel");
    assert_eq!(block, None);
}

/// `cancel_paste` with no in-flight paste drops nothing and leaves the
/// pending input untouched.
#[test]
fn cancel_paste_without_block_is_a_noop() {
    let mut pending: Vec<u8> = b"AB".to_vec();
    let mut block: Option<PasteBlock> = None;
    let dropped = cancel_paste(&mut pending, &mut block);
    assert_eq!(dropped, 0);
    assert_eq!(pending, b"AB".to_vec());
    assert_eq!(block, None);
}

/// `advance_paste_block` shifts the block left as bytes drain from the
/// front.
#[test]
fn advance_paste_block_shifts_left() {
    let mut block = Some(unfed(2, 7));
    advance_paste_block(&mut block, 2);
    assert_eq!(span(&block), Some((0, 5)));
    advance_paste_block(&mut block, 3);
    assert_eq!(span(&block), Some((0, 2)));
}

/// `advance_paste_block` clears the block once every byte up to and
/// including its end has drained from the front.
#[test]
fn advance_paste_block_clears_when_drained() {
    let mut block = Some(unfed(2, 7));
    // Draining 5 bytes leaves the block's last two bytes (indices 5, 6).
    advance_paste_block(&mut block, 5);
    assert_eq!(
        span(&block),
        Some((0, 2)),
        "partially drained block shifts left"
    );
    // Draining the remaining 7 total bytes clears it.
    advance_paste_block(&mut block, 2);
    assert_eq!(block, None, "fully drained block is cleared");
}

/// `advance_paste_block` with no block is a no-op.
#[test]
fn advance_paste_block_without_block_is_a_noop() {
    let mut block: Option<PasteBlock> = None;
    advance_paste_block(&mut block, 3);
    assert_eq!(block, None);
}

// --- end-to-end over a real PTY ------------------------------------------

/// The helper bin path, auto-discovered by cargo for integration tests.
fn helper() -> String {
    std::env::var("CARGO_BIN_EXE_nora_child")
        .expect("CARGO_BIN_EXE_nora_child must be set by cargo for integration tests")
}

/// Encode `s` as the hex string the `check` helper expects.
fn hex(s: &[u8]) -> String {
    s.iter().map(|b| format!("{b:02x}")).collect()
}

/// One poll tick inside the deadline-bounded pumps.
const POLL: Duration = Duration::from_millis(10);
/// Hard deadline for the wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// Spawn the helper in `checkhold` mode: it prints `READY`, then reads
/// stdin until the contiguous `expect` bytes are seen (prints
/// `CHECK:<marker>`) or stdin reaches EOF (prints `CHECKFAIL:<marker>`),
/// then keeps running until EOF so the verdict is observable in a
/// non-terminated tick's snapshot (a terminated tick probes nothing).
fn spawn_checkhold(marker: &str, expect: &[u8]) -> PtySession {
    PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec!["checkhold".to_string(), marker.to_string(), hex(expect)],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        WriterConfig {
            max_bytes: 256,
            reserved_bytes: 64,
        },
    )
    .expect("helper spawns over a real PTY")
}

/// A taken snapshot's grid as one row-major string, so any visible line is
/// a contiguous substring (mirrors `snapshot_text` in `tests/pump_session.rs`).
fn snapshot_text(snap: &Snapshot) -> String {
    snap.cells.iter().map(|c| c.ch).collect()
}

/// N17b end-to-end: a validated paste, interleaved with keystrokes, is
/// streamed to the PTY in order through the bounded writer. The child sees
/// the exact interleaved byte sequence.
#[test]
fn paste_streams_to_pty_in_order() {
    let mut session = spawn_checkhold("PASTE", b"ABhelloCD");

    // Wait for the helper's startup write: it prints `READY` before it
    // starts reading stdin, so the child is then actively awaiting input.
    let deadline = Instant::now() + DEADLINE;
    let mut ready = false;
    while !ready {
        let outcome = pump_tick(&mut session);
        assert!(!outcome.terminated, "checkhold helper stays alive");
        ready = outcome
            .snapshot
            .as_ref()
            .is_some_and(|snap| snapshot_text(snap).contains("READY"));
        if Instant::now() > deadline {
            panic!("deadline exceeded before the helper printed READY");
        }
        thread::sleep(POLL);
    }

    // Build the pending input exactly as the pump thread would: keystrokes,
    // then the validated paste, then more keystrokes.
    let mut pending: Vec<u8> = Vec::new();
    let mut block: Option<PasteBlock> = None;
    pending.extend_from_slice(b"AB");
    apply_paste(&mut pending, &mut block, "hello", false);
    pending.extend_from_slice(b"CD");
    assert_eq!(pending, b"ABhelloCD".to_vec());
    assert_eq!(span(&block), Some((2, 7)));

    // Feed through the bounded writer, advancing the block as bytes drain.
    let fed = feed_pending(&mut session, &mut pending, &mut block);
    assert_eq!(fed, 9, "all nine bytes fit the 256-byte cap and feed now");
    assert_eq!(pending, Vec::<u8>::new(), "the pending input is drained");
    assert_eq!(block, None, "the fully drained block is cleared");

    // Deliver the enqueued bytes to the child (mirrors `pump_thread`'s
    // per-tick flush).
    for _ in 0..8 {
        match session.flush(1024 * 1024) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }

    // Pump until the child confirms it saw the contiguous sequence. The
    // `checkhold` helper stays alive after the verdict, so the confirmation
    // is observable in a non-terminated tick's snapshot.
    let deadline = Instant::now() + DEADLINE;
    let mut confirmed = false;
    while !confirmed {
        let outcome = pump_tick(&mut session);
        assert!(!outcome.terminated, "checkhold helper stays alive");
        if let Some(snap) = &outcome.snapshot {
            let text = snapshot_text(snap);
            if text.contains("CHECK:PASTE") {
                confirmed = true;
            } else if text.contains("CHECKFAIL:PASTE") {
                panic!("child did not see the paste bytes in order");
            }
        }
        if Instant::now() > deadline {
            panic!("deadline exceeded before the child confirmed the paste");
        }
        thread::sleep(POLL);
    }

    session.close();
}

/// Keys typed behind a paste that filled the pane's queue (the child
/// stopped reading) are still accepted, within a bounded headroom.
#[test]
fn keys_are_accepted_behind_a_full_paste_queue() {
    use nora::session::pump::{INPUT_HEADROOM, MAX_PENDING_INPUT, accept_input};
    let mut pending = vec![b'x'; MAX_PENDING_INPUT];
    assert!(accept_input(&mut pending, b"\x03"), "Ctrl+C is accepted");
    assert_eq!(pending.last(), Some(&0x03));
    let room = MAX_PENDING_INPUT + INPUT_HEADROOM - pending.len();
    assert!(accept_input(&mut pending, &vec![b'k'; room]));
    assert!(!accept_input(&mut pending, b"k"), "the headroom is bounded");
    assert_eq!(pending.len(), MAX_PENDING_INPUT + INPUT_HEADROOM);
}
