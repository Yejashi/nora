//! N13a regression tests: per-character font fallback and cell style
//! resolution.
//!
//! Both functions under test are pure (no GPU, no window):
//! `first_supporting` picks the first candidate font that supports a
//! character against the pinned N02 font stack (env-overridable), and
//! `cell_style` resolves a cell's flags and colors into a `CellStyle`
//! exactly as pinned by the N13a slice signatures.

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use glyphon::{FontSystem, fontdb};
use nora::render::fallback::first_supporting;
use nora::render::style::{CellStyle, cell_style};

/// Pinned mono candidate paths, in resolution order (N02 list, verbatim).
const MONO_CANDIDATES: [&str; 2] = [
    "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
    "/nix/store/8m9r3c9q6lx9rj9vffs5d53h0w0cs4a9-noto-fonts-2026.05.01/share/fonts/noto/NotoSansMono.ttf",
];

/// Pinned CJK candidate paths, in resolution order (N02 list, verbatim).
const CJK_CANDIDATES: [&str; 2] = [
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/nix/store/i0nxswc8ljllp6lnha7vpmr3w2l33khj-noto-fonts-cjk-sans-2.004/share/fonts/opentype/noto-cjk/NotoSansCJK-VF.otf.ttc",
];

/// Resolved RGBA of the default palette entries (indices 256/257).
const DEFAULT_FG: [u8; 4] = [200, 210, 220, 255];
const DEFAULT_BG: [u8; 4] = [20, 30, 40, 255];

static MONO_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static CJK_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Resolve a pinned test font: env override first, then the candidate list.
fn resolve_font(env_var: &str, candidates: &[&str]) -> String {
    if let Ok(path) = std::env::var(env_var)
        && std::path::Path::new(&path).is_file()
    {
        return path;
    }
    for candidate in candidates {
        if std::path::Path::new(candidate).is_file() {
            return candidate.to_string();
        }
    }
    panic!("no candidate test font available for {env_var}: {candidates:?}");
}

fn mono_path() -> &'static str {
    MONO_PATH
        .get_or_init(|| resolve_font("NORA_TEST_FONT_MONO", &MONO_CANDIDATES))
        .as_str()
}

fn cjk_path() -> &'static str {
    CJK_PATH
        .get_or_init(|| resolve_font("NORA_TEST_FONT_CJK", &CJK_CANDIDATES))
        .as_str()
}

/// Font system built from the N02 pinned stack (plus the system scan that
/// `FontSystem::new_with_fonts` always performs).
fn font_system() -> FontSystem {
    FontSystem::new_with_fonts([
        fontdb::Source::File(mono_path().into()),
        fontdb::Source::File(cjk_path().into()),
    ])
}

/// The on-disk path of a font source, when it has one.
fn source_path(source: &fontdb::Source) -> Option<&std::path::Path> {
    match source {
        fontdb::Source::File(path) => Some(path),
        fontdb::Source::SharedFile(path, _) => Some(path),
        fontdb::Source::Binary(_) => None,
    }
}

/// The `fontdb::ID` of the first face whose source is the font at `path`.
fn font_id(fs: &FontSystem, path: &str) -> fontdb::ID {
    fs.db()
        .faces()
        .find(|face| source_path(&face.source).is_some_and(|p| p == std::path::Path::new(path)))
        .map(|face| face.id)
        .unwrap_or_else(|| panic!("pinned font did not load: {path}"))
}

#[test]
fn first_supporting_ascii_resolves_to_mono() {
    let mut fs = font_system();
    let mono = font_id(&fs, mono_path());
    let cjk = font_id(&fs, cjk_path());
    assert_ne!(mono, cjk, "mono and CJK faces must be distinct");
    assert_eq!(first_supporting(&mut fs, 'A', &[mono, cjk]), Some(mono));
}

