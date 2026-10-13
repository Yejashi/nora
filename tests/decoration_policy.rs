//! N13c regression tests: underline/strikeout decoration resolution.
//!
//! The function under test is pure (no GPU, no window): `decoration_style`
//! resolves one cosmic-text [`DecorationSpan`] (a span of glyphs sharing the
//! same text decoration, as produced by `LayoutLine::decorations`) into a
//! concrete `DecorationStyle` — the resolved underline and strikethrough
//! colors plus the single/double underline trait — exactly as pinned by the
//! N13c slice signatures.
//!
//! The decoration types are the pinned cosmic-text 0.19.0 public types
//! (cosmic-text is a direct dependency of nora, the same pinned version
//! glyphon pulls in):
//! - `DecorationSpan { glyph_range, data, color_opt, font_size }`
//!   (cosmic-text `layout.rs`)
//! - `GlyphDecorationData { text_decoration, underline_metrics,
//!   strikethrough_metrics, ascent }` (cosmic-text `attrs.rs`)
//! - `TextDecoration { underline, underline_color_opt, strikethrough,
//!   strikethrough_color_opt, overline, overline_color_opt }`
//! - `UnderlineStyle { None, Single, Double }`
//! - `DecorationMetrics { offset, thickness }`
//! - `Color(pub u32)` with `Color::rgb` / `Color::rgba` / `as_rgba`
//!
//! Color resolution is a three-level fallback, per decoration:
//! 1. the decoration's explicit color
//!    (`underline_color_opt` / `strikethrough_color_opt`), if set;
//! 2. the span's `color_opt` (the first glyph's color), if set;
//! 3. the cell's resolved foreground (the `fg` argument).
//!
//! An `UnderlineStyle::None` span yields no underline; a `false`
//! `strikethrough` yields no strikethrough.

use cosmic_text::{
    Color, DecorationMetrics, DecorationSpan, GlyphDecorationData, TextDecoration, UnderlineStyle,
};
use nora::render::style::decoration_style;

/// The cell's resolved foreground RGBA used as the ultimate decoration color
/// fallback (the `fg` argument to [`decoration_style`]).
const FG: [u8; 4] = [10, 20, 30, 255];

/// Build an opaque RGBA with alpha 255, mirroring `text_policy.rs`.
fn rgba(r: u8, g: u8, b: u8) -> [u8; 4] {
    [r, g, b, 255]
}

/// Build a [`DecorationSpan`] carrying the given decoration config and span
/// fallback color. The glyph range, metrics and font size are fixed
/// placeholders: `decoration_style` resolves colors and traits, not geometry,
/// so these values are irrelevant to the assertions.
fn span(text_decoration: TextDecoration, color_opt: Option<Color>) -> DecorationSpan {
    DecorationSpan {
        glyph_range: 0..1,
        data: GlyphDecorationData {
            text_decoration,
            underline_metrics: DecorationMetrics::default(),
            strikethrough_metrics: DecorationMetrics::default(),
            ascent: 0.8,
        },
        color_opt,
        font_size: 20.0,
    }
}

#[test]
fn decoration_none_has_no_decorations() {
    let s = span(TextDecoration::new(), None);
    let d = decoration_style(&s, FG);
    assert_eq!(d.underline, None);
    assert!(!d.underline_double);
    assert_eq!(d.strikethrough, None);
}

#[test]
fn decoration_single_underline_explicit_color() {
    let td = TextDecoration {
        underline: UnderlineStyle::Single,
        underline_color_opt: Some(Color::rgb(1, 2, 3)),
        ..TextDecoration::new()
    };
    let s = span(td, None);
    let d = decoration_style(&s, FG);
    assert_eq!(d.underline, Some(rgba(1, 2, 3)));
    assert!(!d.underline_double);
    assert_eq!(d.strikethrough, None);
}

