//! N08a: mapping an owned terminal [`Snapshot`] to renderable draw items.
//!
//! Pure mapping code: no terminal locks, no engine calls, no winit. The
//! snapshot's damage report scopes which cells produce draw items —
//! [`Damage::Full`] rescan every row, [`Damage::Partial`] rescans each
//! `[left..=right]` span (inclusive right, as reported by the engine), and
//! the cursor row is always rescanned because the cursor is presentation,
//! not damage. Cell geometry is pure terminal math (`col * cell_w`,
//! `row * cell_h`), per N02a; the shaper never decides where a cell starts.
//!
//! Colors are resolved against the snapshot's owned [`Colors`] table at
//! mapping time: a `Spec` color is used as-is; an `Indexed` or `Named`
//! color reads the table slot and falls back to the default when the slot
//! is unset (default foreground white, default background black, per the
//! session layer's resolution rule). The pinned `Flags` conceal/reverse
//! bits (`HIDDEN`/`INVERSE`) suppress the glyph or swap the resolved
//! fg/bg pair, in both draw items and background fills.

use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use glyphon::TextBounds;

use crate::engine::{CellText, Damage, Snapshot};

/// Terminal cell geometry for placing draw items, in physical pixels.
///
/// `cell_w` is measured per N02a (cross-character advance equality), never
/// hardcoded; `cell_h` and `font_size` are the N02a baseline (24 px row for
/// a 20 px font).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TermLayout {
    pub cell_w: f32,
    pub cell_h: f32,
    pub font_size: f32,
}

/// One shaped cluster to draw in one cell.
///
/// `cluster` is the cell character plus its `zerowidth` characters, shaped
/// per cell exactly as N02a/N02b shape them. `fg`/`bg` are the resolved
/// colors after conceal/reverse handling. `bounds` is the cell's allocation
/// in physical pixels: the glyph ink is clipped to it (N02b).
#[derive(Debug, Clone, PartialEq)]
pub struct DrawItem {
    pub row: usize,
    pub col: usize,
    pub cluster: String,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bounds: TextBounds,
    pub is_cursor: bool,
}

/// One background fill over `span` cells starting at `(row, col)`, in the
/// resolved cell background color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FillRect {
    pub row: usize,
    pub col: usize,
    pub span: u16,
    pub color: Rgb,
}

/// Fallback colors for unset table slots, per the session layer's
/// resolution rule: default foreground white, default background black.
const DEFAULT_FG: Rgb = Rgb {
    r: 255,
    g: 255,
    b: 255,
};
const DEFAULT_BG: Rgb = Rgb { r: 0, g: 0, b: 0 };

/// Resolve one vte color against the snapshot's color table.
///
/// `Spec(r)` returns `r` as-is; `Indexed(i)` and `Named(n)` read the table
/// slot `i`/`n` and return `default` when the slot is unset.
pub fn resolve_color(colors: &Colors, color: &Color, default: Rgb) -> Rgb {
    match color {
        Color::Spec(r) => *r,
        Color::Indexed(i) => colors[*i as usize].unwrap_or(default),
        Color::Named(n) => colors[*n as usize].unwrap_or(default),
    }
}

/// Whether the snapshot's cursor is visible: the `SHOW_CURSOR` mode bit is
/// set and the cursor position is present.
pub fn cursor_visible(snap: &Snapshot) -> bool {
    snap.modes.contains(TermMode::SHOW_CURSOR) && snap.cursor.is_some()
}

/// Whether a damage report is a no-op for this snapshot: the pinned no-op
/// shape is a single `Partial` span exactly at the cursor cell
/// (`line == cursor row`, `left == right == cursor col`) — what the engine
/// reports when only the cursor moved in place — or an empty `Partial`
/// with no cursor. Anything wider, on another row, `Full`, or a span with
/// no cursor to account for it is real damage.
pub fn is_noop_damage(damage: &Damage, cursor: Option<(usize, usize)>) -> bool {
    let Damage::Partial(lines) = damage else {
        return false;
    };
    match cursor {
        None => lines.is_empty(),
        Some((line, col)) => {
            lines.len() == 1
                && lines[0].line == line
                && lines[0].left == col
                && lines[0].right == col
        }
    }
}

/// The snapshot's resolved default foreground (table slot
/// `NamedColor::Foreground`, or white when unset).
fn default_fg(snap: &Snapshot) -> Rgb {
    resolve_color(
        &snap.colors,
        &Color::Named(NamedColor::Foreground),
        DEFAULT_FG,
    )
}

/// The snapshot's resolved default background (table slot
/// `NamedColor::Background`, or black when unset).
fn default_bg(snap: &Snapshot) -> Rgb {
    resolve_color(
        &snap.colors,
        &Color::Named(NamedColor::Background),
        DEFAULT_BG,
    )
}

/// Resolve a cell's fg/bg pair, swapping them for the pinned `INVERSE`
/// flag (the block's "inverted" semantics).
fn resolved_pair(snap: &Snapshot, cell: &CellText, fg_default: Rgb, bg_default: Rgb) -> (Rgb, Rgb) {
    let mut fg = resolve_color(&snap.colors, &cell.fg, fg_default);
    let mut bg = resolve_color(&snap.colors, &cell.bg, bg_default);
    if cell.flags.contains(Flags::INVERSE) {
        let (f, b) = (fg, bg);
        (fg, bg) = (b, f);
    }
    (fg, bg)
}

