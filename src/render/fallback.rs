//! Per-character font fallback (N13a).
//!
//! [`first_supporting`] returns the first candidate font (in priority order)
//! that can render a character, by consulting each candidate's supported
//! Unicode codepoints. It is pure — no GPU, no shaping — only a codepoint
//! membership test against each candidate [`Font`].

use glyphon::{FontSystem, fontdb};

/// The font identifier type (cosmic-text's [`fontdb::ID`], re-exported through
/// glyphon).
pub type FontId = fontdb::ID;

/// Return the first candidate font (in priority order) that supports `ch`.
///
/// Candidates are checked in order; the first whose supported Unicode
/// codepoint list contains `ch` is returned. A candidate that cannot be
/// loaded is skipped. Returns `None` when no candidate supports `ch`.
///
/// `fs` is borrowed mutably because loading a candidate [`Font`] may upgrade a
/// `Source::File` face to a shared face (cosmic-text's
/// `FontSystem::get_font`), which requires `&mut FontSystem`.
pub fn first_supporting(fs: &mut FontSystem, ch: char, candidates: &[FontId]) -> Option<FontId> {
    let codepoint = ch as u32;
    for &id in candidates {
        let Some(font) = fs.get_font(id, fontdb::Weight::NORMAL) else {
            continue;
        };
        if font.unicode_codepoints().contains(&codepoint) {
            return Some(id);
        }
    }
    None
}
