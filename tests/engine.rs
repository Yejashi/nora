//! Headless terminal-engine spike (N01).
//!
//! Proves the pinned alacritty_terminal pair accepts PTY-style bytes through
//! the `vte` processor and renders them into grid cells — no window, GPU or
//! PTY required. Kept small and reusable by the N03 engine adapter.

use alacritty_terminal::{
    event::VoidListener,
    grid::Dimensions,
    term::{Config, Term},
    vte,
};

/// Fixed test dimensions implementing the engine's `Dimensions` contract.
struct Dims {
    columns: usize,
    screen_lines: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// Build a headless term with default configuration.
fn headless_term() -> Term<VoidListener> {
    let dims = Dims {
        columns: 80,
        screen_lines: 24,
    };
    Term::new(Config::default(), &dims, VoidListener)
}

/// Feed PTY bytes into the term through a headless `vte` processor.
fn advance(term: &mut Term<VoidListener>, bytes: &[u8]) {
    let mut processor = vte::ansi::Processor::<vte::ansi::StdSyncHandler>::new();
    processor.advance(term, bytes);
}

/// Non-blank visible cells as (row, column, char), in reading order.
fn non_blank_cells(term: &Term<VoidListener>) -> Vec<(usize, usize, char)> {
    term.grid()
        .display_iter()
        .filter(|cell| cell.cell.c != ' ')
        .map(|cell| (cell.point.line.0 as usize, cell.point.column.0, cell.cell.c))
        .collect()
}

#[test]
fn bytes_render_into_grid_cells() {
    let mut term = headless_term();
    advance(&mut term, b"hello");

    assert_eq!(
        non_blank_cells(&term),
        vec![
            (0, 0, 'h'),
            (0, 1, 'e'),
            (0, 2, 'l'),
            (0, 3, 'l'),
            (0, 4, 'o')
        ]
    );
}

#[test]
fn carriage_return_and_line_feed_start_new_lines() {
    let mut term = headless_term();
    advance(&mut term, b"a\r\nb\r\nc");

    // Each CRLF pair moves to the first column of the next row.
    assert_eq!(
        non_blank_cells(&term),
        vec![(0, 0, 'a'), (1, 0, 'b'), (2, 0, 'c')]
    );
}

#[test]
fn bare_line_feed_keeps_the_column() {
    let mut term = headless_term();
    advance(&mut term, b"x\ny");

    // LF alone moves down one row without returning to column 0.
    assert_eq!(non_blank_cells(&term), vec![(0, 0, 'x'), (1, 1, 'y')]);
}

#[test]
fn ansi_color_escape_does_not_produce_cells() {
    let mut term = headless_term();
    advance(&mut term, b"\x1b[31mred\x1b[0m");

    // The SGR sequence changes attributes only; the visible text is "red".
    let cells = non_blank_cells(&term);
    let text: String = cells.iter().map(|(_, _, c)| *c).collect();
    assert_eq!(text, "red");
}
