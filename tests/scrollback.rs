//! N14a scrollback regression suite for the headless terminal engine
//! (`nora::engine`) — scrollback history, viewport scrolling, and the
//! snapshot's `display_offset` mapping of history rows into the viewport.
//!
//! Headless: no PTY, no threads, no GPU, no window. Each test feeds raw
//! bytes into a fresh [`Engine::with_history`] and asserts the card's
//! acceptance lines on `history_size`, `display_offset`, `cursor`,
//! `snapshot.cells`/`display_offset`, and `probe` damage.

use alacritty_terminal::grid::Scroll;

use nora::engine::{Damage, Engine, opener_argv};
use nora::render::term::is_noop_damage;

/// The platform opener `opener_argv` uses.
const OPENER: &str = if cfg!(target_os = "macos") {
    "open"
} else if cfg!(windows) {
    "explorer"
} else {
    "xdg-open"
};

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

/// Audit RV1: partly scrolled back with the cursor still on screen (after
/// `clear`, wheel up 3 lines), an idle probe is a no-op. The damage
/// iterator reports the always-damaged cursor cell in viewport rows, so
/// the probe's cursor must be in viewport rows too; otherwise every pump
/// wake built a snapshot and a frame (a redraw loop).
#[test]
fn idle_probe_is_noop_when_partly_scrolled() {
    let mut engine = Engine::with_history(24, 80, 100);
    feed_lines(&mut engine, 200);
    // `clear`: home the cursor and erase the screen.
    engine.feed(b"\x1b[H\x1b[2J");
    engine.scroll(Scroll::Delta(3));
    // Consume the scroll's full damage.
    let _ = engine.snapshot();

    for round in 0..3 {
        let (damage, cursor) = engine.probe();
        assert_eq!(cursor, Some((3, 0)), "viewport-mapped cursor");
        assert!(
            is_noop_damage(&damage, cursor),
            "idle probe {round} counted as damage: {damage:?} cursor {cursor:?}"
        );
    }
}

/// Fully scrolled past the cursor, the probe reports no cursor and an
/// empty (no-op) damage list.
#[test]
fn idle_probe_is_noop_when_cursor_scrolled_off() {
    let mut engine = Engine::with_history(24, 80, 100);
    feed_lines(&mut engine, 200);
    engine.scroll(Scroll::Delta(5));
    let _ = engine.snapshot();
    let (damage, cursor) = engine.probe();
    assert_eq!(cursor, None);
    assert!(is_noop_damage(&damage, cursor), "{damage:?}");
}

/// N35b: with no prompt markers stored, jumping to the previous prompt
/// is a clean no-op (returns `None` and does not scroll).
#[test]
fn jump_to_previous_prompt_empty() {
    let mut engine = Engine::with_history(24, 80, 1000);
    assert_eq!(engine.marker_count(), 0);
    assert!(
        engine.jump_to_previous_prompt().is_none(),
        "no markers: no jump"
    );
    assert_eq!(
        engine.display_offset(),
        0,
        "no scroll when there is no marker"
    );
}

/// N35b: jumping to the previous prompt scrolls the viewport to the most
/// recent `A` marker, even after the terminal has scrolled past it.
///
/// The 24-row screen fills from the top, so the first 23 newlines do not
/// scroll. Feed 30 lines (7 scroll off the bottom), then an `A` marker,
/// then 10 more lines (10 scroll). The marker is recorded at the bottom
/// (`line = 0`) after the first 30 lines, so after the final 10 lines
/// scroll it is exactly 10 lines above the bottom — within the 17-line
/// history — and the jump must land the viewport at `display_offset == 10`.
#[test]
fn jump_to_previous_prompt_across_scroll() {
    let mut engine = Engine::with_history(24, 80, 1000);

    // 30 lines, an `A` marker, then 10 more lines, in one chunk. The
    // marker is detected between the two line runs, so it is stored at
    // the bottom row (`line = screen_lines - 1 = 23`) with the grid's
    // `total_scrolled` at that point (8: the first run pushed 8 lines
    // into history).
    let mut buf = Vec::new();
    for _ in 0..30 {
        buf.extend_from_slice(b"line\n");
    }
    buf.extend_from_slice(&[
        0x1B, 0x5D, b'1', b'3', b'3', b' ', b';', b' ', b'A', 0x1B, 0x5C,
    ]);
    for _ in 0..10 {
        buf.extend_from_slice(b"line\n");
    }
    engine.feed(&buf);

    // Drain the marker queue (inserts the marker into the store).
    let markers = engine.drain_markers();
    assert_eq!(markers.len(), 1, "one marker detected");
    assert_eq!(engine.marker_count(), 1, "one marker stored");

    // The marker is now 10 lines above the bottom (10 lines scrolled since
    // it was stored). Jump to it.
    let id = engine.jump_to_previous_prompt();
    assert!(id.is_some(), "jump should find the stored marker");
    assert_eq!(
        engine.display_offset(),
        10,
        "viewport must scroll to the marker's line (10 above the bottom)"
    );
}