#[test]
fn first_supporting_cjk_resolves_to_cjk_font() {
    let mut fs = font_system();
    let mono = font_id(&fs, mono_path());
    let cjk = font_id(&fs, cjk_path());
    // U+6D77 (汉): covered by the CJK face, not by the mono face.
    assert_eq!(
        first_supporting(&mut fs, '\u{6D77}', &[mono, cjk]),
        Some(cjk)
    );
}

#[test]
fn first_supporting_prefers_earlier_candidate() {
    let mut fs = font_system();
    let mono = font_id(&fs, mono_path());
    let cjk = font_id(&fs, cjk_path());
    // Both fonts cover ASCII; the earlier candidate wins.
    assert_eq!(first_supporting(&mut fs, 'A', &[cjk, mono]), Some(cjk));
}

#[test]
fn first_supporting_unsupported_char_returns_none() {
    let mut fs = font_system();
    let mono = font_id(&fs, mono_path());
    let cjk = font_id(&fs, cjk_path());
    // U+1200 (Ethiopic): covered by neither pinned face — the N02 stack
    // needed a dedicated Ethiopic font for exactly this character.
    assert_eq!(first_supporting(&mut fs, '\u{1200}', &[mono, cjk]), None);
}

#[test]
fn first_supporting_empty_candidates_returns_none() {
    let mut fs = font_system();
    assert_eq!(first_supporting(&mut fs, 'A', &[]), None);
}

/// Palette with known entries; every other entry stays `None`, exactly like
/// `Colors::default()`.
fn palette() -> Colors {
    let mut colors = Colors::default();
    colors[0] = Some(Rgb {
        r: 10,
        g: 20,
        b: 30,
    }); // Black
    colors[2] = Some(Rgb {
        r: 30,
        g: 200,
        b: 40,
    }); // Green
    colors[9] = Some(Rgb {
        r: 99,
        g: 111,
        b: 123,
    }); // BrightRed
    colors[100] = Some(Rgb { r: 7, g: 8, b: 9 });
    colors[200] = Some(Rgb {
        r: 11,
        g: 22,
        b: 33,
    });
    colors[256] = Some(Rgb {
        r: 200,
        g: 210,
        b: 220,
    }); // Foreground
    colors[257] = Some(Rgb {
        r: 20,
        g: 30,
        b: 40,
    }); // Background
    colors
}

fn rgba(r: u8, g: u8, b: u8) -> [u8; 4] {
    [r, g, b, 255]
}

#[test]
fn cell_style_plain_resolves_default_fg_and_bg() {
    let style: CellStyle = cell_style(
        Flags::empty(),
        Color::Named(NamedColor::Foreground),
        Color::Named(NamedColor::Background),
        &palette(),
    );
    assert_eq!(style.fg, DEFAULT_FG);
    assert_eq!(style.bg, DEFAULT_BG);
    assert!(!style.bold);
    assert!(!style.italic);
    assert!(!style.faint);
    assert!(!style.conceal);
}

#[test]
fn cell_style_bold_sets_flag() {
    let style = cell_style(
        Flags::BOLD,
        Color::Spec(Rgb { r: 1, g: 2, b: 3 }),
        Color::Spec(Rgb { r: 4, g: 5, b: 6 }),
        &palette(),
    );
    assert!(style.bold);
    assert!(!style.italic);
    assert!(!style.faint);
    assert!(!style.conceal);
    assert_eq!(style.fg, rgba(1, 2, 3));
    assert_eq!(style.bg, rgba(4, 5, 6));
}

#[test]
fn cell_style_italic_sets_flag() {
    let style = cell_style(
        Flags::ITALIC,
        Color::Spec(Rgb { r: 1, g: 2, b: 3 }),
        Color::Spec(Rgb { r: 4, g: 5, b: 6 }),
        &palette(),
    );
    assert!(style.italic);
    assert!(!style.bold);
    assert!(!style.faint);
    assert!(!style.conceal);
    assert_eq!(style.fg, rgba(1, 2, 3));
    assert_eq!(style.bg, rgba(4, 5, 6));
}

