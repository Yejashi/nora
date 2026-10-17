//! Line anchors across width changes that reflow wrapped lines.
//!
//! A width change on the main screen joins or splits soft-wrapped rows,
//! which moves each line by the rows joined or split around it. The
//! vendored grid records where the reflow moved every point (`ReflowMap`,
//! `// nora:`), and `Engine::resize` moves prompt markers, hyperlinks,
//! the selection and image placements with their text. These tests check
//! the map cell by cell against the grid, then each anchor kind through
//! the engine.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use nora::engine::Engine;

/// A small deterministic generator (no test dependency needed).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % bound.max(1)
    }
}

/// A distinct character per index: narrow ones from the private use area,
/// wide ones from the CJK block.
fn unique(index: usize, wide: bool) -> char {
    let base = if wide { 0x4E00 } else { 0xE000 };
    char::from_u32(base + index as u32).expect("valid char")
}

fn term(lines: usize, cols: usize, history: usize) -> Term<VoidListener> {
    let config = Config {
        scrolling_history: history,
        ..Config::default()
    };
    Term::new(config, &TermSize::new(cols, lines), VoidListener)
}

/// Lines of random lengths (some several rows long, some empty) of
/// distinct characters, `wide_every`th one wide (0: none).
fn feed_text(
    term: &mut Term<VoidListener>,
    seed: u64,
    lines: usize,
    max_len: usize,
    wide_every: usize,
) {
    let mut rng = Lcg(seed);
    let mut parser: Processor = Processor::new();
    let mut index = 0;
    let mut text = String::new();
    for line in 0..lines {
        for _ in 0..rng.next(max_len + 1) {
            let wide = wide_every != 0 && index % wide_every == wide_every - 1;
            text.push(unique(index, wide));
            index += 1;
        }
        if line + 1 < lines {
            text.push_str("\r\n");
        }
    }
    parser.advance(term, text.as_bytes());
}

/// Every distinct character on the grid with its point.
fn cells(term: &Term<VoidListener>) -> Vec<(char, i32, usize)> {
    let grid = term.grid();
    let mut out = Vec::new();
    for line in grid.topmost_line().0..=grid.bottommost_line().0 {
        for col in 0..grid.columns() {
            let cell = &grid[Line(line)][Column(col)];
            let spacer = cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
            if u32::from(cell.c) >= 0x4E00 && !spacer {
                out.push((cell.c, line, col));
            }
        }
    }
    out
}

/// Resize, then check that the map takes every character's old point to
/// where the grid now has it (or above the topmost line when the history
/// limit dropped it). Returns the map's size.
fn resize_and_check(term: &mut Term<VoidListener>, lines: usize, cols: usize) -> usize {
    let before = cells(term);
    let shift = |term: &Term<VoidListener>| {
        term.grid().total_scrolled() as i64 - term.grid().total_pulled() as i64
    };
    let shift_before = shift(term);
    term.resize(TermSize::new(cols, lines));
    let map = term.take_reflow_map();
    // The height change moved lines by the counters; the map then takes
    // the grid lines as they were after it.
    let moved = (shift(term) - shift_before) as i32;
    let before: Vec<_> = before
        .into_iter()
        .map(|(c, l, col)| (c, l - moved, col))
        .collect();
    let after: std::collections::HashMap<char, (i32, usize)> = cells(term)
        .into_iter()
        .map(|(c, l, col)| (c, (l, col)))
        .collect();
    let topmost = term.grid().topmost_line().0;
    for (ch, line, col) in before {
        let mapped = map.as_ref().map_or((line, col), |m| m.map(line, col));
        match after.get(&ch) {
            Some(&now) => assert_eq!(mapped, now, "{ch:?} from ({line}, {col}) to {cols} cols"),
            None => assert!(
                mapped.0 < topmost,
                "{ch:?} dropped but mapped to {mapped:?}"
            ),
        }
    }
    map.map_or(0, |m| m.len())
}

/// Narrow text through a cycle of widths, with and without a height
/// change, including the history limit dropping rows as lines split.
#[test]
fn map_follows_every_cell_through_width_cycles() {
    for (seed, history) in [(1, 1000), (2, 30), (3, 0)] {
        let mut term = term(10, 20, history);
        feed_text(&mut term, seed, 40, 70, 0);
        for (lines, cols) in [
            (10, 13),
            (10, 7),
            (12, 31),
            (8, 20),
            (10, 1),
            (10, 45),
            (6, 3),
            (10, 20),
        ] {
            resize_and_check(&mut term, lines, cols);
        }
    }
}

