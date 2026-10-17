//! Frame scene assembly: every pane of the active tab placed at its
//! origin, plus the tab bar and the pane separators.
//!
//! Pure data transformation from snapshots to draw lists; no GPU, no
//! window. Coordinates are grid cells for cells and fills, physical
//! pixels for [`PixelRect`]s.

use alacritty_terminal::vte::ansi::CursorShape;
use cosmic_text::UnderlineStyle;
use nora::engine::Snapshot;
use nora::render::style::{CellStyle, cell_style};
use nora::render::term::{TermLayout, cursor_fill, default_bg};

use super::graphics::{CellFill, CellText, DrawCell};

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
    /// drawn when `cursor` is `Some(window_focused)` (the active pane);
    /// an unfocused window shows it hollow.
    pub fn add_pane(
        &mut self,
        snap: &Snapshot,
        col0: u32,
        row0: u32,
        frame_bg: [u8; 3],
        cursor: Option<bool>,
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
                let text = CellText::cluster(cell.ch, &cell.zerowidth);
                self.cells.push(DrawCell {
                    row: row0 + row as u32,
                    col: col0 + col as u32,
                    text,
                    style,
                    width: cell.width(),
                });
            }
        }
        if let Some(focused) = cursor
            && let Some(block) = cursor_fill(snap, layout)
        {
            let (row, col) = (row0 + block.row as u32, col0 + block.col as u32);
            let color = [block.color.r, block.color.g, block.color.b, 255];
            let (x, y) = (col as f32 * layout.cell_w, row as f32 * layout.cell_h);
            let w = f32::from(block.span) * layout.cell_w;
            let h = layout.cell_h;
            // Bar thickness scales with the cell (2 px at 20 px fonts).
            let t = (layout.cell_w / 6.0).round().max(1.0);
            let rect = |x, y, w, h| PixelRect { x, y, w, h, color };
            match (snap.cursor_shape, focused) {
                (CursorShape::Hidden, _) => {}
                // An unfocused window shows an outline, as other terminals do.
                (_, false) | (CursorShape::HollowBlock, _) => {
                    self.rects.push(rect(x, y, w, t));
                    self.rects.push(rect(x, y + h - t, w, t));
                    self.rects.push(rect(x, y, t, h));
                    self.rects.push(rect(x + w - t, y, t, h));
                }
                (CursorShape::Beam, true) => self.rects.push(rect(x, y, t, h)),
                (CursorShape::Underline, true) => self.rects.push(rect(x, y + h - t, w, t)),
                (CursorShape::Block, true) => {
                    self.fills.push(CellFill {
                        row,
                        col,
                        span: block.span,
                        color,
                    });
                    // The glyph under the block is drawn in the cell
                    // background so it stays legible.
                    if let Some(cell) = self.cells[first_cell..]
                        .iter_mut()
                        .find(|cell| cell.row == row && cell.col == col)
                    {
                        cell.style.fg = cell.style.bg;
                    }
                }
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
                        text: (*ch).into(),
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

impl Scene {
    /// The search bar: `text` on grid row `row`, across `cols` columns, over
    /// whatever the pane shows there.
    pub fn add_search_bar(&mut self, text: &str, row: u32, cols: u32) {
        self.cells.retain(|cell| cell.row != row);
        self.fills.retain(|fill| fill.row != row);
        self.fills.push(CellFill {
            row,
            col: 0,
            span: cols.min(u32::from(u16::MAX)) as u16,
            color: SEARCH_BAR_BG,
        });
        for (col, ch) in text.chars().take(cols as usize).enumerate() {
            if ch != ' ' {
                self.cells.push(DrawCell {
                    row,
                    col: col as u32,
                    text: ch.into(),
                    style: tab_style(TAB_ACTIVE_FG, SEARCH_BAR_BG, false),
                    width: 1,
                });
            }
        }
    }
}

const SEARCH_BAR_BG: [u8; 4] = [0x3a, 0x2e, 0x10, 255];

impl Scene {
    /// IME preedit text (being composed, not yet sent) at grid cell
    /// (`row`, `col`), underlined over an opaque background so it reads
    /// over whatever is under it; clipped at `cols`. Returns the columns
    /// it covers.
    pub fn add_preedit(
        &mut self,
        text: &str,
        row: u32,
        col: u32,
        cols: u32,
        fg: [u8; 4],
        bg: [u8; 4],
    ) -> u32 {
        self.add_overlay_text(text, row, col, cols, fg, bg, true)
    }

    /// An inline suggestion (ADR-013) at grid cell (`row`, `col`): `fg`
    /// (dim) over an opaque `bg`, never written into the grid; clipped at
    /// `cols`. Returns the columns it covers.
    pub fn add_ghost_text(
        &mut self,
        text: &str,
        row: u32,
        col: u32,
        cols: u32,
        fg: [u8; 4],
        bg: [u8; 4],
    ) -> u32 {
        self.add_overlay_text(text, row, col, cols, fg, bg, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn add_overlay_text(
        &mut self,
        text: &str,
        row: u32,
        col: u32,
        cols: u32,
        fg: [u8; 4],
        bg: [u8; 4],
        underline: bool,
    ) -> u32 {
        use unicode_width::UnicodeWidthChar;
        let mut at = col;
        for ch in text.chars() {
            let width = ch.width().unwrap_or(0) as u32;
            if width == 0 {
                continue;
            }
            if at + width > cols {
                break;
            }
            // Drop every glyph overlapping the span, including a wide one
            // that starts a column earlier (audit RV10).
            self.cells.retain(|c| {
                !(c.row == row && c.col < at + width && c.col + u32::from(c.width.max(1)) > at)
            });
            self.fills.push(CellFill {
                row,
                col: at,
                span: width as u16,
                color: bg,
            });
            let mut style = tab_style(fg, bg, false);
            if underline {
                style.underline = UnderlineStyle::Single;
            }
            self.cells.push(DrawCell {
                row,
                col: at,
                text: ch.into(),
                style,
                width: width as u16,
            });
            at += width;
        }
        at - col
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
    fn search_bar_replaces_its_row() {
        let mut scene = Scene::default();
        scene.cells.push(DrawCell {
            row: 3,
            col: 0,
            text: "x".into(),
            style: tab_style(TAB_FG, TAB_BAR_BG, false),
            width: 1,
        });
        scene.add_search_bar("Find: ab", 3, 20);
        let text: String = scene.cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(text, "Find:ab");
        assert_eq!(scene.fills.len(), 1);
    }

    #[test]
    fn preedit_uses_display_width_and_clips() {
        let mut scene = Scene::default();
        let fg = [255, 255, 255, 255];
        let bg = [0, 0, 0, 255];
        assert_eq!(scene.add_preedit("日本a", 2, 5, 40, fg, bg), 5);
        let cols: Vec<u32> = scene.cells.iter().map(|c| c.col).collect();
        assert_eq!(cols, vec![5, 7, 9]);
        let mut clipped = Scene::default();
        assert_eq!(clipped.add_preedit("日本", 0, 38, 40, fg, bg), 2);
    }

    /// Audit RV10: a wide glyph starting one column before the preedit
    /// overlaps it and is removed; one ending before it stays.
    #[test]
    fn preedit_removes_an_overlapping_wide_glyph() {
        let mut scene = Scene::default();
        let fg = [255, 255, 255, 255];
        let bg = [0, 0, 0, 255];
        for (col, text) in [(2, "日"), (4, "本")] {
            scene.cells.push(DrawCell {
                row: 0,
                col,
                text: text.into(),
                style: tab_style(fg, bg, false),
                width: 2,
            });
        }
        scene.add_preedit("a", 0, 5, 40, fg, bg);
        let left: Vec<(u32, &str)> = scene
            .cells
            .iter()
            .map(|c| (c.col, c.text.as_str()))
            .collect();
        assert_eq!(left, vec![(2, "日"), (5, "a")]);
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
