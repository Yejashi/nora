//! Changes the pinned `Term` reports with the same damage shape as "only
//! the cursor is there" (`is_noop_damage`) must still produce a frame:
//! `Engine::cursor_changed` reports them, and an idle engine reports
//! nothing after a snapshot (no redraw loop).
//!
//! Headless: no PTY, no threads, no sleeps.

use nora::engine::Engine;
use nora::render::term::is_noop_damage;

/// Whether the pump would build a frame now (the rule in `pump_tick`).
fn frame_due(engine: &Engine) -> bool {
    let (damage, cursor) = engine.probe();
    !is_noop_damage(&damage, cursor) || engine.cursor_changed()
}

/// An engine fed `setup` and then snapshotted, so nothing is due.
fn settled(setup: &[u8]) -> Engine {
    let mut engine = Engine::new(4, 10);
    engine.feed(setup);
    let _ = engine.snapshot();
    assert!(!frame_due(&engine), "nothing is due right after a snapshot");
    engine
}

/// Feed `bytes` and check that a frame is due although the damage alone
/// is the no-op shape, and that the snapshot settles it.
fn assert_frame_due(setup: &[u8], bytes: &[u8], what: &str) {
    let mut engine = settled(setup);
    engine.feed(bytes);
    let (damage, cursor) = engine.probe();
    assert!(
        is_noop_damage(&damage, cursor),
        "{what}: the damage alone looks like nothing ({damage:?}); \
         if this fails the vendored term now reports it and the case is moot"
    );
    assert!(frame_due(&engine), "{what}: a frame must be due");
    let _ = engine.snapshot();
    assert!(!frame_due(&engine), "{what}: the snapshot settles it");
}

#[test]
fn idle_engine_is_not_due_after_a_snapshot() {
    let engine = settled(b"hello");
    assert!(!frame_due(&engine));
    assert!(!frame_due(&engine), "probing twice changes nothing");
}

#[test]
fn character_in_the_last_column_is_due() {
    // Nine characters put the cursor in the last column; the tenth is
    // written there and the cursor stays (wrap pending).
    assert_frame_due(b"abcdefghi", b"j", "last column");
}

#[test]
fn combining_mark_is_due() {
    // The mark attaches to the cell before the cursor; the cursor stays.
    assert_frame_due(b"e", "\u{301}".as_bytes(), "combining mark");
}

#[test]
fn erase_at_the_cursor_cell_is_due() {
    // EL0 in the last column, EL1 at column 0. (ECH in the last column
    // damages one column past it, so its damage is already real.)
    assert_frame_due(b"abcdefghij\x1b[10G", b"\x1b[K", "EL0 last column");
    assert_frame_due(b"x\r", b"\x1b[1K", "EL1 column 0");
}

#[test]
fn cursor_visibility_and_shape_are_due() {
    assert_frame_due(b"", b"\x1b[?25l", "DECTCEM hide");
    assert_frame_due(b"\x1b[?25l", b"\x1b[?25h", "DECTCEM show");
    assert_frame_due(b"", b"\x1b[6 q", "DECSCUSR bar");
}
