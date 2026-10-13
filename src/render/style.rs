//! Per-cell style resolution (N13a) and decoration resolution (N13c).
//!
//! [`cell_style`] resolves a terminal cell's [`Flags`] (the alacritty
//! `TermFlags` re-exported by [`crate::engine`]) and colors into a concrete
//! [`CellStyle`]: resolved RGBA foreground/background plus the bold, italic,
//! faint, conceal, underline and strikethrough traits. It is pure — it reads
//! only the cell's flags and the terminal color table, never the GPU.
//!
//! [`decoration_style`] resolves one cosmic-text [`DecorationSpan`] (read
//! back from a shaped line's `decorations` in the render pass) into a
//! concrete [`DecorationStyle`]: the resolved underline and strikethrough
//! colors plus the single/double underline trait.

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, Rgb};
use cosmic_text::{DecorationSpan, UnderlineStyle};

/// Resolved visual style for one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellStyle {
    /// Resolved foreground RGBA.
    pub fg: [u8; 4],
    /// Resolved background RGBA.
    pub bg: [u8; 4],
    /// Bold weight (shape with a bold face).
    pub bold: bool,
    /// Italic style (shape with an italic face).
    pub italic: bool,
    /// Dimmed: the foreground is blended toward the background.
    pub faint: bool,
    /// Concealed: the cell is not drawn.
    pub conceal: bool,
    /// Underline style (N13c): `UNDERCURL`, `DOTTED_UNDERLINE` and
    /// `DASHED_UNDERLINE` have no cosmic-text counterpart and collapse to
    /// [`UnderlineStyle::Single`].
    pub underline: UnderlineStyle,
    /// Strikethrough (N13c).
    pub strikethrough: bool,
}

/// Resolve a cell's style from its [`Flags`] and colors.
///
/// Color resolution is a flat, direct index into the terminal color table:
/// `Named(n) -> colors[n]`, `Indexed(i) -> colors[i]`,
/// `Spec(rgb) -> rgba(r, g, b, 255)`. A `None` table entry falls back to the
/// default foreground (index 256) / background (index 257) by use.
///
/// `bold` (BOLD) maps a `Named` foreground through [`NamedColor::to_bright`]
/// before resolving. `reverse` (INVERSE) swaps foreground and background.
/// `faint` (DIM) blends the resolved foreground 50% toward the resolved
/// background, per channel. `italic` (ITALIC) and `conceal` (HIDDEN) set the
/// matching traits. `underline` (UNDERLINE / DOUBLE_UNDERLINE /
/// UNDERCURL / DOTTED_UNDERLINE / DASHED_UNDERLINE) and `strikethrough`
/// (STRIKEOUT) set the matching decoration traits (N13c).
pub fn cell_style(flags: Flags, fg: Color, bg: Color, colors: &Colors) -> CellStyle {
    let bold = flags.contains(Flags::BOLD);
    let italic = flags.contains(Flags::ITALIC);
    let faint = flags.contains(Flags::DIM);
    let conceal = flags.contains(Flags::HIDDEN);
    // UNDERCURL, DOTTED_UNDERLINE and DASHED_UNDERLINE have no cosmic-text
    // counterpart and render as a single underline (N13c).
    let underline = if flags.contains(Flags::DOUBLE_UNDERLINE) {
        UnderlineStyle::Double
    } else if flags.intersects(Flags::ALL_UNDERLINES) {
        UnderlineStyle::Single
    } else {
        UnderlineStyle::None
    };
    let strikethrough = flags.contains(Flags::STRIKEOUT);

    // Bold: a named foreground brightens before it is resolved through the
    // table (e.g. `Named::Red` -> `Named::BrightRed` -> `colors[1]`).
    let fg = if bold {
        match fg {
            Color::Named(name) => Color::Named(name.to_bright()),
            other => other,
        }
    } else {
        fg
    };

    let mut fg_rgba = resolve(fg, colors, true);
    let mut bg_rgba = resolve(bg, colors, false);

    // INVERSE: swap foreground and background.
    if flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg_rgba, &mut bg_rgba);
    }

    // DIM: blend the foreground 50% toward the background, per channel.
    if faint {
        for i in 0..4 {
            fg_rgba[i] = ((fg_rgba[i] as u16 + bg_rgba[i] as u16) / 2) as u8;
        }
    }

    CellStyle {
        fg: fg_rgba,
        bg: bg_rgba,
        bold,
        italic,
        faint,
        conceal,
        underline,
        strikethrough,
    }
}