/// Wide characters: reflow inserts and removes leading spacers.
#[test]
fn map_follows_wide_characters() {
    for seed in [4, 5, 6] {
        let mut term = term(10, 20, 1000);
        feed_text(&mut term, seed, 30, 60, 3);
        for (lines, cols) in [(10, 13), (10, 7), (11, 31), (10, 9), (10, 40), (10, 20)] {
            resize_and_check(&mut term, lines, cols);
        }
    }
}

/// Output ending mid-line keeps the cursor on wrapped text, which reflow
/// moves with its own rules.
#[test]
fn map_follows_cells_on_the_cursor_line() {
    let mut term = term(5, 10, 100);
    let mut parser: Processor = Processor::new();
    let text: String = (0..35).map(|i| unique(i, false)).collect();
    parser.advance(&mut term, text.as_bytes());
    for (lines, cols) in [(5, 6), (5, 15), (5, 4), (5, 40), (5, 10)] {
        resize_and_check(&mut term, lines, cols);
    }
}

/// Unwrapped lines only move as a block: the map stays empty however long
/// the history is, so its size follows the rows reflow changed.
#[test]
fn map_is_bounded_by_the_rows_reflow_changed() {
    let mut term = term(10, 40, 10_000);
    let mut parser: Processor = Processor::new();
    for i in 0..5000 {
        parser.advance(&mut term, format!("{}\r\n", unique(i, false)).as_bytes());
    }
    let long: String = (5000..5100).map(|i| unique(i, false)).collect();
    parser.advance(&mut term, long.as_bytes());
    // Only the long line wraps: a handful of breakpoints for its rows.
    let n = resize_and_check(&mut term, 10, 30);
    assert!(n <= 8);
    assert!(resize_and_check(&mut term, 10, 40) <= 8);
}

/// The alternate screen does not reflow: no map, nothing moves.
#[test]
fn alt_screen_records_no_map() {
    let mut term = term(5, 10, 100);
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, b"\x1b[?1049h");
    parser.advance(&mut term, "y".repeat(25).as_bytes());
    term.resize(TermSize::new(4, 5));
    // The main screen behind it reflowed nothing (it is empty).
    assert!(term.take_reflow_map().is_none());
}

const OSC_133_A: &str = "\x1b]133;A\x07";
const OSC_133_C: &str = "\x1b]133;C\x07";
const OSC_133_D: &str = "\x1b]133;D\x07";

/// The viewport text of `engine`, one string per row (trailing blanks
/// dropped).
fn rows(engine: &mut Engine) -> Vec<String> {
    let snapshot = engine.snapshot();
    (0..snapshot.rows)
        .map(|row| {
            let text: String = (0..snapshot.cols)
                .map(|col| snapshot.cells[row * snapshot.cols + col].ch)
                .collect();
            text.trim_end().to_string()
        })
        .collect()
}

/// Commands whose output wraps: three prompts, each followed by a long
/// output line, on an 80-column screen with history.
fn shell_session(engine: &mut Engine) {
    for (i, len) in [(1, 150), (2, 230), (3, 90)] {
        let out: String = std::iter::repeat_n(char::from(b'a' + i as u8), len).collect();
        let bytes = format!("{OSC_133_A}$ cmd{i}\r\n{OSC_133_C}{out}\r\nend{i}\r\n{OSC_133_D}");
        engine.feed(bytes.as_bytes());
    }
    engine.feed(format!("{OSC_133_A}$ ").as_bytes());
}

