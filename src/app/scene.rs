//! Frame scene assembly: every pane of the active tab placed at its
//! origin, plus the tab bar and the pane separators.
//!
//! Pure data transformation from snapshots to draw lists; no GPU, no
//! window. Coordinates are grid cells for cells and fills, physical
//! pixels for [`PixelRect`]s.

use cosmic_text::UnderlineStyle;
use nora::engine::Snapshot;
use nora::render::style::{CellStyle, cell_style};
use nora::render::term::{TermLayout, cursor_fill, default_bg};

use super::graphics::{CellFill, DrawCell};

/// A solid rectangle in physical pixels (pane separators).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: [u8; 4],
}

/// The draw lists for one frame.
#[derive(Default)]
pub struct Scene {
    pub cells: Vec<DrawCell>,
    pub fills: Vec<CellFill>,
    pub rects: Vec<PixelRect>,
}

/// Separator and inactive-tab colors.
const SEPARATOR: [u8; 4] = [0x55, 0x55, 0x55, 255];
const TAB_BAR_BG: [u8; 4] = [0x26, 0x26, 0x26, 255];
const TAB_FG: [u8; 4] = [0xb0, 0xb0, 0xb0, 255];
const TAB_ACTIVE_BG: [u8; 4] = [0x44, 0x66, 0x88, 255];
const TAB_ACTIVE_FG: [u8; 4] = [0xff, 0xff, 0xff, 255];

impl Scene {
    /// Add one pane's snapshot at grid origin (`col0`, `row0`). The
    /// snapshot's colors must already be a full table (palette overlay).
    /// `frame_bg` is the color the frame is cleared to; a pane whose
    /// default background differs is filled with its own. The cursor is
    /// drawn only when `cursor` is set (the active pane).
    pub fn add_pane(
        &mut self,
        snap: &Snapshot,
        col0: u32,
        row0: u32,
        frame_bg: [u8; 3],
        cursor: bool,
        layout: &TermLayout,
    ) {
        let bg = default_bg(snap);
        let pane_bg = [bg.r, bg.g, bg.b];
        if pane_bg != frame_bg {
            for row in 0..snap.rows as u32 {
                self.fills.push(CellFill {
                    row: row0 + row,
                    col: col0,
                    span: snap.cols as u16,
                    color: [bg.r, bg.g, bg.b, 255],
                });
            }
        }
        let first_cell = self.cells.len();
        for row in 0..snap.rows {
            for col in 0..snap.cols {
                let cell = &snap.cells[row * snap.cols + col];
                // Spacer cells (wide-char continuation) are not drawn.
                if cell.width() == 0 {
                    continue;
                }
                let style = cell_style(cell.flags, cell.fg, cell.bg, &snap.colors);
                if style.bg[..3] != pane_bg {
                    self.fills.push(CellFill {
                        row: row0 + row as u32,
                        col: col0 + col as u32,
                        span: cell.width(),
                        color: style.bg,
                    });
                }
                if cell.ch == ' ' || style.conceal {
                    continue;
                }
                // The cluster: the character plus its combining marks.
                let mut text = String::with_capacity(4);
                text.push(cell.ch);
                text.extend(cell.zerowidth.iter());
                self.cells.push(DrawCell {
                    row: row0 + row as u32,
                    col: col0 + col as u32,
                    text,
                    style,
                    width: cell.width(),
                });
            }
        }
        if cursor && let Some(block) = cursor_fill(snap, layout) {
            let (row, col) = (row0 + block.row as u32, col0 + block.col as u32);
            self.fills.push(CellFill {
                row,
                col,
                span: block.span,
                color: [block.color.r, block.color.g, block.color.b, 255],
            });
            // The glyph under the block is drawn in the cell background so
            // it stays legible.
            if let Some(cell) = self.cells[first_cell..]
                .iter_mut()
                .find(|cell| cell.row == row && cell.col == col)
            {
                cell.style.fg = cell.style.bg;
            }
        }
    }

    /// The tab bar on grid row 0: one label per tab, the active one
    /// highlighted, the rest of the row in the bar color.
    pub fn add_tab_bar(&mut self, labels: &[String], active: usize, cols: u32) {
        self.fills.push(CellFill {
            row: 0,
            col: 0,
            span: cols.min(u32::from(u16::MAX)) as u16,
            color: TAB_BAR_BG,
        });
        let mut col = 0u32;
        for (index, label) in labels.iter().enumerate() {
            let (fg, bg) = if index == active {
                (TAB_ACTIVE_FG, TAB_ACTIVE_BG)
            } else {
                (TAB_FG, TAB_BAR_BG)
            };
            let text: Vec<char> = format!(" {} ", label).chars().collect();
            let width = (text.len() as u32).min(cols.saturating_sub(col));
            if width == 0 {
                break;
            }
            self.fills.push(CellFill {
                row: 0,
                col,
                span: width as u16,
                color: bg,
            });
            for (i, ch) in text.iter().take(width as usize).enumerate() {
                if *ch != ' ' {
                    self.cells.push(DrawCell {
                        row: 0,
                        col: col + i as u32,
                        text: ch.to_string(),
                        style: tab_style(fg, bg, index == active),
                        width: 1,
                    });
                }
            }
            col += width + 1;
        }
    }

    /// 1-pixel separators on the shared edges of tiled panes: a vertical
    /// line at each pane's left edge (unless at column 0) and a horizontal
    /// line at each pane's top edge (unless at `top_row`).
    pub fn add_separators(
        &mut self,
        panes: &[(u32, u32, u32, u32)],
        top_row: u32,
        cell_w: f32,
        cell_h: f32,
    ) {
        for &(col, row, w, h) in panes {
            if col > 0 {
                self.rects.push(PixelRect {
                    x: col as f32 * cell_w,
                    y: row as f32 * cell_h,
                    w: 1.0,
                    h: h as f32 * cell_h,
                    color: SEPARATOR,
                });
            }
            if row > top_row {
                self.rects.push(PixelRect {
                    x: col as f32 * cell_w,
                    y: row as f32 * cell_h,
                    w: w as f32 * cell_w,
                    h: 1.0,
                    color: SEPARATOR,
                });
            }
        }
    }
}

fn tab_style(fg: [u8; 4], bg: [u8; 4], bold: bool) -> CellStyle {
    CellStyle {
        fg,
        bg,
        bold,
        italic: false,
        faint: false,
        conceal: false,
        underline: UnderlineStyle::None,
        strikethrough: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_bar_labels_and_highlight() {
        let mut scene = Scene::default();
        scene.add_tab_bar(&["1".to_string(), "2 vim".to_string()], 1, 40);
        let text: String = scene.cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(text, "12vim");
        let active_fill = scene.fills.iter().find(|f| f.color == TAB_ACTIVE_BG);
        assert_eq!(active_fill.map(|f| (f.col, f.span)), Some((4, 7)));
    }

    #[test]
    fn separators_only_on_shared_edges() {
        let mut scene = Scene::default();
        // Two panes side by side below a tab bar row.
        scene.add_separators(&[(0, 1, 10, 5), (10, 1, 10, 5)], 1, 8.0, 16.0);
        assert_eq!(scene.rects.len(), 1);
        assert_eq!(scene.rects[0].x, 80.0);
        assert_eq!(scene.rects[0].h, 80.0);
    }
}
