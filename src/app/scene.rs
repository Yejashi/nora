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
    /// Pixel rectangles over everything else (separators, cursor outlines).
    pub rects: Vec<PixelRect>,
    /// Pixel rectangles under the text, after the cell fills (the tab bar).
    pub backdrops: Vec<PixelRect>,
    /// Kitty graphics placements (ADR-014), lowest z first per pane.
    pub images: Vec<super::images::ImageDraw>,
}

/// Pane separator color.
const SEPARATOR: [u8; 4] = [0x55, 0x55, 0x55, 255];
/// Search bar text color.
const BAR_FG: [u8; 4] = [0xff, 0xff, 0xff, 255];
/// The widest a tab grows, in cells.
const TAB_MAX_COLS: u32 = 28;

/// The tab bar's colors, from the theme: the terminal background (its
/// alpha the `[window] opacity`), the foreground and an accent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabColors {
    pub bg: [u8; 4],
    pub fg: [u8; 4],
    pub accent: [u8; 4],
}

/// What the tab bar shows.
pub struct TabBar<'a> {
    /// One title per tab, in order; empty for a tab without one.
    pub titles: &'a [String],
    pub active: usize,
    /// The tab under the mouse pointer.
    pub hovered: Option<usize>,
    pub colors: TabColors,
}

/// Each tab's width in cells: the bar shared out evenly, at most
/// [`TAB_MAX_COLS`], at least one cell; 0 with no tabs or no columns. Tabs
/// start at column 0, so tab `i` covers `i * width` up to the next one.
pub fn tab_width(count: usize, cols: u32) -> u32 {
    if count == 0 {
        return 0;
    }
    (cols / count as u32).clamp(1, TAB_MAX_COLS).min(cols)
}

/// The tab at grid column `col` of the tab bar, if any.
pub fn tab_at(count: usize, cols: u32, col: u32) -> Option<usize> {
    let width = tab_width(count, cols);
    if width == 0 || col >= cols {
        return None;
    }
    let index = (col / width) as usize;
    (index < count && (index as u32 + 1) * width <= cols).then_some(index)
}

/// A tab label within `room` cells: the number, two spaces, then the title
/// cut with an ellipsis when it does not fit. The number alone when there
/// is no title or no room for one.
fn tab_label(number: &str, title: &str, room: u32) -> String {
    use unicode_width::UnicodeWidthChar;
    let title = title.trim();
    let left = room.saturating_sub(number.len() as u32 + 2);
    if title.is_empty() || left < 2 {
        return number.to_string();
    }
    let mut label = format!("{number}  ");
    let total: u32 = title.chars().map(|c| c.width().unwrap_or(0) as u32).sum();
    if total <= left {
        label.push_str(title);
        return label;
    }
    let mut used = 0;
    for c in title.chars() {
        let w = c.width().unwrap_or(0) as u32;
        if used + w + 1 > left {
            break;
        }
        label.push(c);
        used += w;
    }
    label.truncate(label.trim_end().len());
    label.push('…');
    label
}

/// `a` moved `t` of the way to `b` (RGB); `a`'s alpha is kept.
fn mix(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let channel = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    [
        channel(a[0], b[0]),
        channel(a[1], b[1]),
        channel(a[2], b[2]),
        a[3],
    ]
}

/// `snap` cut or padded to `rows` x `cols`: the stand-in drawn for a pane
/// whose latest snapshot predates a resize until the pump sends one at the
/// new size, so a live resize shows the old content instead of a bare
/// background. Added cells are blank; a wide character cut by the new
/// right edge becomes a blank; the cursor is kept only when inside.
pub fn fit_snapshot(snap: &Snapshot, rows: usize, cols: usize) -> Snapshot {
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::vte::ansi::{Color, NamedColor};
    let blank = |row, col| nora::engine::CellText {
        row,
        col,
        ch: ' ',
        fg: Color::Named(NamedColor::Foreground),
        bg: Color::Named(NamedColor::Background),
        flags: Flags::empty(),
        zerowidth: Vec::new(),
        underline_color: None,
        hyperlink: None,
    };
    let mut cells = Vec::with_capacity(rows * cols);
    let mut selected = Vec::with_capacity(rows * cols);
    for row in 0..rows {
        for col in 0..cols {
            let i = row * snap.cols + col;
            match snap
                .cells
                .get(i)
                .filter(|_| row < snap.rows && col < snap.cols)
            {
                Some(cell) if !(col + 1 == cols && cell.flags.contains(Flags::WIDE_CHAR)) => {
                    cells.push(cell.clone());
                    selected.push(snap.selected.get(i).copied().unwrap_or(false));
                }
                _ => {
                    cells.push(blank(row, col));
                    selected.push(false);
                }
            }
        }
    }
    Snapshot {
        rows,
        cols,
        epoch: snap.epoch,
        cells,
        selected,
        cursor: snap.cursor.filter(|&(row, col)| row < rows && col < cols),
        cursor_shape: snap.cursor_shape,
        modes: snap.modes,
        colors: snap.colors,
        damage: nora::engine::Damage::Full,
        display_offset: snap.display_offset,
        images: snap.images.clone(),
    }
}