/// A vertical resize that pulls lines from history into the visible area
/// must not invalidate a stored marker's line reference (N35b). The marker
/// stays 10 lines above the bottom after the resize, so the jump lands at
/// the same offset.
#[test]
fn jump_to_previous_prompt_across_resize() {
    let mut engine = Engine::with_history(24, 80, 1000);

    // 30 lines (8 into history), an `A` marker, then 10 more lines (10
    // into history), in one chunk.
    let mut buf = Vec::new();
    for _ in 0..30 {
        buf.extend_from_slice(b"line\n");
    }
    buf.extend_from_slice(&[
        0x1B, 0x5D, b'1', b'3', b'3', b' ', b';', b' ', b'A', 0x1B, 0x5C,
    ]);
    for _ in 0..10 {
        buf.extend_from_slice(b"line\n");
    }
    engine.feed(&buf);
    let markers = engine.drain_markers();
    assert_eq!(markers.len(), 1, "one marker detected");

    // Grow the viewport by 5 rows; 5 lines are pulled from history into
    // the visible area, shifting every stored line down by 5.
    engine.resize(29, 80);

    // The marker is still 10 lines above the bottom. Jump to it.
    let id = engine.jump_to_previous_prompt();
    assert!(id.is_some(), "jump should find the stored marker");
    assert_eq!(
        engine.display_offset(),
        10,
        "resize must not invalidate the marker's line reference"
    );
}

/// N35c: with no markers stored, selecting the previous command is a clean
/// no-op (returns `None` and leaves no selection).
#[test]
fn select_previous_command_empty() {
    let engine = Engine::with_history(24, 80, 1000);
    assert!(
        engine.select_previous_command().is_none(),
        "no markers: no selection"
    );
    assert!(engine.selection().is_none(), "no selection created");
}

/// N35c: a completed command (a `C` marker followed by its `D` marker) is
/// selected from the command's first output line to its last. The `D`
/// marker lands on the line after the last output, so the selection ends
/// at `D - 1`.
#[test]
fn select_previous_command_selects_command_range() {
    let mut engine = Engine::with_history(24, 80, 1000);

    // A prompt, a command, its two lines of output, then the command-end
    // marker, all within the live screen (no scroll).
    let mut buf = Vec::new();
    buf.extend_from_slice(&[
        0x1B, 0x5D, b'1', b'3', b'3', b' ', b';', b' ', b'A', 0x1B, 0x5C,
    ]);
    buf.extend_from_slice(b"$ ");
    buf.extend_from_slice(&[
        0x1B, 0x5D, b'1', b'3', b'3', b' ', b';', b' ', b'B', 0x1B, 0x5C,
    ]);
    buf.extend_from_slice(b"ls\n");
    buf.extend_from_slice(&[
        0x1B, 0x5D, b'1', b'3', b'3', b' ', b';', b' ', b'C', 0x1B, 0x5C,
    ]);
    buf.extend_from_slice(b"total 8\n");
    buf.extend_from_slice(b"file1\n");
    buf.extend_from_slice(&[
        0x1B, 0x5D, b'1', b'3', b'3', b' ', b';', b' ', b'D', 0x1B, 0x5C,
    ]);
    engine.feed(&buf);
    let _ = engine.drain_markers();

    // The `C` marker is on line 1 (first output line) and the `D` marker on
    // line 3 (after the two output lines). The selection must cover lines
    // 1..2 (the output), not line 3 (the next prompt).
    let sel = engine
        .select_previous_command()
        .expect("a completed command must be selectable");
    assert_eq!(
        sel.start,
        (1, 0),
        "selection starts at the command's first line"
    );
    assert_eq!(
        sel.end,
        (2, 79),
        "selection ends at the command's last output line"
    );
}

