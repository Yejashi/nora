//! N14a scrollback regression suite for the headless terminal engine
//! (`nora::engine`) — scrollback history, viewport scrolling, and the
//! snapshot's `display_offset` mapping of history rows into the viewport.
//!
//! Headless: no PTY, no threads, no GPU, no window. Each test feeds raw
//! bytes into a fresh [`Engine::with_history`] and asserts the card's
//! acceptance lines on `history_size`, `display_offset`, `cursor`,
//! `snapshot.cells`/`display_offset`, and `probe` damage.

use alacritty_terminal::grid::Scroll;

use nora::engine::{Damage, Engine};

/// Feed `count` lines of `line\n` to the engine in one chunk. Each
/// newline past the viewport bottom pushes one line into scrollback
/// history (up to the engine's scrollback cap).
fn feed_lines(engine: &mut Engine, count: usize) {
    let mut buf = Vec::with_capacity(count * 5);
    for _ in 0..count {
        buf.extend_from_slice(b"line\n");
    }
    engine.feed(&buf);
}

/// N14a (1): a fresh engine with a 1000-line scrollback reports zero
/// history (no lines scrolled yet) and a bottom-aligned viewport
/// (`display_offset == 0`); after feeding enough lines to overflow the
/// 24-row viewport, `history_size` becomes positive and stays within the
/// 1000-line cap.
#[test]
fn history_size_positive() {
    let mut engine = Engine::with_history(24, 80, 1000);
    // A fresh engine has no history yet: grid lines equal viewport rows,
    // so `history_size` is 0 and the viewport is bottom-aligned.
    assert_eq!(engine.history_size(), 0, "fresh engine has no history");
    assert_eq!(
        engine.display_offset(),
        0,
        "fresh viewport is bottom-aligned"
    );

    // Feed enough lines to overflow the 24-row viewport into history.
    feed_lines(&mut engine, 200);
    assert!(
        engine.history_size() > 0,
        "fed lines must create history; got {}",
        engine.history_size()
    );
    assert!(
        engine.history_size() <= 1000,
        "history must not exceed the 1000-line scrollback cap; got {}",
        engine.history_size()
    );
}

/// N14a (2): scrolling up by more than the available history clamps the
/// viewport to the top of the history — `display_offset` never exceeds
/// `history_size` (100 here) and is positive once any history exists.
#[test]
fn scroll_display_clamps_to_history() {
    let mut engine = Engine::with_history(24, 80, 100);
    feed_lines(&mut engine, 200); // history_size = 100 (capped)
    assert_eq!(
        engine.history_size(),
        100,
        "history is capped at the scrollback"
    );

    // Scroll up by 500: clamped to the 100 lines of history.
    engine.scroll(Scroll::Delta(500));
    assert!(
        engine.display_offset() <= 100,
        "display_offset must not exceed the history (100); got {}",
        engine.display_offset()
    );
    assert!(
        engine.display_offset() > 0,
        "scrolling up must move the viewport into history; got {}",
        engine.display_offset()
    );
}

/// N14a (3): after scrolling up into history, a snapshot contains exactly
/// one viewport of cells (`rows * cols`), carries the viewport's
/// `display_offset`, and maps every history row into a viewport row
/// (`row < rows`).
#[test]
fn snapshot_maps_history_rows_to_viewport() {
    let mut engine = Engine::with_history(24, 80, 100);
    feed_lines(&mut engine, 200); // history_size = 100 (capped)

    // Scroll up by 10 lines into history.
    engine.scroll(Scroll::Delta(10));
    assert_eq!(
        engine.display_offset(),
        10,
        "viewport scrolled 10 lines into history"
    );

    let snap = engine.snapshot();
    assert_eq!(
        snap.cells.len(),
        24 * 80,
        "snapshot must contain exactly one viewport of cells"
    );
    assert_eq!(
        snap.display_offset, 10,
        "snapshot must carry the viewport's history offset"
    );
    assert!(
        snap.cells.iter().all(|cell| cell.row < 24),
        "all snapshot cells must be within the 24-row viewport"
    );
}

/// N14a (4): when scrolled up, the cursor sits at the bottom of the live
/// screen — below the visible viewport — so it must not be reported.
#[test]
fn cursor_none_when_scrolled_up() {
    let mut engine = Engine::with_history(24, 80, 100);
    feed_lines(&mut engine, 200); // cursor lands at the bottom of the live screen

    // Scroll up by 10 lines: the cursor (at the bottom) is now below the
    // visible viewport and must not be reported.
    engine.scroll(Scroll::Delta(10));
    assert_eq!(
        engine.cursor(),
        None,
        "the cursor is below the visible viewport when scrolled up"
    );
}

/// N14a (5): a viewport scroll changes the display offset, which the
/// pinned term marks fully damaged — so a `probe` after scrolling reports
/// [`Damage::Full`] (the prior snapshot consumed the earlier damage).
#[test]
fn scroll_produces_full_damage() {
    let mut engine = Engine::with_history(24, 80, 100);
    feed_lines(&mut engine, 200);

    // Consume the pending damage (a fresh engine starts fully damaged).
    let snap = engine.snapshot();
    assert_eq!(
        snap.damage,
        Damage::Full,
        "the first snapshot is fully damaged"
    );

    // Scrolling changes the display offset, which the pinned term marks
    // fully damaged (alacritty_terminal-0.26.0 term/mod.rs:404-407).
    engine.scroll(Scroll::Delta(1));

    let (damage, _cursor) = engine.probe();
    assert_eq!(
        damage,
        Damage::Full,
        "a viewport scroll must report full damage"
    );
}