#[test]
fn decoration_single_underline_falls_back_to_span_color() {
    // No explicit underline color: the span's fallback color is used.
    let td = TextDecoration {
        underline: UnderlineStyle::Single,
        ..TextDecoration::new()
    };
    let s = span(td, Some(Color::rgb(40, 50, 60)));
    let d = decoration_style(&s, FG);
    assert_eq!(d.underline, Some(rgba(40, 50, 60)));
    assert!(!d.underline_double);
}

#[test]
fn decoration_single_underline_falls_back_to_fg() {
    // No explicit color and no span color: the cell foreground is used.
    let td = TextDecoration {
        underline: UnderlineStyle::Single,
        ..TextDecoration::new()
    };
    let s = span(td, None);
    let d = decoration_style(&s, FG);
    assert_eq!(d.underline, Some(FG));
    assert!(!d.underline_double);
}

#[test]
fn decoration_double_underline_sets_double_flag() {
    let td = TextDecoration {
        underline: UnderlineStyle::Double,
        ..TextDecoration::new()
    };
    let s = span(td, None);
    let d = decoration_style(&s, FG);
    assert_eq!(d.underline, Some(FG));
    assert!(d.underline_double);
}

#[test]
fn decoration_strikethrough_explicit_color() {
    let td = TextDecoration {
        strikethrough: true,
        strikethrough_color_opt: Some(Color::rgb(7, 8, 9)),
        ..TextDecoration::new()
    };
    let s = span(td, None);
    let d = decoration_style(&s, FG);
    assert_eq!(d.strikethrough, Some(rgba(7, 8, 9)));
    assert_eq!(d.underline, None);
}

#[test]
fn decoration_strikethrough_falls_back_to_fg() {
    let td = TextDecoration {
        strikethrough: true,
        ..TextDecoration::new()
    };
    let s = span(td, None);
    let d = decoration_style(&s, FG);
    assert_eq!(d.strikethrough, Some(FG));
}

#[test]
fn decoration_strikethrough_false_has_none() {
    // `TextDecoration::new()` leaves `strikethrough: false`.
    let s = span(TextDecoration::new(), None);
    let d = decoration_style(&s, FG);
    assert_eq!(d.strikethrough, None);
}

#[test]
fn decoration_underline_and_strikethrough_together() {
    let td = TextDecoration {
        underline: UnderlineStyle::Single,
        underline_color_opt: Some(Color::rgb(1, 1, 1)),
        strikethrough: true,
        strikethrough_color_opt: Some(Color::rgb(2, 2, 2)),
        ..TextDecoration::new()
    };
    let s = span(td, None);
    let d = decoration_style(&s, FG);
    assert_eq!(d.underline, Some(rgba(1, 1, 1)));
    assert!(!d.underline_double);
    assert_eq!(d.strikethrough, Some(rgba(2, 2, 2)));
}

#[test]
fn decoration_explicit_color_beats_span_color() {
    // The decoration's explicit color wins over the span fallback color.
    let td = TextDecoration {
        underline: UnderlineStyle::Single,
        underline_color_opt: Some(Color::rgb(1, 2, 3)),
        ..TextDecoration::new()
    };
    let s = span(td, Some(Color::rgb(40, 50, 60)));
    let d = decoration_style(&s, FG);
    assert_eq!(d.underline, Some(rgba(1, 2, 3)));
}

#[test]
fn decoration_span_color_beats_fg() {
    // No explicit color: the span fallback color wins over the cell
    // foreground, for both the underline and the strikethrough.
    let td = TextDecoration {
        underline: UnderlineStyle::Single,
        strikethrough: true,
        ..TextDecoration::new()
    };
    let s = span(td, Some(Color::rgb(40, 50, 60)));
    let d = decoration_style(&s, FG);
    assert_eq!(d.underline, Some(rgba(40, 50, 60)));
    assert_eq!(d.strikethrough, Some(rgba(40, 50, 60)));
}