/// Prompt jumps land on the prompt rows and the previous command's
/// selection copies its output after shrinking and growing the width.
#[test]
fn prompt_markers_follow_reflow() {
    let mut engine = Engine::with_history(10, 80, 1000);
    shell_session(&mut engine);
    let expected: String = std::iter::repeat_n('d', 90).collect::<String>() + "\nend3";
    for cols in [33, 17, 120, 80] {
        engine.resize(10, cols);
        engine.drain_markers();
        // The previous command (`cmd3`) selects its output whole.
        assert!(engine.select_previous_command().is_some(), "{cols} cols");
        assert_eq!(
            engine.copy_selection().as_deref(),
            Some(expected.as_str()),
            "{cols} cols"
        );
        // Each jump puts a prompt above the view on the top row, newest
        // first, down to the first one.
        engine.scroll(alacritty_terminal::grid::Scroll::Bottom);
        let mut seen = Vec::new();
        while engine.jump_to_previous_prompt().is_some() {
            seen.push(rows(&mut engine)[0].clone());
        }
        let all = ["$ cmd3", "$ cmd2", "$ cmd1"];
        assert!(!seen.is_empty(), "{cols} cols");
        assert_eq!(seen, all[all.len() - seen.len()..], "{cols} cols");
        engine.scroll(alacritty_terminal::grid::Scroll::Bottom);
    }
}

/// Markers still queued (not yet drained) at the resize move too.
#[test]
fn queued_markers_follow_reflow() {
    let mut engine = Engine::with_history(8, 20, 100);
    // The prompt above a wrapped line: splitting it moves the prompt up.
    engine.feed(format!("{OSC_133_A}$ here\r\n{}", "z".repeat(50)).as_bytes());
    engine.resize(8, 10);
    let (scrolled, pulled) = {
        let term = engine.term_handle();
        let term = term.lock();
        (term.grid().total_scrolled(), term.grid().total_pulled())
    };
    let lines: Vec<i32> = engine
        .drain_markers()
        .iter()
        .map(|m| m.current_line(scrolled, pulled).expect("in range"))
        .collect();
    // Splitting the line below pushed the prompt into the history.
    engine.scroll(alacritty_terminal::grid::Scroll::Top);
    let row = rows(&mut engine)
        .iter()
        .position(|r| r == "$ here")
        .expect("prompt") as i32;
    let line = row - engine.display_offset() as i32;
    assert!(line < 0);
    assert_eq!(lines, vec![line]);
}

/// A hyperlink above a wrapped line is still found on its row.
#[test]
fn hyperlink_follows_reflow() {
    let mut engine = Engine::with_history(8, 20, 100);
    let link = "see \x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\";
    engine.feed(format!("{link}\r\n{}", "z".repeat(50)).as_bytes());
    engine.resize(8, 10);
    engine.drain_hyperlinks();
    engine.scroll(alacritty_terminal::grid::Scroll::Top);
    let row = rows(&mut engine)
        .iter()
        .position(|r| r == "see link")
        .expect("row");
    assert!(row < engine.display_offset(), "moved into the history");
    assert_eq!(
        engine.hyperlink_at(row, 5).as_deref(),
        Some("https://example.com")
    );
}

/// A selection on a wrapped line copies the same text across a narrow and
/// wide cycle.
#[test]
fn selection_copies_the_same_text_across_reflow() {
    let mut engine = Engine::with_history(8, 30, 100);
    let long: String = (0..75).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    engine.feed(format!("first\r\n{long}\r\nlast\r\n").as_bytes());
    // From column 5 of the long line's first row to column 10 of its
    // third row (rows 1..=3 at 30 columns).
    engine.select((1, 5), (3, 10));
    let before = engine.copy_selection().expect("selected");
    assert_eq!(before, &long[5..71]);
    for cols in [12, 7, 50, 100, 30] {
        engine.resize(8, cols);
        assert_eq!(
            engine.copy_selection().as_deref(),
            Some(before.as_str()),
            "{cols} cols"
        );
    }
}

/// The selection made on the main screen is moved while the alternate
/// screen is shown, and is back on its text afterwards.
#[test]
fn main_selection_follows_reflow_behind_the_alt_screen() {
    let mut engine = Engine::with_history(8, 30, 100);
    let long: String = (0..75).map(|i| char::from(b'A' + (i % 26) as u8)).collect();
    engine.feed(format!("first\r\n{long}\r\nlast\r\n").as_bytes());
    engine.select((2, 0), (2, 29));
    let before = engine.copy_selection().expect("selected");
    engine.feed(b"\x1b[?1049h");
    engine.resize(8, 13);
    engine.feed(b"\x1b[?1049l");
    assert_eq!(engine.copy_selection().as_deref(), Some(before.as_str()));
}

