//! N14b selection + copy regression suite for the headless terminal
//! engine (`nora::engine`) — `select`/`clear_selection`/`selection`/
//! `copy_selection` and the snapshot's per-cell `selected` field.
//!
//! Headless: no PTY, no threads, no GPU, no window. Each test feeds raw
//! bytes into a fresh [`Engine`] and asserts the card's acceptance lines
//! on `selection`, `copy_selection`, and `snapshot.selected`.
//!
//! Selection semantics pinned here (matching the codebase's inclusive
//! range convention — cf. `LineDamage`'s inclusive `left`/`right`): a
//! selection from `start` to `end` is a row-major linear range that
//! includes both endpoint cells, i.e. `cells[start_idx..=end_idx]` where
//! `idx = row * cols + col`.

use alacritty_terminal::grid::Scroll;

use nora::engine::Engine;

/// N14b (1): `select` records the start/end pair, `selection` returns it,
/// and `clear_selection` removes it.
#[test]
fn select_and_clear() {
    let engine = Engine::new(24, 80);

    engine.select((0, 0), (2, 5));
    let sel = engine.selection().expect("select records a selection");
    assert_eq!(sel.start, (0, 0), "selection start");
    assert_eq!(sel.end, (2, 5), "selection end");

    engine.clear_selection();
    assert!(
        engine.selection().is_none(),
        "clear_selection removes the selection"
    );
}

/// N14b (2): `copy_selection` returns the selected text.
#[test]
fn copy_selection_returns_text() {
    let mut engine = Engine::new(24, 80);
    engine.feed(b"hello world\nfoo bar\n");

    // "hello" sits at row 0, cols 0-4.
    engine.select((0, 0), (0, 5));
    let text = engine
        .copy_selection()
        .expect("a selection copies its text");
    assert!(
        text.contains("hello"),
        "the copied text contains the selected word; got {text:?}"
    );
}

/// N14b (3): with no selection, `copy_selection` returns `None`.
#[test]
fn copy_selection_none_when_empty() {
    let mut engine = Engine::new(24, 80);
    engine.feed(b"hello world\n");
    assert!(
        engine.copy_selection().is_none(),
        "no selection: nothing to copy"
    );
}

/// N14b (4): the snapshot carries a per-cell `selected` field, one entry
/// per cell (`selected.len() == cells.len()`), with exactly the selected
/// row-major range marked and everything else unmarked.
#[test]
fn snapshot_selected_field() {
    let mut engine = Engine::new(24, 80);
    engine.feed(b"hello world\nfoo bar\n");

    // Select (0,2)..=(1,3): row 0 cols 2-79 and row 1 cols 0-3.
    engine.select((0, 2), (1, 3));
    let snap = engine.snapshot();

    assert_eq!(
        snap.selected.len(),
        snap.cells.len(),
        "one selection flag per cell"
    );

    let start = 2; // (0,2)
    let end = 83; // (1,3)
    for i in 0..snap.cells.len() {
        let expected = i >= start && i <= end;
        assert_eq!(
            snap.selected[i],
            expected,
            "cell {i} (row {}, col {})",
            i / 80,
            i % 80
        );
    }
}

/// N14b (5): a selection spanning a row boundary copies the contiguous
/// row-major text — no missing or duplicated characters.
#[test]
fn selection_wraps_at_row_boundary() {
    let mut engine = Engine::new(24, 80);
    // 100 'a's wrap: row 0 = cols 0-79, row 1 = cols 0-19.
    let mut buf = vec![b'a'; 100];
    buf.push(b'\n');
    engine.feed(&buf);

    // Select across the row 0 -> row 1 boundary.
    engine.select((0, 70), (1, 10));
    let text = engine
        .copy_selection()
        .expect("a selection copies its text");
    // Rows are joined with `\n` (terminal copy convention); the body is
    // the selected cells only.
    let body: String = text.chars().filter(|c| *c != '\n').collect();
    assert!(
        body.chars().all(|c| c == 'a'),
        "the selection is all 'a'; got {text:?}"
    );
    assert!(
        (20..=21).contains(&body.len()),
        "row 0 cols 70-79 (10) + row 1 cols 0-10 (11) = 21 (inclusive) or 20 (exclusive); got {}",
        body.len()
    );
    assert_eq!(
        text.matches('\n').count(),
        1,
        "exactly one row separator between the two rows; got {text:?}"
    );
}

/// N14b (6): a selection including a wide (CJK) character copies the
/// character once — the wide-char spacer is not duplicated.
#[test]
fn selection_with_wide_chars() {
    let mut engine = Engine::new(24, 80);
    // 'a' (col 0), 'b' (col 1), '中' wide (cols 2-3).
    engine.feed("ab中\n".as_bytes());

    engine.select((0, 0), (0, 3));
    let text = engine
        .copy_selection()
        .expect("a selection copies its text");
    assert_eq!(
        text.trim(),
        "ab中",
        "the wide char is copied once, without its spacer"
    );
}

/// N14b (7): after scrolling up, a selection in the visible viewport
/// copies the visible (scrolled) content, not the live screen below.
#[test]
fn selection_after_scroll() {
    let mut engine = Engine::with_history(24, 80, 100);
    // Feed 40 distinct lines; the last ~23 fill the live screen and the
    // rest scroll into history (line i is "L{i:02}"). CRLF per line: a bare
    // LF moves the cursor down but keeps the column, so clean rows need
    // the carriage return.
    for i in 0..40u32 {
        let line = format!("L{i:02}\r\n");
        engine.feed(line.as_bytes());
    }

    // Scroll up 10 lines: the top visible row is now a history line
    // (L0x), well above the live screen's top (L1x).
    engine.scroll(Scroll::Delta(10));
    assert!(
        engine.display_offset() > 0,
        "the viewport scrolled into history"
    );

    // Select the top visible row and read its line number.
    engine.select((0, 0), (0, 3));
    let text = engine
        .copy_selection()
        .expect("a visible-viewport selection copies");
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    let line_no: u32 = digits
        .parse()
        .expect("the top visible row is a fed line (Lxx)");
    assert!(
        line_no < 15,
        "the scrolled-up viewport shows history (around L07), not the live screen (L1x); got line {line_no}"
    );
    assert!(
        line_no >= 5,
        "scrolled up by 10: the top visible row is around L07; got line {line_no}"
    );
}