/// The resolved decoration style for a cell (N13c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecorationStyle {
    /// The underline color (RGBA), or `None` if no underline.
    pub underline: Option<[u8; 4]>,
    /// `true` if the underline is double.
    pub underline_double: bool,
    /// The strikethrough color (RGBA), or `None` if no strikethrough.
    pub strikethrough: Option<[u8; 4]>,
}

/// Resolve one cosmic-text [`DecorationSpan`] into a concrete
/// [`DecorationStyle`] (N13c).
///
/// The `fg` argument is the cell's resolved foreground RGBA, used as the
/// ultimate color fallback.
pub fn decoration_style(span: &DecorationSpan, fg: [u8; 4]) -> DecorationStyle {
    let td = &span.data.text_decoration;
    let span_color = span.color_opt.map(|c| c.as_rgba());

    // Three-level color fallback, per decoration: the decoration's explicit
    // color, then the span's color, then the cell foreground.
    let resolve = |explicit: Option<cosmic_text::Color>| -> [u8; 4] {
        explicit.map(|c| c.as_rgba()).or(span_color).unwrap_or(fg)
    };

    let underline = if td.underline == UnderlineStyle::None {
        None
    } else {
        Some(resolve(td.underline_color_opt))
    };
    let strikethrough = if !td.strikethrough {
        None
    } else {
        Some(resolve(td.strikethrough_color_opt))
    };

    DecorationStyle {
        underline,
        underline_double: td.underline == UnderlineStyle::Double,
        strikethrough,
    }
}