#[test]
fn cell_style_dim_blends_fg_toward_bg() {
    let style = cell_style(
        Flags::DIM,
        Color::Spec(Rgb {
            r: 100,
            g: 150,
            b: 200,
        }),
        Color::Spec(Rgb {
            r: 0,
            g: 50,
            b: 100,
        }),
        &palette(),
    );
    assert!(style.faint);
    assert!(!style.bold);
    assert!(!style.italic);
    assert!(!style.conceal);
    // 50% blend per channel: (100, 150, 200) toward (0, 50, 100).
    assert_eq!(style.fg, rgba(50, 100, 150));
    assert_eq!(style.bg, rgba(0, 50, 100));
}

#[test]
fn cell_style_inverse_swaps_fg_and_bg() {
    let style = cell_style(
        Flags::INVERSE,
        Color::Spec(Rgb {
            r: 10,
            g: 20,
            b: 30,
        }),
        Color::Spec(Rgb {
            r: 40,
            g: 50,
            b: 60,
        }),
        &palette(),
    );
    assert!(!style.bold);
    assert!(!style.italic);
    assert!(!style.faint);
    assert!(!style.conceal);
    assert_eq!(style.fg, rgba(40, 50, 60));
    assert_eq!(style.bg, rgba(10, 20, 30));
}

#[test]
fn cell_style_hidden_sets_conceal() {
    let style = cell_style(
        Flags::HIDDEN,
        Color::Spec(Rgb { r: 1, g: 2, b: 3 }),
        Color::Spec(Rgb { r: 4, g: 5, b: 6 }),
        &palette(),
    );
    assert!(style.conceal);
    assert!(!style.bold);
    assert!(!style.italic);
    assert!(!style.faint);
    assert_eq!(style.fg, rgba(1, 2, 3));
    assert_eq!(style.bg, rgba(4, 5, 6));
}

#[test]
fn cell_style_named_color_resolves_through_table() {
    let style = cell_style(
        Flags::empty(),
        Color::Named(NamedColor::Green),
        Color::Named(NamedColor::Black),
        &palette(),
    );
    assert_eq!(style.fg, rgba(30, 200, 40));
    assert_eq!(style.bg, rgba(10, 20, 30));
}

#[test]
fn cell_style_indexed_color_resolves_flat() {
    let style = cell_style(
        Flags::empty(),
        Color::Indexed(200),
        Color::Indexed(100),
        &palette(),
    );
    // Flat direct index: no +16 offset.
    assert_eq!(style.fg, rgba(11, 22, 33));
    assert_eq!(style.bg, rgba(7, 8, 9));
}

#[test]
fn cell_style_spec_color_is_used_directly() {
    let style = cell_style(
        Flags::empty(),
        Color::Spec(Rgb { r: 1, g: 2, b: 3 }),
        Color::Spec(Rgb { r: 4, g: 5, b: 6 }),
        &palette(),
    );
    assert_eq!(style.fg, rgba(1, 2, 3));
    assert_eq!(style.bg, rgba(4, 5, 6));
}

#[test]
fn cell_style_none_entry_falls_back_to_default() {
    let style = cell_style(
        Flags::empty(),
        Color::Named(NamedColor::Magenta),
        Color::Indexed(101),
        &palette(),
    );
    // Neither entry is set in the palette: fg falls back to index 256,
    // bg to index 257.
    assert_eq!(style.fg, DEFAULT_FG);
    assert_eq!(style.bg, DEFAULT_BG);
}

#[test]
fn cell_style_bold_named_fg_applies_to_bright() {
    let style = cell_style(
        Flags::BOLD,
        Color::Named(NamedColor::Red),
        Color::Named(NamedColor::Background),
        &palette(),
    );
    assert!(style.bold);
    // Red.to_bright() == BrightRed (index 9), resolved through the table.
    assert_eq!(style.fg, rgba(99, 111, 123));
    assert_eq!(style.bg, DEFAULT_BG);
}