/// N35c: a command still running (its `C` marker has been emitted but not
/// yet its `D`) is selected from its `C` line to the bottom of the screen.
#[test]
fn select_previous_command_in_progress_selects_to_bottom() {
    let mut engine = Engine::with_history(24, 80, 1000);

    let mut buf = Vec::new();
    buf.extend_from_slice(&[
        0x1B, 0x5D, b'1', b'3', b'3', b' ', b';', b' ', b'A', 0x1B, 0x5C,
    ]);
    buf.extend_from_slice(b"$ ");
    buf.extend_from_slice(&[
        0x1B, 0x5D, b'1', b'3', b'3', b' ', b';', b' ', b'B', 0x1B, 0x5C,
    ]);
    buf.extend_from_slice(b"long_running\n");
    buf.extend_from_slice(&[
        0x1B, 0x5D, b'1', b'3', b'3', b' ', b';', b' ', b'C', 0x1B, 0x5C,
    ]);
    buf.extend_from_slice(b"partial output\n");
    engine.feed(&buf);
    let _ = engine.drain_markers();

    // The `C` marker is on line 1; the command is still running, so the
    // selection runs from line 1 to the bottom (line 23).
    let sel = engine
        .select_previous_command()
        .expect("a running command must be selectable");
    assert_eq!(
        sel.start,
        (1, 0),
        "selection starts at the command's first line"
    );
    assert_eq!(
        sel.end,
        (23, 79),
        "selection runs to the bottom of the screen"
    );
}

/// N35c: a command that has scrolled into the history is still selected
/// correctly — the viewport scrolls so the command's output is visible at
/// the bottom of the viewport.
#[test]
fn select_previous_command_across_scroll() {
    let mut engine = Engine::with_history(24, 80, 1000);

    // 30 lines (7 into history), then a prompt + command + 2 output lines +
    // the command-end marker, all in one chunk. Each output line scrolls the
    // bottom, so the `C` and `D` markers are stored at the bottom row with
    // increasing `total_scrolled` values (C at +8, D at +10).
    let mut buf = Vec::new();
    for _ in 0..30 {
        buf.extend_from_slice(b"line\n");
    }
    buf.extend_from_slice(&[
        0x1B, 0x5D, b'1', b'3', b'3', b' ', b';', b' ', b'A', 0x1B, 0x5C,
    ]);
    buf.extend_from_slice(b"$ ");
    buf.extend_from_slice(&[
        0x1B, 0x5D, b'1', b'3', b'3', b' ', b';', b' ', b'B', 0x1B, 0x5C,
    ]);
    buf.extend_from_slice(b"ls\n");
    buf.extend_from_slice(&[
        0x1B, 0x5D, b'1', b'3', b'3', b' ', b';', b' ', b'C', 0x1B, 0x5C,
    ]);
    buf.extend_from_slice(b"total 8\n");
    buf.extend_from_slice(b"file1\n");
    buf.extend_from_slice(&[
        0x1B, 0x5D, b'1', b'3', b'3', b' ', b';', b' ', b'D', 0x1B, 0x5C,
    ]);
    engine.feed(&buf);
    let _ = engine.drain_markers();

    // The command's output is 2 lines (C is 2 lines above D). The selection
    // covers those 2 lines, scrolled to the bottom of the viewport.
    let sel = engine
        .select_previous_command()
        .expect("a scrolled command must be selectable");
    assert_eq!(
        sel.start,
        (22, 0),
        "selection starts 2 lines above the bottom"
    );
    assert_eq!(sel.end, (23, 79), "selection ends at the bottom row");
    assert_eq!(
        engine.display_offset(),
        1,
        "viewport scrolled so the command's output is visible"
    );
}