/// Resolve one [`Color`] to RGBA through the terminal color table.
///
/// `Named(n)` and `Indexed(i)` index the table flat and directly (no +16
/// offset). `Spec(rgb)` is used directly. A `None` table entry falls back to
/// the default foreground (index 256) when `use_fg_fallback`, else the default
/// background (index 257). If that is also unset, black is returned.
fn resolve(color: Color, colors: &Colors, use_fg_fallback: bool) -> [u8; 4] {
    let rgb: Option<Rgb> = match color {
        Color::Named(name) => colors[name as usize],
        Color::Indexed(i) => colors[i as usize],
        Color::Spec(rgb) => Some(rgb),
    };
    let rgb = rgb.or_else(|| colors[if use_fg_fallback { 256 } else { 257 }]);
    match rgb {
        Some(rgb) => [rgb.r, rgb.g, rgb.b, 255],
        None => [0, 0, 0, 255],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::{NamedColor, Rgb};

    /// Opaque black background used as the "no flag" default in these tests
    /// (the `Color` enum has no default variant).
    const BG: Color = Color::Spec(Rgb { r: 0, g: 0, b: 0 });

    /// A color table with a few entries set; everything else is `None` so the
    /// default-foreground/background fallback (indices 256/257) is exercised.
    fn colors() -> Colors {
        let mut c = Colors::default();
        c[0] = Some(Rgb {
            r: 10,
            g: 20,
            b: 30,
        });
        c[1] = Some(Rgb {
            r: 11,
            g: 21,
            b: 31,
        });
        c[9] = Some(Rgb {
            r: 12,
            g: 22,
            b: 32,
        });
        c[256] = Some(Rgb {
            r: 200,
            g: 210,
            b: 220,
        });
        c[257] = Some(Rgb {
            r: 16,
            g: 17,
            b: 18,
        });
        c
    }

    #[test]
    fn named_resolves_flat() {
        let s = cell_style(
            Flags::empty(),
            Color::Named(NamedColor::Black),
            BG,
            &colors(),
        );
        assert_eq!(s.fg, [10, 20, 30, 255]);
    }

    #[test]
    fn indexed_resolves_flat() {
        let s = cell_style(
            Flags::empty(),
            Color::Spec(Rgb { r: 1, g: 2, b: 3 }),
            BG,
            &colors(),
        );
        assert_eq!(s.fg, [1, 2, 3, 255]);
        let s = cell_style(Flags::empty(), Color::Indexed(1), BG, &colors());
        assert_eq!(s.fg, [11, 21, 31, 255]);
    }

    #[test]
    fn none_entry_falls_back_to_default_fg_bg() {
        // Blue (4) is unset in the table for both colors.
        let s = cell_style(
            Flags::empty(),
            Color::Named(NamedColor::Blue),
            Color::Named(NamedColor::Blue),
            &colors(),
        );
        // Foreground falls back to the default foreground (256).
        assert_eq!(s.fg, [200, 210, 220, 255]);
        // Background falls back to the default background (257).
        assert_eq!(s.bg, [16, 17, 18, 255]);
    }

    #[test]
    fn bold_brightens_named_fg() {
        let s = cell_style(Flags::BOLD, Color::Named(NamedColor::Red), BG, &colors());
        // Red -> to_bright() -> BrightRed (9) -> colors[9].
        assert_eq!(s.fg, [12, 22, 32, 255]);
        assert!(s.bold);
    }

    #[test]
    fn inverse_swaps_fg_bg() {
        // Blue (4) is unset in the table -> resolves to the default
        // background (257).
        let s = cell_style(
            Flags::INVERSE,
            Color::Named(NamedColor::Black),
            Color::Named(NamedColor::Blue),
            &colors(),
        );
        assert_eq!(s.fg, [16, 17, 18, 255]);
        assert_eq!(s.bg, [10, 20, 30, 255]);
    }

    #[test]
    fn dim_blends_fg_toward_bg() {
        // Blue (4) is unset in the table -> resolves to the default
        // background (257).
        let s = cell_style(
            Flags::DIM,
            Color::Named(NamedColor::Black),
            Color::Named(NamedColor::Blue),
            &colors(),
        );
        // fg = (10+16)/2, (20+17)/2, (30+18)/2.
        assert_eq!(s.fg, [13, 18, 24, 255]);
        assert!(s.faint);
    }

    #[test]
    fn traits_are_set() {
        let s = cell_style(
            Flags::BOLD | Flags::ITALIC | Flags::HIDDEN,
            Color::Spec(Rgb { r: 5, g: 5, b: 5 }),
            BG,
            &colors(),
        );
        assert!(s.bold && s.italic && s.conceal);
        assert_eq!(s.fg, [5, 5, 5, 255]);
    }

    #[test]
    fn underline_flags_resolve() {
        let c = Color::Spec(Rgb { r: 1, g: 1, b: 1 });
        let s = cell_style(Flags::UNDERLINE, c, BG, &colors());
        assert_eq!(s.underline, UnderlineStyle::Single);
        let s = cell_style(Flags::DOUBLE_UNDERLINE, c, BG, &colors());
        assert_eq!(s.underline, UnderlineStyle::Double);
        // UNDERCURL, DOTTED and DASHED have no cosmic-text counterpart and
        // collapse to a single underline (N13c).
        for f in [
            Flags::UNDERCURL,
            Flags::DOTTED_UNDERLINE,
            Flags::DASHED_UNDERLINE,
        ] {
            let s = cell_style(f, c, BG, &colors());
            assert_eq!(s.underline, UnderlineStyle::Single);
        }
        let s = cell_style(Flags::empty(), c, BG, &colors());
        assert_eq!(s.underline, UnderlineStyle::None);
    }

    #[test]
    fn strikeout_flag_resolves() {
        let c = Color::Spec(Rgb { r: 1, g: 1, b: 1 });
        let s = cell_style(Flags::STRIKEOUT, c, BG, &colors());
        assert!(s.strikethrough);
        let s = cell_style(Flags::empty(), c, BG, &colors());
        assert!(!s.strikethrough);
    }
}
