//! Headless font-stack spike (N01).
//!
//! Proves the pinned glyphon/cosmic-text stack resolves system fonts without
//! a GPU or window. Kept small and reusable by the N02 text adapter.

use glyphon::{Attrs, Family, FontSystem};

#[test]
fn system_font_stack_resolves_monospace() {
    let mut font_system = FontSystem::new();
    let matches = font_system.get_font_matches(&Attrs::new().family(Family::Monospace));
    assert!(
        !matches.is_empty(),
        "no monospace fonts found in the system font stack"
    );
}