// ---------------------------------------------------------------------------
// N35d: hyperlink activation opener resolution (pure function)
// ---------------------------------------------------------------------------

/// `opener_argv` for an `https` URI returns `[OPENER, <uri>]`.
#[test]
fn opener_argv_http() {
    assert_eq!(
        opener_argv("https://example.com"),
        Some(vec![OPENER.to_string(), "https://example.com".to_string()])
    );
}

/// `opener_argv` for a `mailto` URI returns `[OPENER, <uri>]`.
#[test]
fn opener_argv_mailto() {
    assert_eq!(
        opener_argv("mailto:user@example.com"),
        Some(vec![
            OPENER.to_string(),
            "mailto:user@example.com".to_string()
        ])
    );
}

/// `opener_argv` for a `file` URI returns `[OPENER, <uri>]`.
#[test]
fn opener_argv_file() {
    assert_eq!(
        opener_argv("file:///home/user"),
        Some(vec![OPENER.to_string(), "file:///home/user".to_string()])
    );
}

/// `opener_argv` for a disallowed scheme (`javascript`) returns `None`.
#[test]
fn opener_argv_disallowed_scheme() {
    assert_eq!(opener_argv("javascript:alert(1)"), None);
}

/// `opener_argv` for an empty URI returns `None`.
#[test]
fn opener_argv_empty() {
    assert_eq!(opener_argv(""), None);
}

/// `opener_argv` for a URI with no scheme returns `None`.
#[test]
fn opener_argv_no_scheme() {
    assert_eq!(opener_argv("example.com/path"), None);
}

/// `opener_argv` trims surrounding whitespace before resolving.
#[test]
fn opener_argv_whitespace_trimmed() {
    assert_eq!(
        opener_argv("  https://example.com  "),
        Some(vec![OPENER.to_string(), "https://example.com".to_string()])
    );
}

// ---------------------------------------------------------------------------
// N35d: engine cwd tracking (OSC 7)
// ---------------------------------------------------------------------------

/// Wrap an OSC data string in a full OSC frame: `ESC ] <data> ESC \`.
fn osc(data: &str) -> Vec<u8> {
    let mut out = vec![0x1B, 0x5D]; // ESC ]
    out.extend_from_slice(data.as_bytes());
    out.push(0x1B); // ESC
    out.push(0x5C); // \ (ST terminator)
    out
}

/// A fresh engine reports no cwd.
#[test]
fn engine_cwd_none_initial() {
    let engine = Engine::new(24, 80);
    assert_eq!(engine.cwd(), None, "fresh engine has no cwd");
}

/// Feeding `OSC 7 ; file:///home/user ST` sets the engine's cwd.
#[test]
fn engine_cwd_from_osc7() {
    let mut engine = Engine::new(24, 80);
    engine.feed(&osc("7;file:///home/user"));
    assert_eq!(
        engine.cwd(),
        Some("/home/user".to_string()),
        "OSC 7 file:// path sets the cwd"
    );
}

/// A later `OSC 7` overwrites the earlier cwd.
#[test]
fn engine_cwd_overwrites() {
    let mut engine = Engine::new(24, 80);
    engine.feed(&osc("7;file:///home/user"));
    engine.feed(&osc("7;file:///tmp"));
    assert_eq!(engine.cwd(), Some("/tmp".to_string()));
}

// ---------------------------------------------------------------------------
// N35d: engine hyperlink drain and activation (OSC 8)
// ---------------------------------------------------------------------------

/// A fresh engine has no hyperlinks.
#[test]
fn engine_hyperlink_none_initial() {
    let mut engine = Engine::new(24, 80);
    assert!(engine.drain_hyperlinks().is_empty());
}