/// The cell's pixel allocation as `TextBounds`, pure terminal math:
/// `left = col * cell_w`, `top = row * cell_h`, width `span * cell_w`,
/// height `cell_h` (N02b constructs bounds from the same numbers).
fn cell_bounds(row: usize, col: usize, span: u16, layout: &TermLayout) -> TextBounds {
    let left = (col as f32 * layout.cell_w) as i32;
    let top = (row as f32 * layout.cell_h) as i32;
    TextBounds {
        left,
        top,
        right: left + (span as f32 * layout.cell_w) as i32,
        bottom: top + layout.cell_h as i32,
    }
}

/// Per-row scan ranges (half-open `[start, end)` column pairs) selected by
/// the damage report, with the cursor row always scanned.
fn scan_ranges(snap: &Snapshot) -> Vec<Vec<(usize, usize)>> {
    let rows = snap.rows;
    let cols = snap.cols;
    let mut ranges: Vec<Vec<(usize, usize)>> = vec![Vec::new(); rows];
    match &snap.damage {
        Damage::Full => {
            for row in ranges.iter_mut() {
                row.push((0, cols));
            }
        }
        Damage::Partial(lines) => {
            for line_damage in lines {
                let line = line_damage.line;
                let left = line_damage.left;
                let right = line_damage.right + 1; // inclusive right
                if line < rows && right > left && left < cols {
                    ranges[line].push((left.min(cols), right.min(cols)));
                }
            }
        }
    }
    if let Some((line, _)) = snap.cursor
        && line < rows
    {
        ranges[line].push((0, cols));
    }
    ranges
}

/// Map the snapshot's damaged region (plus the cursor row) to draw items.
///
/// Zero-width and space cells produce no item (a non-default background
/// still produces a fill, via [`build_bg_fills`]); a pinned `HIDDEN` cell
/// suppresses the glyph (the fill still applies); the cluster is the cell
/// character plus its `zerowidth` characters; the cell under the cursor is
/// marked `is_cursor`. Items are emitted in row-major order.
pub fn build_draw_items(snap: &Snapshot, layout: &TermLayout) -> Vec<DrawItem> {
    let rows = snap.rows;
    let cols = snap.cols;
    if rows == 0 || cols == 0 || snap.cells.len() != rows * cols {
        return Vec::new();
    }
    let fg_default = default_fg(snap);
    let bg_default = default_bg(snap);

    let mut items = Vec::new();
    for (row, ranges) in scan_ranges(snap).into_iter().enumerate() {
        let mut ordered: Vec<(usize, usize)> = ranges;
        ordered.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (start, end) in ordered {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        for (start, end) in merged {
            for col in start..end.min(cols) {
                let cell = &snap.cells[row * cols + col];
                let span = cell.width();
                if span == 0 || cell.ch == ' ' || cell.flags.contains(Flags::HIDDEN) {
                    continue;
                }
                let (fg, bg) = resolved_pair(snap, cell, fg_default, bg_default);
                let mut cluster = cell.ch.to_string();
                cluster.extend(cell.zerowidth.iter().copied());
                items.push(DrawItem {
                    row,
                    col,
                    cluster,
                    fg,
                    bg,
                    bounds: cell_bounds(row, col, span, layout),
                    is_cursor: snap.cursor == Some((row, col)),
                });
            }
        }
    }
    items
}

/// Map every non-default background cell to a fill, row-major.
///
/// A cell produces one [`FillRect`] when its resolved background (after
/// the `INVERSE` swap) differs from the resolved default background.
/// Zero-width cells produce no fill.
pub fn build_bg_fills(snap: &Snapshot, layout: &TermLayout) -> Vec<FillRect> {
    let _ = layout;
    let rows = snap.rows;
    let cols = snap.cols;
    if rows == 0 || cols == 0 || snap.cells.len() != rows * cols {
        return Vec::new();
    }
    let fg_default = default_fg(snap);
    let bg_default = default_bg(snap);

    let mut fills = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            let cell = &snap.cells[row * cols + col];
            let span = cell.width();
            if span == 0 {
                continue;
            }
            let (_, bg) = resolved_pair(snap, cell, fg_default, bg_default);
            if bg != bg_default {
                fills.push(FillRect {
                    row,
                    col,
                    span,
                    color: bg,
                });
            }
        }
    }
    fills
}

/// The cursor's background fill: a block fill at the cursor cell in the
/// resolved `NamedColor::Cursor` color (default foreground when the slot
/// is unset), or `None` when the cursor is not visible or out of range.
pub fn cursor_fill(snap: &Snapshot, layout: &TermLayout) -> Option<FillRect> {
    let _ = layout;
    let (row, col) = snap.cursor?;
    let rows = snap.rows;
    let cols = snap.cols;
    if !cursor_visible(snap)
        || rows == 0
        || row >= rows
        || col >= cols
        || snap.cells.len() != rows * cols
    {
        return None;
    }
    let span = snap.cells[row * cols + col].width().max(1);
    let color = resolve_color(
        &snap.colors,
        &Color::Named(NamedColor::Cursor),
        default_fg(snap),
    );
    Some(FillRect {
        row,
        col,
        span,
        color,
    })
}
