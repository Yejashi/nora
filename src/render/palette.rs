//! The base color palette: every slot of the engine's color table.
//!
//! alacritty_terminal keeps only the colors an application *changed* (OSC 4,
//! 10, 11, 12); every other slot of `Term::colors()` is `None` and the
//! frontend supplies the value. [`Palette`] is that frontend table: the 16
//! ANSI colors, the xterm 6x6x6 cube and gray ramp, the default foreground,
//! background and cursor, and the dim variants. [`Palette::overlay`] applies
//! a snapshot's runtime overrides on top, so the renderer always resolves
//! against a fully populated table.

use alacritty_terminal::term::color::{COUNT, Colors};
use alacritty_terminal::vte::ansi::{NamedColor, Rgb};

use crate::config::{NoraConfig, ThemeConfig};

/// xterm's default 16 ANSI colors (normal 0-7, bright 8-15).
const XTERM_16: [Rgb; 16] = [
    rgb(0x00, 0x00, 0x00),
    rgb(0xcd, 0x00, 0x00),
    rgb(0x00, 0xcd, 0x00),
    rgb(0xcd, 0xcd, 0x00),
    rgb(0x00, 0x00, 0xee),
    rgb(0xcd, 0x00, 0xcd),
    rgb(0x00, 0xcd, 0xcd),
    rgb(0xe5, 0xe5, 0xe5),
    rgb(0x7f, 0x7f, 0x7f),
    rgb(0xff, 0x00, 0x00),
    rgb(0x00, 0xff, 0x00),
    rgb(0xff, 0xff, 0x00),
    rgb(0x5c, 0x5c, 0xff),
    rgb(0xff, 0x00, 0xff),
    rgb(0x00, 0xff, 0xff),
    rgb(0xff, 0xff, 0xff),
];

/// Channel levels of the xterm 6x6x6 color cube (indices 16-231).
const CUBE_LEVELS: [u8; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];

/// Dim colors are the normal colors scaled by this factor (alacritty's
/// convention for `NamedColor::Dim*`).
const DIM_FACTOR: f32 = 0.66;

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

fn dim(c: Rgb) -> Rgb {
    let scale = |v: u8| (f32::from(v) * DIM_FACTOR).round() as u8;
    rgb(scale(c.r), scale(c.g), scale(c.b))
}

/// Parse a `#RRGGBB` string. `None` for anything else.
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(rgb(channel(0)?, channel(2)?, channel(4)?))
}

/// A fully populated color table (all [`COUNT`] slots).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    slots: [Rgb; COUNT],
}

impl Default for Palette {
    fn default() -> Self {
        Self::from_theme(&ThemeConfig::default())
    }
}

impl Palette {
    /// Build the palette from a validated theme: xterm defaults, with the
    /// theme's foreground, background, cursor and up to 16 ANSI colors on
    /// top. Malformed strings (rejected by config validation) fall back to
    /// the built-in value.
    pub fn from_theme(theme: &ThemeConfig) -> Self {
        let mut slots = [rgb(0, 0, 0); COUNT];
        slots[..16].copy_from_slice(&XTERM_16);
        for (slot, value) in slots.iter_mut().zip(theme.colors.iter()).take(16) {
            if let Some(color) = parse_hex(value) {
                *slot = color;
            }
        }
        for r in 0..6 {
            for g in 0..6 {
                for b in 0..6 {
                    slots[16 + r * 36 + g * 6 + b] =
                        rgb(CUBE_LEVELS[r], CUBE_LEVELS[g], CUBE_LEVELS[b]);
                }
            }
        }
        for i in 0..24 {
            let level = 8 + 10 * i as u8;
            slots[232 + i] = rgb(level, level, level);
        }
        let fg = parse_hex(&theme.foreground).unwrap_or(rgb(0xff, 0xff, 0xff));
        let bg = parse_hex(&theme.background).unwrap_or(rgb(0, 0, 0));
        let cursor = parse_hex(&theme.cursor).unwrap_or(fg);
        slots[NamedColor::Foreground as usize] = fg;
        slots[NamedColor::Background as usize] = bg;
        slots[NamedColor::Cursor as usize] = cursor;
        for i in 0..8 {
            slots[NamedColor::DimBlack as usize + i] = dim(slots[i]);
        }
        slots[NamedColor::BrightForeground as usize] = fg;
        slots[NamedColor::DimForeground as usize] = dim(fg);
        Self { slots }
    }

    /// The palette for a whole config: the legacy `[theme]` first, then
    /// `[colors]` on top (foreground, background, cursor, normal 0-7,
    /// bright 8-15). Dim variants follow the final normal colors.
    pub fn from_config(config: &NoraConfig) -> Self {
        let mut palette = Self::from_theme(&config.theme);
        let colors = &config.colors;
        let set = |slots: &mut [Rgb; COUNT], index: usize, value: &Option<String>| {
            if let Some(color) = value.as_deref().and_then(parse_hex) {
                slots[index] = color;
            }
        };
        set(
            &mut palette.slots,
            NamedColor::Foreground as usize,
            &colors.foreground,
        );
        set(
            &mut palette.slots,
            NamedColor::Background as usize,
            &colors.background,
        );
        set(
            &mut palette.slots,
            NamedColor::Cursor as usize,
            &colors.cursor,
        );
        for (i, (_, value)) in colors.normal.entries().iter().enumerate() {
            set(&mut palette.slots, i, value);
        }
        for (i, (_, value)) in colors.bright.entries().iter().enumerate() {
            set(&mut palette.slots, 8 + i, value);
        }
        for i in 0..8 {
            palette.slots[NamedColor::DimBlack as usize + i] = dim(palette.slots[i]);
        }
        let fg = palette.foreground();
        palette.slots[NamedColor::BrightForeground as usize] = fg;
        palette.slots[NamedColor::DimForeground as usize] = dim(fg);
        palette
    }

