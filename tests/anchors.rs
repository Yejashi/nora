//! Stable line anchors (prompt markers) across vertical resizes.
//!
//! A marker records the cursor line and the grid's `total_scrolled` /
//! `total_pulled` counters; its current line is
//! `line - Δscrolled + Δpulled`. These tests check that the anchor stays
//! on the row whose content it marked when the viewport grows taller,
//! with and without history to pull, on both screens.

use nora::engine::Engine;

const OSC_133_A: &[u8] = b"\x1b]133;A\x07";

/// The current line of every queued marker, computed from the grid's
/// counters now.
fn marker_lines(engine: &mut Engine) -> Vec<i32> {
    let (scrolled, pulled) = {
        let term = engine.term_handle();
        let term = term.lock();
        (term.grid().total_scrolled(), term.grid().total_pulled())
    };
    engine
        .drain_markers()
        .iter()
        .map(|m| m.current_line(scrolled, pulled).expect("in range"))
        .collect()
}

/// The row whose text starts with `prefix`, as a grid line.
fn row_of(engine: &mut Engine, prefix: &str) -> Option<i32> {
    let snapshot = engine.snapshot();
    (0..snapshot.rows)
        .find(|&row| {
            let text: String = (0..snapshot.cols)
                .map(|col| snapshot.cells[row * snapshot.cols + col].ch)
                .collect();
            text.starts_with(prefix)
        })
        .map(|row| row as i32)
}

/// No history: growing 24 -> 50 rows adds blank rows at the bottom and
/// moves no content, so a marker at line 0 stays at line 0.
#[test]
fn grow_without_history_keeps_marker_line() {
    let mut engine = Engine::with_history(24, 80, 0);
    engine.feed(OSC_133_A);
    engine.feed(b"$ prompt");
    engine.resize(50, 80);
    assert_eq!(marker_lines(&mut engine), vec![0]);
    assert_eq!(row_of(&mut engine, "$ prompt"), Some(0));
}

/// Some history: 30 lines on a 24-row screen leave 7 in history. Growing
/// to 50 pulls those 7 back (content moves down 7) and adds 19 blank rows
/// below; the marker follows its row.
#[test]
fn grow_with_partial_history_follows_content() {
    let mut engine = Engine::with_history(24, 80, 100);
    for i in 0..30 {
        engine.feed(format!("line {i}\r\n").as_bytes());
    }
    engine.feed(OSC_133_A);
    engine.feed(b"$ prompt");
    let before = row_of(&mut engine, "$ prompt").expect("prompt row");
    assert_eq!(before, 23);
    engine.resize(50, 80);
    let after = row_of(&mut engine, "$ prompt").expect("prompt row");
    assert_eq!(after, 30);
    assert_eq!(marker_lines(&mut engine), vec![after]);
}

/// The alternate screen has no history: growing it moves nothing, so a
/// marker placed there stays on its row.
#[test]
fn grow_on_alt_screen_keeps_marker_line() {
    let mut engine = Engine::with_history(24, 80, 100);
    engine.feed(b"\x1b[?1049h\x1b[6;1H");
    engine.feed(OSC_133_A);
    engine.feed(b"$ alt");
    engine.resize(50, 80);
    assert_eq!(row_of(&mut engine, "$ alt"), Some(5));
    assert_eq!(marker_lines(&mut engine), vec![5]);
}

/// Repeated grow/shrink cycles never let the anchor drift.
#[test]
fn grow_shrink_cycles_do_not_drift() {
    let mut engine = Engine::with_history(24, 80, 0);
    engine.feed(OSC_133_A);
    engine.feed(b"$ prompt");
    for _ in 0..3 {
        engine.resize(50, 80);
        engine.resize(24, 80);
    }
    assert_eq!(row_of(&mut engine, "$ prompt"), Some(0));
    assert_eq!(marker_lines(&mut engine), vec![0]);
}