/// Feeding `OSC 8 ; ; URI ST` records a hyperlink at the cursor's cell;
/// `activate_hyperlink` at that cell resolves the opener and consumes the
/// hyperlink.
#[test]
fn engine_hyperlink_drain_and_activate() {
    let mut engine = Engine::new(24, 80);
    // Feed a hyperlink; the cursor starts at row 0, col 0.
    engine.feed(&osc("8;;https://example.com"));
    let drained = engine.drain_hyperlinks();
    assert_eq!(drained.len(), 1, "one hyperlink drained");
    let h = &drained[0];
    assert_eq!(h.uri, "https://example.com");
    assert_eq!(h.line, 0, "hyperlink at the cursor row");
    assert_eq!(h.col, 0, "hyperlink at col 0");

    // A fresh engine: activate at the hyperlink's cell resolves the
    // opener and consumes it.
    let mut engine = Engine::new(24, 80);
    engine.feed(&osc("8;;https://example.com"));
    let opener = engine.activate_hyperlink(0, 0);
    assert_eq!(
        opener,
        Some(vec![OPENER.to_string(), "https://example.com".to_string()])
    );

    // The hyperlink was consumed by activation.
    assert!(engine.drain_hyperlinks().is_empty());
}

/// `activate_hyperlink` at a cell with no hyperlink returns `None` and
/// leaves the hyperlink unconsumed.
#[test]
fn engine_hyperlink_activate_wrong_cell() {
    let mut engine = Engine::new(24, 80);
    engine.feed(&osc("8;;https://example.com"));
    // Activate at the wrong cell (row 22, not 0).
    assert_eq!(engine.activate_hyperlink(22, 0), None);
    // The hyperlink is still present.
    assert_eq!(engine.drain_hyperlinks().len(), 1);
}

/// `activate_hyperlink` for a disallowed scheme returns `None` and still
/// consumes the hyperlink.
#[test]
fn engine_hyperlink_activate_disallowed_scheme() {
    let mut engine = Engine::new(24, 80);
    engine.feed(&osc("8;;javascript:alert(1)"));
    // The hyperlink is at row 0, col 0.
    assert_eq!(engine.activate_hyperlink(0, 0), None);
    // The hyperlink was consumed despite the disallowed scheme.
    assert!(engine.drain_hyperlinks().is_empty());
}

/// Scrollback search: matches in the history are found (negative grid
/// lines), shown by scrolling, and marked with the selection; case is
/// ignored for an all-lowercase query only.
#[test]
fn search_finds_matches_in_scrollback_and_scrolls_to_them() {
    let mut engine = nora::engine::Engine::with_history(5, 30, 200);
    let mut stream = Vec::new();
    for i in 0..60 {
        if i == 7 {
            stream.extend_from_slice(b"the NEEDLE is here\r\n");
        } else {
            stream.extend_from_slice(format!("filler line {i}\r\n").as_bytes());
        }
    }
    engine.feed(&stream);
    let hits = engine.search_text("needle");
    assert_eq!(hits.len(), 1, "case-insensitive for a lowercase query");
    let hit = hits[0];
    assert!(hit.line < 0, "the match is in the scrollback: {hit:?}");
    assert_eq!((hit.start, hit.end), (4, 9));
    assert!(
        engine.search_text("Needle").is_empty(),
        "uppercase makes it exact"
    );
    assert_eq!(engine.display_offset(), 0);
    engine.show_search_hit(hit);
    assert!(engine.display_offset() > 0, "scrolled to the match");
    let snap = engine.snapshot();
    let selected: usize = snap.selected.iter().filter(|s| **s).count();
    assert_eq!(selected, 6, "the six cells of NEEDLE are selected");
}

/// Regression: while the view is scrolled back, an idle terminal reports no
/// damage (it used to count as damage on every probe, so the pump built a
/// snapshot and the window drew a frame on every wake: a redraw loop).
#[test]
fn idle_terminal_scrolled_back_reports_no_damage() {
    let mut engine = Engine::with_history(5, 20, 100);
    let mut s = Vec::new();
    for i in 0..30 {
        s.extend_from_slice(format!("line {i}\r\n").as_bytes());
    }
    engine.feed(&s);
    engine.scroll(alacritty_terminal::grid::Scroll::PageUp);
    let _ = engine.snapshot();
    assert!(engine.display_offset() > 0);
    let (damage, cursor) = engine.probe();
    assert!(
        nora::render::term::is_noop_damage(&damage, cursor),
        "idle while scrolled back must be a no-op: {damage:?} {cursor:?}"
    );
}