    /// One slot's color.
    pub fn get(&self, index: usize) -> Rgb {
        self.slots[index]
    }

    /// The default foreground.
    pub fn foreground(&self) -> Rgb {
        self.slots[NamedColor::Foreground as usize]
    }

    /// The default background.
    pub fn background(&self) -> Rgb {
        self.slots[NamedColor::Background as usize]
    }

    /// A complete color table: the snapshot's runtime overrides (colors an
    /// application set with OSC 4/10/11/12) where present, this palette
    /// everywhere else. Every slot of the result is `Some`.
    pub fn overlay(&self, overrides: &Colors) -> Colors {
        let mut table = Colors::default();
        for (i, base) in self.slots.iter().enumerate() {
            table[i] = Some(overrides[i].unwrap_or(*base));
        }
        table
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_palette_has_white_on_black_and_xterm_colors() {
        let p = Palette::default();
        assert_eq!(p.foreground(), rgb(0xff, 0xff, 0xff));
        assert_eq!(p.background(), rgb(0, 0, 0));
        assert_eq!(p.get(1), rgb(0xcd, 0, 0));
        // Cube corner and a mid entry, gray ramp ends.
        assert_eq!(p.get(16), rgb(0, 0, 0));
        assert_eq!(p.get(231), rgb(0xff, 0xff, 0xff));
        assert_eq!(p.get(16 + 36 + 12 + 3), rgb(0x5f, 0x87, 0xaf));
        assert_eq!(p.get(232), rgb(8, 8, 8));
        assert_eq!(p.get(255), rgb(238, 238, 238));
    }

    #[test]
    fn theme_overrides_named_and_ansi_slots() {
        let theme = ThemeConfig {
            background: "#101010".into(),
            foreground: "#c0c0c0".into(),
            cursor: "#ff8800".into(),
            colors: vec!["#111111".into(), "#220000".into()],
        };
        let p = Palette::from_theme(&theme);
        assert_eq!(p.background(), rgb(0x10, 0x10, 0x10));
        assert_eq!(p.foreground(), rgb(0xc0, 0xc0, 0xc0));
        assert_eq!(p.get(NamedColor::Cursor as usize), rgb(0xff, 0x88, 0));
        assert_eq!(p.get(0), rgb(0x11, 0x11, 0x11));
        assert_eq!(p.get(1), rgb(0x22, 0, 0));
        // Unspecified ANSI slots keep the xterm default.
        assert_eq!(p.get(2), rgb(0, 0xcd, 0));
        assert_eq!(p.get(NamedColor::DimRed as usize), dim(rgb(0x22, 0, 0)));
    }

    #[test]
    fn overlay_fills_every_slot_and_keeps_runtime_overrides() {
        let p = Palette::default();
        let mut overrides = Colors::default();
        overrides[4] = Some(rgb(1, 2, 3));
        let table = p.overlay(&overrides);
        for i in 0..COUNT {
            assert!(table[i].is_some(), "slot {i} unset");
        }
        assert_eq!(table[4], Some(rgb(1, 2, 3)));
        assert_eq!(table[5], Some(p.get(5)));
    }

    /// Regression (first-use defect): with the engine's all-`None` table,
    /// default-colored text resolved to black on the black background.
    /// Through the palette overlay it resolves to the theme foreground.
    #[test]
    fn default_text_is_visible_through_the_overlay() {
        use crate::render::style::cell_style;
        use alacritty_terminal::term::cell::Flags;
        use alacritty_terminal::vte::ansi::Color;

        let table = Palette::default().overlay(&Colors::default());
        let style = cell_style(
            Flags::empty(),
            Color::Named(NamedColor::Foreground),
            Color::Named(NamedColor::Background),
            &table,
        );
        assert_eq!(style.fg, [0xff, 0xff, 0xff, 255]);
        assert_eq!(style.bg, [0, 0, 0, 255]);
        let red = cell_style(
            Flags::empty(),
            Color::Named(NamedColor::Red),
            Color::Indexed(46),
            &table,
        );
        assert_eq!(red.fg, [0xcd, 0, 0, 255]);
        assert_eq!(red.bg, [0, 0xff, 0, 255]);
    }

    #[test]
    fn colors_section_overrides_the_legacy_theme() {
        let mut config = NoraConfig::default();
        config.theme.background = "#101010".into();
        config.theme.colors = vec!["#111111".into()];
        config.colors.background = Some("#202020".into());
        config.colors.normal.red = Some("#aa0000".into());
        config.colors.bright.blue = Some("#0000ff".into());
        let p = Palette::from_config(&config);
        assert_eq!(p.background(), rgb(0x20, 0x20, 0x20));
        assert_eq!(
            p.get(0),
            rgb(0x11, 0x11, 0x11),
            "theme still applies where colors is unset"
        );
        assert_eq!(p.get(1), rgb(0xaa, 0, 0));
        assert_eq!(p.get(12), rgb(0, 0, 0xff));
        assert_eq!(p.get(NamedColor::DimRed as usize), dim(rgb(0xaa, 0, 0)));
    }

    #[test]
    fn parse_hex_rejects_malformed() {
        assert_eq!(parse_hex("#a0B1c2"), Some(rgb(0xa0, 0xb1, 0xc2)));
        for bad in ["", "#", "a0b1c2", "#a0b1c", "#a0b1c2d", "#g0b1c2", "#ä0b1c"] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
    }
}
