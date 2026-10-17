//! Line anchors across width changes that reflow wrapped lines.
//!
//! A width change on the main screen joins or splits soft-wrapped rows,
//! which moves each line by the rows joined or split around it. The
//! vendored grid records where the reflow moved every point (`ReflowMap`,
//! `// nora:`). These tests check the map cell by cell against the grid.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

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