impl Scene {
    /// Add one pane's snapshot at grid origin (`col0`, `row0`). The
    /// snapshot's colors must already be a full table (palette overlay).
    /// `frame_bg` is the color the frame is cleared to, its alpha the
    /// `[window] opacity`; a pane whose default background differs is
    /// filled with its own at that opacity. The cursor is
    /// drawn when `cursor` is `Some(window_focused)` (the active pane);
    /// an unfocused window shows it hollow.
    pub fn add_pane(
        &mut self,
        snap: &Snapshot,
        col0: u32,
        row0: u32,
        frame_bg: [u8; 4],
        cursor: Option<bool>,
        layout: &TermLayout,
    ) {
        let bg = default_bg(snap);
        let pane_bg = [bg.r, bg.g, bg.b];
        if pane_bg != frame_bg[..3] {
            for row in 0..snap.rows as u32 {
                self.fills.push(CellFill {
                    row: row0 + row,
                    col: col0,
                    span: snap.cols as u16,
                    color: [bg.r, bg.g, bg.b, frame_bg[3]],
                    cursor: false,
                });
            }
        }
        // Images scale with the cell (zoom): sizes and offsets were taken at
        // the cell size of the placement.
        let (cw, ch) = (layout.cell_w, layout.cell_h);
        let (x0, y0) = (col0 as f32 * cw, row0 as f32 * ch);
        let clip = [x0, y0, snap.cols as f32 * cw, snap.rows as f32 * ch];
        for image in &snap.images {
            let sx = cw / image.cell.0.max(1) as f32;
            let sy = ch / image.cell.1.max(1) as f32;
            self.images.push(super::images::ImageDraw {
                rect: [
                    x0 + image.col as f32 * cw + image.offset.0 as f32 * sx,
                    y0 + image.row as f32 * ch + image.offset.1 as f32 * sy,
                    image.size.0 as f32 * sx,
                    image.size.1 as f32 * sy,
                ],
                clip,
                image: image.clone(),
            });
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
                        cursor: false,
                    });
                }
                // Unicode placeholders show image tiles (ADR-014), never a glyph.
                // A plain space draws nothing, but one carrying an underline,
                // a strikethrough or combining marks is shaped: decorations
                // come only from shaped cells.
                let blank = cell.ch == ' '
                    && cell.zerowidth.is_empty()
                    && style.underline == UnderlineStyle::None
                    && !style.strikethrough;
                if blank || style.conceal || cell.ch == nora::graphics_image::PLACEHOLDER {
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
                        cursor: true,
                    });
                    // The glyph under the block is drawn in a color that
                    // contrasts with the cursor, so it stays legible.
                    if let Some(cell) = self.cells[first_cell..]
                        .iter_mut()
                        .find(|cell| cell.row == row && cell.col == col)
                    {
                        cell.style.fg = cursor_text_color(
                            cell.style.fg,
                            cell.style.bg,
                            color,
                            [bg.r, bg.g, bg.b, 255],
                        );
                    }
                }
            }
        }
    }

    /// The tab bar on grid row 0, `cols` wide: equal tabs ([`tab_width`])
    /// on a strip tinted from the theme. The active tab takes the terminal
    /// background, so it reads as joined to the pane below, with an accent
    /// line along its top; the others are dimmed, the hovered one lifted.
    /// Labels are the tab number and title, centered and cut with an
    /// ellipsis.
    pub fn add_tab_bar(&mut self, bar: &TabBar, cols: u32, layout: &TermLayout) {
        let count = bar.titles.len();
        let width = tab_width(count, cols);
        if width == 0 {
            return;
        }
        let TabColors { bg, fg, accent } = bar.colors;
        let opaque = |c: [u8; 4]| [c[0], c[1], c[2], 255];
        let (cw, ch) = (layout.cell_w, layout.cell_h);
        let strip = mix(bg, fg, 0.07);
        self.fills.push(CellFill {
            row: 0,
            col: 0,
            span: cols.min(u32::from(u16::MAX)) as u16,
            color: strip,
            cursor: false,
        });
        if let Some(hovered) = bar.hovered.filter(|&h| h != bar.active && h < count) {
            self.fills.push(CellFill {
                row: 0,
                col: hovered as u32 * width,
                span: width as u16,
                color: mix(bg, fg, 0.13),
                cursor: false,
            });
        }
        // Under the text, in order: the strip's bottom border, hairlines
        // between quiet tabs, then the active tab covering both.
        self.backdrops.push(PixelRect {
            x: 0.0,
            y: ch - 1.0,
            w: cols as f32 * cw,
            h: 1.0,
            color: mix(bg, fg, 0.16),
        });
        let quiet = |i: usize| i != bar.active && Some(i) != bar.hovered;
        for i in 1..count.min((cols / width) as usize) {
            if quiet(i - 1) && quiet(i) {
                self.backdrops.push(PixelRect {
                    x: (i as u32 * width) as f32 * cw,
                    y: (ch * 0.28).round(),
                    w: 1.0,
                    h: (ch * 0.44).round(),
                    color: mix(bg, fg, 0.22),
                });
            }
        }
        let x = (bar.active as u32 * width) as f32 * cw;
        let w = width as f32 * cw;
        self.backdrops.push(PixelRect {
            x,
            y: 0.0,
            w,
            h: ch,
            color: bg,
        });
        self.backdrops.push(PixelRect {
            x,
            y: 0.0,
            w,
            h: (ch / 12.0).round().max(2.0),
            color: opaque(accent),
        });
        for (index, title) in bar.titles.iter().enumerate() {
            let start = index as u32 * width;
            if start + width > cols {
                break;
            }
            let (number_fg, title_fg, cell_bg) = if index == bar.active {
                (accent, fg, bg)
            } else if Some(index) == bar.hovered {
                (mix(fg, bg, 0.45), mix(fg, bg, 0.15), mix(bg, fg, 0.13))
            } else {
                (mix(fg, bg, 0.6), mix(fg, bg, 0.4), strip)
            };
            let number = (index + 1).to_string();
            // One cell of padding on each side.
            let room = width.saturating_sub(2);
            let label = tab_label(&number, title, room);
            let (glyphs, used) = layout_line(&label, room);
            let left = start + 1 + (room - used) / 2;
            for (at, c, w) in glyphs {
                let in_number = (at as usize) < number.len();
                let color = if in_number { number_fg } else { title_fg };
                self.cells.push(DrawCell {
                    row: 0,
                    col: left + at,
                    text: c.into(),
                    style: tab_style(opaque(color), cell_bg, index == bar.active),
                    width: w,
                });
            }
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
    /// whatever the pane shows there. Everything already in the scene is
    /// kept off that row: its cells and fills are removed, and pixel rects
    /// (cursor, separators) and images are cut at the row's top edge
    /// (`cell_h` is the row height in pixels). Add it after the panes and
    /// separators.
    pub fn add_search_bar(&mut self, text: &str, row: u32, cols: u32, cell_h: f32) {
        self.cells.retain(|cell| cell.row != row);
        self.fills.retain(|fill| fill.row != row);
        let (top, bottom) = (row as f32 * cell_h, (row + 1) as f32 * cell_h);
        self.backdrops
            .retain(|rect| rect.y + rect.h <= top || rect.y >= bottom);
        self.rects.retain_mut(|rect| {
            if rect.y >= top && rect.y < bottom {
                return false;
            }
            if rect.y < top && rect.y + rect.h > top {
                rect.h = top - rect.y;
            }
            true
        });
        for image in &mut self.images {
            let [_, y, _, h] = &mut image.clip;
            *h = h.min((top - *y).max(0.0));
        }
        self.fills.push(CellFill {
            row,
            col: 0,
            span: cols.min(u32::from(u16::MAX)) as u16,
            color: SEARCH_BAR_BG,
            cursor: false,
        });
        for (col, ch, width) in layout_line(text, cols).0 {
            self.cells.push(DrawCell {
                row,
                col,
                text: ch.into(),
                style: tab_style(BAR_FG, SEARCH_BAR_BG, false),
                width,
            });
        }
    }
}

/// Lay `text` out on one row by display width, within `cols` columns: the
/// visible characters with their column offset and width, and the columns
/// used. A wide character that would cross the edge ends the line; spaces
/// advance without a glyph and zero-width characters are dropped.
fn layout_line(text: &str, cols: u32) -> (Vec<(u32, char, u16)>, u32) {
    use unicode_width::UnicodeWidthChar;
    let mut glyphs = Vec::new();
    let mut at = 0u32;
    for ch in text.chars() {
        let width = ch.width().unwrap_or(0) as u32;
        if width == 0 {
            continue;
        }
        if at + width > cols {
            break;
        }
        if ch != ' ' {
            glyphs.push((at, ch, width as u16));
        }
        at += width;
    }
    (glyphs, at)
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
            // The block cursor is drawn after the other fills; the overlay
            // covers it, as it covers the glyphs.
            self.fills.retain(|f| {
                !(f.cursor
                    && f.row == row
                    && f.col < at + width
                    && f.col + u32::from(f.span.max(1)) > at)
            });
            self.fills.push(CellFill {
                row,
                col: at,
                span: width as u16,
                color: bg,
                cursor: false,
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

/// The color of the glyph under a block cursor: the cell background, unless
/// it is (nearly) the cursor color — e.g. a reverse-video cell under a
/// default cursor — then the cell foreground, then the default background,
/// and as a last resort black or white, whichever contrasts with the cursor.
fn cursor_text_color(fg: [u8; 4], bg: [u8; 4], cursor: [u8; 4], default_bg: [u8; 4]) -> [u8; 4] {
    // Close enough to read as one color: under 48 summed over R, G and B.
    let near = |a: [u8; 4]| {
        a.iter()
            .zip(cursor.iter())
            .take(3)
            .map(|(x, y)| u32::from(x.abs_diff(*y)))
            .sum::<u32>()
            < 48
    };
    if let Some(color) = [bg, fg, default_bg].into_iter().find(|c| !near(*c)) {
        return color;
    }
    let luma = 299 * u32::from(cursor[0]) + 587 * u32::from(cursor[1]) + 114 * u32::from(cursor[2]);
    if luma > 128 * 1000 {
        [0, 0, 0, 255]
    } else {
        [255, 255, 255, 255]
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

    /// A snapshot from before a resize is cut or padded to the new pane
    /// size instead of being dropped (no bare-background frame).
    #[test]
    fn fit_snapshot_cuts_and_pads() {
        let mut engine = nora::engine::Engine::new(3, 4);
        engine.feed("ab日\r\ncd".as_bytes());
        let snap = engine.snapshot();
        let row = |s: &Snapshot, r: usize| -> String {
            (0..s.cols).map(|c| s.cells[r * s.cols + c].ch).collect()
        };
        let small = fit_snapshot(&snap, 2, 3);
        assert_eq!((small.rows, small.cols, small.cells.len()), (2, 3, 6));
        assert_eq!(row(&small, 0), "ab ", "the cut wide char becomes a blank");
        assert_eq!(row(&small, 1), "cd ");
        assert_eq!(small.cursor, Some((1, 2)));
        assert_eq!(small.selected.len(), 6);
        for (i, cell) in small.cells.iter().enumerate() {
            assert_eq!((cell.row, cell.col), (i / 3, i % 3));
        }
        let big = fit_snapshot(&snap, 4, 6);
        assert_eq!((big.rows, big.cols, big.cells.len()), (4, 6, 24));
        assert_eq!(row(&big, 1), "cd    ");
        assert_eq!(row(&big, 3), "      ");
        assert_eq!(fit_snapshot(&snap, 1, 1).cursor, None, "cursor cut off");
    }

    const COLORS: TabColors = TabColors {
        bg: [0x10, 0x10, 0x10, 255],
        fg: [0xf0, 0xf0, 0xf0, 255],
        accent: [0x40, 0x80, 0xff, 255],
    };

    fn tab_bar(titles: &[&str], active: usize, hovered: Option<usize>, cols: u32) -> Scene {
        let titles: Vec<String> = titles.iter().map(|t| t.to_string()).collect();
        let mut scene = Scene::default();
        let bar = TabBar {
            titles: &titles,
            active,
            hovered,
            colors: COLORS,
        };
        scene.add_tab_bar(&bar, cols, &LAYOUT);
        scene
    }

    /// Equal tabs: labels centered in each, the active tab drawn in the
    /// terminal background with an accent line on top, its number in the
    /// accent and its title bold.
    #[test]
    fn tab_bar_labels_and_highlight() {
        let scene = tab_bar(&["", "vim"], 1, None, 40);
        let placed: Vec<(u32, &str)> = scene
            .cells
            .iter()
            .map(|c| (c.col, c.text.as_str()))
            .collect();
        assert_eq!(
            placed,
            vec![(9, "1"), (27, "2"), (30, "v"), (31, "i"), (32, "m")]
        );
        let active = scene
            .backdrops
            .iter()
            .find(|r| r.color == COLORS.bg)
            .unwrap();
        assert_eq!((active.x, active.w, active.h), (200.0, 200.0, 20.0));
        let accent = scene
            .backdrops
            .iter()
            .find(|r| r.color == COLORS.accent)
            .unwrap();
        assert_eq!((accent.x, accent.y, accent.h), (200.0, 0.0, 2.0));
        let number = scene.cells.iter().find(|c| c.col == 27).unwrap();
        assert_eq!(number.style.fg, COLORS.accent);
        assert!(
            scene
                .cells
                .iter()
                .filter(|c| c.col >= 27)
                .all(|c| c.style.bold)
        );
        assert!(!scene.cells[0].style.bold);
    }

    /// Wide characters take two columns in tab labels and the search bar,
    /// and one that would cross the edge is left out.
    #[test]
    fn bars_lay_out_by_display_width() {
        let scene = tab_bar(&["日本", ""], 0, None, 40);
        let placed: Vec<(u32, &str, u16)> = scene
            .cells
            .iter()
            .map(|c| (c.col, c.text.as_str(), c.width))
            .collect();
        assert_eq!(
            placed,
            vec![(6, "1", 1), (9, "日", 2), (11, "本", 2), (29, "2", 1)]
        );

        let mut bar = Scene::default();
        bar.add_search_bar("a日本", 5, 4, 10.0);
        let placed: Vec<(u32, u16)> = bar.cells.iter().map(|c| (c.col, c.width)).collect();
        assert_eq!(placed, vec![(0, 1), (1, 2)]);
    }

    /// Tabs share the bar evenly up to a cap; a click maps back to the tab
    /// drawn there, and the leftover columns belong to no tab.
    #[test]
    fn tab_geometry_and_hit_testing() {
        assert_eq!(tab_width(0, 80), 0);
        assert_eq!(tab_width(2, 80), 28, "capped");
        assert_eq!(tab_width(3, 40), 13);
        assert_eq!(tab_width(50, 40), 1, "at least one cell");
        assert_eq!(tab_width(1, 0), 0);
        assert_eq!(tab_at(2, 80, 0), Some(0));
        assert_eq!(tab_at(2, 80, 28), Some(1));
        assert_eq!(tab_at(2, 80, 55), Some(1));
        assert_eq!(tab_at(2, 80, 56), None, "past the last tab");
        assert_eq!(tab_at(3, 40, 39), None, "the leftover column");
        assert_eq!(tab_at(50, 40, 39), Some(39));
        assert_eq!(tab_at(50, 40, 40), None);
        assert_eq!(tab_at(0, 40, 0), None);
    }

    /// Titles are cut by display width with an ellipsis; without room for
    /// a title the number stands alone.
    #[test]
    fn tab_labels_fit_their_room() {
        assert_eq!(tab_label("1", "vim", 18), "1  vim");
        assert_eq!(tab_label("1", "a very long title here", 10), "1  a very…");
        assert_eq!(tab_label("1", "日本語", 7), "1  日…");
        assert_eq!(tab_label("12", "  ", 18), "12");
        assert_eq!(tab_label("1", "vim", 4), "1");
    }

    /// The hovered tab is lifted; hairlines separate only quiet neighbours;
    /// the strip keeps the background's alpha while text stays opaque.
    #[test]
    fn tab_bar_hover_separators_and_opacity() {
        let scene = tab_bar(&["a", "b", "c", "d"], 0, Some(2), 80);
        let hover = mix(COLORS.bg, COLORS.fg, 0.13);
        let lifted = scene.fills.iter().find(|f| f.color == hover).unwrap();
        assert_eq!((lifted.col, lifted.span), (40, 20));
        let hairlines: Vec<f32> = scene
            .backdrops
            .iter()
            .filter(|r| r.w == 1.0)
            .map(|r| r.x)
            .collect();
        assert!(
            hairlines.is_empty(),
            "every gap touches the active or hovered tab"
        );
        let scene = tab_bar(&["a", "b", "c", "d"], 0, None, 80);
        let hairlines: Vec<f32> = scene
            .backdrops
            .iter()
            .filter(|r| r.w == 1.0)
            .map(|r| r.x)
            .collect();
        assert_eq!(hairlines, vec![400.0, 600.0]);

        let mut translucent = COLORS;
        translucent.bg[3] = 128;
        let titles = vec!["a".to_string(), "b".to_string()];
        let mut scene = Scene::default();
        let bar = TabBar {
            titles: &titles,
            active: 0,
            hovered: None,
            colors: translucent,
        };
        scene.add_tab_bar(&bar, 40, &LAYOUT);
        assert_eq!(scene.fills[0].color[3], 128, "the strip");
        assert!(scene.cells.iter().all(|c| c.style.fg[3] == 255));
    }

    #[test]
    fn search_bar_replaces_its_row() {
        let mut scene = Scene::default();
        scene.cells.push(DrawCell {
            row: 3,
            col: 0,
            text: "x".into(),
            style: tab_style(BAR_FG, SEARCH_BAR_BG, false),
            width: 1,
        });
        let rect = |y, h| PixelRect {
            x: 0.0,
            y,
            w: 1.0,
            h,
            color: SEPARATOR,
        };
        // A cursor rect on the bar row, a separator running into it and a
        // rect above it (cell height 10: the bar spans y 30..40).
        scene.rects = vec![rect(32.0, 2.0), rect(0.0, 40.0), rect(5.0, 10.0)];
        scene.add_search_bar("Find: ab", 3, 20, 10.0);
        let text: String = scene.cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(text, "Find:ab");
        assert_eq!(scene.fills.len(), 1);
        let rects: Vec<(f32, f32)> = scene.rects.iter().map(|r| (r.y, r.h)).collect();
        assert_eq!(rects, vec![(0.0, 30.0), (5.0, 10.0)]);
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

    /// A snapshot of `bytes` fed to a fresh engine, colors overlaid with the
    /// default palette as the app does. Returns it with the frame color.
    fn pane(bytes: &[u8], rows: usize, cols: usize) -> (Snapshot, [u8; 4]) {
        let mut engine = nora::engine::Engine::new(rows, cols);
        engine.feed(bytes);
        let mut snap = engine.snapshot();
        let palette = nora::render::palette::Palette::default();
        snap.colors = palette.overlay(&snap.colors);
        let bg = palette.background();
        (snap, [bg.r, bg.g, bg.b, 255])
    }

    const LAYOUT: TermLayout = TermLayout {
        cell_w: 10.0,
        cell_h: 20.0,
        font_size: 16.0,
    };

    /// `\e[4mfoo bar`: the space between the words is still drawn, so its
    /// underline joins the two; an unstyled space is not.
    #[test]
    fn decorated_spaces_are_drawn() {
        let (snap, frame_bg) = pane(b"\x1b[4mfoo bar\x1b[0m x \x1b[9m \x1b[0m", 1, 20);
        let mut scene = Scene::default();
        scene.add_pane(&snap, 0, 0, frame_bg, None, &LAYOUT);
        let cols: Vec<u32> = scene.cells.iter().map(|c| c.col).collect();
        assert_eq!(cols, vec![0, 1, 2, 3, 4, 5, 6, 8, 10]);
        let space = scene.cells.iter().find(|c| c.col == 3).unwrap();
        assert_eq!(space.text.as_str(), " ");
        assert_eq!(space.style.underline, UnderlineStyle::Single);
        assert!(
            scene
                .cells
                .iter()
                .find(|c| c.col == 10)
                .unwrap()
                .style
                .strikethrough
        );
    }

    /// `[window] opacity`: a pane whose default background differs from the
    /// frame's is filled at the frame's alpha; an explicitly colored cell
    /// stays opaque.
    #[test]
    fn translucent_frame_keeps_explicit_cell_colors_opaque() {
        let (snap, frame_bg) = pane(b"\x1b[41mX\x1b[0m", 1, 4);
        let mut scene = Scene::default();
        scene.add_pane(&snap, 0, 0, [1, 2, 3, 128], None, &LAYOUT);
        let pane_fill = scene.fills.iter().find(|f| f.span == 4).unwrap();
        assert_eq!(
            pane_fill.color,
            [frame_bg[0], frame_bg[1], frame_bg[2], 128]
        );
        let red = scene.fills.iter().find(|f| f.span == 1).unwrap();
        assert_eq!(red.color[3], 255);
        let mut same = Scene::default();
        same.add_pane(
            &snap,
            0,
            0,
            [frame_bg[0], frame_bg[1], frame_bg[2], 128],
            None,
            &LAYOUT,
        );
        assert!(
            same.fills.iter().all(|f| f.span == 1),
            "no pane fill when the bg matches"
        );
    }

    /// A combining mark on a space keeps its cell.
    #[test]
    fn combining_mark_on_a_space_is_drawn() {
        let (snap, frame_bg) = pane(" \u{301}".as_bytes(), 1, 10);
        let mut scene = Scene::default();
        scene.add_pane(&snap, 0, 0, frame_bg, None, &LAYOUT);
        let texts: Vec<&str> = scene.cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec![" \u{301}"]);
    }

    #[test]
    fn cursor_text_contrasts_with_the_cursor() {
        let (white, black) = ([0xee, 0xee, 0xee, 255], [0x10, 0x10, 0x10, 255]);
        let red = [0xcc, 0x20, 0x20, 255];
        // Normal cell: the background.
        assert_eq!(cursor_text_color(white, black, white, black), black);
        // Reverse video under a default (foreground-colored) cursor.
        assert_eq!(cursor_text_color(black, white, white, black), black);
        // Nearly equal counts as equal.
        assert_eq!(
            cursor_text_color(red, [0xe8, 0xf0, 0xe8, 255], white, black),
            red
        );
        // Everything matches the cursor: the default background, else
        // black or white.
        assert_eq!(cursor_text_color(white, white, white, black), black);
        assert_eq!(
            cursor_text_color(white, white, white, white),
            [0, 0, 0, 255]
        );
        assert_eq!(
            cursor_text_color(black, black, black, black),
            [255, 255, 255, 255]
        );
    }

    /// A reverse-video cell under a block cursor in the foreground color
    /// keeps a visible glyph, and the block is marked to draw over the
    /// selection.
    #[test]
    fn block_cursor_on_reverse_video_keeps_its_glyph() {
        use alacritty_terminal::vte::ansi::NamedColor;
        let (mut snap, frame_bg) = pane(b"\x1b[7mX\x1b[0m\x1b[H", 2, 10);
        snap.colors[NamedColor::Cursor] = snap.colors[NamedColor::Foreground];
        let fg = snap.colors[NamedColor::Foreground].unwrap();
        let cursor = [fg.r, fg.g, fg.b, 255];
        let mut scene = Scene::default();
        scene.add_pane(&snap, 0, 0, frame_bg, Some(true), &LAYOUT);
        let glyph = scene.cells.iter().find(|c| c.col == 0).unwrap();
        assert_eq!(glyph.style.bg, cursor, "reverse video: bg is the fg");
        assert_ne!(glyph.style.fg, cursor);
        let block: Vec<&CellFill> = scene.fills.iter().filter(|f| f.cursor).collect();
        assert_eq!(block.len(), 1);
        assert_eq!((block[0].row, block[0].col, block[0].color), (0, 0, cursor));
        // A preedit over the cursor covers the block, as it did before.
        scene.add_preedit("a", 0, 0, 10, cursor, frame_bg);
        assert!(!scene.fills.iter().any(|f| f.cursor));
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