/// A search hit (and its highlight) stays on its text.
#[test]
fn search_hit_stays_on_its_text() {
    let mut engine = Engine::with_history(8, 30, 100);
    let filler: String = "x".repeat(70);
    engine.feed(format!("{filler}\r\n{filler}NEEDLE{filler}\r\ntail\r\n").as_bytes());
    let mut hit = *engine.search_text("NEEDLE").first().expect("hit");
    engine.show_search_hit(hit);
    for cols in [11, 64, 9, 140, 30] {
        if let Some(remap) = engine.resize(8, cols) {
            hit = remap.search_hit(hit);
        }
        let fresh = *engine.search_text("NEEDLE").first().expect("hit");
        assert_eq!(hit, fresh, "{cols} cols");
        // The highlight (the selection) still copies the match.
        assert_eq!(
            engine.copy_selection().as_deref(),
            Some("NEEDLE"),
            "{cols} cols"
        );
        engine.show_search_hit(hit);
    }
}

/// An image placed below wrapped lines stays at its text row.
#[test]
fn image_placement_stays_at_its_text_row() {
    let mut engine = Engine::with_history(12, 40, 100);
    engine.graphics().set_cell_size(10, 20);
    let long: String = "w".repeat(110);
    engine.feed(format!("{long}\r\nIMG:").as_bytes());
    // A 1x1 RGBA image, displayed at the cursor (after "IMG:").
    engine.feed(b"\x1b_Ga=T,f=32,s=1,v=1,c=2,r=1;AAAAAA==\x1b\\");
    engine.feed(b"\r\nbelow\r\n");
    let image_row = |engine: &mut Engine| {
        let snapshot = engine.snapshot();
        let image = snapshot.images.first().expect("image visible");
        (image.row, image.col)
    };
    let text_row = |engine: &mut Engine| rows(engine).iter().position(|r| r.starts_with("IMG:"));
    assert_eq!(
        image_row(&mut engine),
        (text_row(&mut engine).expect("row") as i32, 4)
    );
    for cols in [25, 13, 90, 40] {
        engine.resize(12, cols);
        let row = text_row(&mut engine).expect("row") as i32;
        assert_eq!(image_row(&mut engine), (row, 4), "{cols} cols");
    }
}

/// Rows pushed out of the history while reflow splits lines take their
/// anchors with them: the selection on them copies nothing, and an
/// anchor below them stays on its text.
#[test]
fn rows_dropped_by_the_history_limit_take_their_anchors() {
    let mut engine = Engine::with_history(4, 20, 2);
    engine.feed(format!("{}\r\nkeep\r\n{OSC_133_A}$ p", "q".repeat(40)).as_bytes());
    // The long line is in the history (2 rows), the prompt at the bottom.
    engine.scroll(alacritty_terminal::grid::Scroll::Top);
    engine.select((0, 0), (0, 3));
    assert_eq!(engine.copy_selection().as_deref(), Some("qqqq"));
    engine.scroll(alacritty_terminal::grid::Scroll::Bottom);
    // At 5 columns the long line needs 8 rows: the history keeps 2.
    engine.resize(4, 5);
    assert_eq!(engine.copy_selection(), None);
    engine.drain_markers();
    engine.scroll(alacritty_terminal::grid::Scroll::Top);
    assert!(engine.jump_to_previous_prompt().is_none() || rows(&mut engine)[0] == "$ p");
    engine.scroll(alacritty_terminal::grid::Scroll::Bottom);
    let r = rows(&mut engine);
    assert_eq!(r.last().map(String::as_str), Some("$ p"));
}

/// A one-column grid, and an empty one, survive width cycles with their
/// anchors.
#[test]
fn one_column_and_empty_grids() {
    let mut engine = Engine::with_history(5, 10, 100);
    engine.resize(5, 1);
    engine.resize(5, 10);
    assert_eq!(engine.copy_selection(), None);
    engine.feed(format!("{OSC_133_A}$ abc\r\nxyz").as_bytes());
    engine.select((0, 0), (0, 4));
    for cols in [1, 3, 1, 10] {
        engine.resize(5, cols);
        assert_eq!(
            engine.copy_selection().as_deref(),
            Some("$ abc"),
            "{cols} cols"
        );
    }
    engine.drain_markers();
    assert!(engine.select_previous_command().is_none());
    let mut term = term(3, 4, 50);
    feed_text(&mut term, 9, 12, 9, 0);
    for cols in [1, 2, 1, 7, 1, 4] {
        resize_and_check(&mut term, 3, cols);
    }
}
