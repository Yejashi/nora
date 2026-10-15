//! Local key bindings: which key chords perform a terminal UI action
//! instead of being sent to the application.
//!
//! Bindings match the *physical* key (layout-independent) plus the exact
//! modifier set. Every default uses Ctrl+Shift (or Shift with a scroll key,
//! or Ctrl with PageUp/PageDown), so the plain Ctrl+letter control
//! characters that shells and editors depend on (Ctrl+D EOF, Ctrl+W delete
//! word, Ctrl+T transpose, Ctrl+F forward-char, Ctrl+X prefix) always reach
//! the application.
//!
//! An action that has nothing to act on (focus a pane where there is no
//! neighbour, switch tabs with a single tab, scroll in the alternate
//! screen) is not consumed: the key is encoded and sent as usual.

use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

/// A pane focus direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// A local terminal action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Copy,
    Paste,
    NewTab,
    NextTab,
    PrevTab,
    ClosePane,
    SplitRight,
    SplitDown,
    FocusPane(Direction),
    ToggleSearch,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToTop,
    ScrollToBottom,
}

impl Action {
    /// Whether holding the chord repeats the action. Non-repeating
    /// actions swallow their auto-repeat events, so a held chord never
    /// leaks control bytes to the application.
    pub fn repeats(self) -> bool {
        matches!(
            self,
            Action::ZoomIn | Action::ZoomOut | Action::ScrollPageUp | Action::ScrollPageDown
        )
    }
}

/// One binding: a physical key, the exact modifiers, the action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBinding {
    pub key: KeyCode,
    pub mods: ModifiersState,
    pub action: Action,
}

/// The binding table, searched in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    entries: Vec<KeyBinding>,
}

/// The modifiers that take part in matching (lock keys never do).
fn significant(mods: ModifiersState) -> ModifiersState {
    mods & (ModifiersState::SHIFT
        | ModifiersState::CONTROL
        | ModifiersState::ALT
        | ModifiersState::SUPER)
}

impl Default for Bindings {
    fn default() -> Self {
        use Action::*;
        use KeyCode as K;
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let c = ModifiersState::CONTROL;
        let s = ModifiersState::SHIFT;
        let table: &[(KeyCode, ModifiersState, Action)] = &[
            (K::KeyC, cs, Copy),
            (K::KeyV, cs, Paste),
            (K::KeyT, cs, NewTab),
            (K::KeyW, cs, ClosePane),
            (K::PageDown, c, NextTab),
            (K::PageUp, c, PrevTab),
            (K::Enter, cs, SplitRight),
            (K::Backslash, cs, SplitDown),
            (K::ArrowLeft, cs, FocusPane(Direction::Left)),
            (K::ArrowRight, cs, FocusPane(Direction::Right)),
            (K::ArrowUp, cs, FocusPane(Direction::Up)),
            (K::ArrowDown, cs, FocusPane(Direction::Down)),
            (K::KeyF, cs, ToggleSearch),
            (K::Equal, cs, ZoomIn),
            (K::NumpadAdd, cs, ZoomIn),
            (K::Minus, cs, ZoomOut),
            (K::NumpadSubtract, cs, ZoomOut),
            (K::Digit0, cs, ZoomReset),
            (K::PageUp, s, ScrollPageUp),
            (K::PageDown, s, ScrollPageDown),
            (K::Home, s, ScrollToTop),
            (K::End, s, ScrollToBottom),
        ];
        Self {
            entries: table
                .iter()
                .map(|&(key, mods, action)| KeyBinding { key, mods, action })
                .collect(),
        }
    }
}

impl Bindings {
    /// A table with no bindings: every key goes to the application.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// The bound entries, in match order.
    pub fn entries(&self) -> &[KeyBinding] {
        &self.entries
    }

    /// The action bound to `physical` with exactly `mods`, if any.
    pub fn lookup(&self, physical: &PhysicalKey, mods: ModifiersState) -> Option<Action> {
        let PhysicalKey::Code(code) = physical else {
            return None;
        };
        let mods = significant(mods);
        self.entries
            .iter()
            .find(|b| b.key == *code && b.mods == mods)
            .map(|b| b.action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> PhysicalKey {
        PhysicalKey::Code(code)
    }

    #[test]
    fn shell_control_characters_are_never_bound() {
        let b = Bindings::default();
        let ctrl = ModifiersState::CONTROL;
        for code in [
            KeyCode::KeyC,
            KeyCode::KeyD,
            KeyCode::KeyF,
            KeyCode::KeyQ,
            KeyCode::KeyT,
            KeyCode::KeyV,
            KeyCode::KeyW,
            KeyCode::KeyX,
            KeyCode::Tab,
            KeyCode::ArrowLeft,
            KeyCode::ArrowRight,
            KeyCode::Equal,
            KeyCode::Minus,
            KeyCode::Digit0,
        ] {
            assert_eq!(
                b.lookup(&key(code), ctrl),
                None,
                "Ctrl+{code:?} must reach the app"
            );
        }
    }

    #[test]
    fn documented_defaults_resolve() {
        let b = Bindings::default();
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(b.lookup(&key(KeyCode::KeyC), cs), Some(Action::Copy));
        assert_eq!(b.lookup(&key(KeyCode::KeyT), cs), Some(Action::NewTab));
        assert_eq!(b.lookup(&key(KeyCode::KeyW), cs), Some(Action::ClosePane));
        assert_eq!(
            b.lookup(&key(KeyCode::KeyF), cs),
            Some(Action::ToggleSearch)
        );
        assert_eq!(
            b.lookup(&key(KeyCode::PageDown), ModifiersState::CONTROL),
            Some(Action::NextTab)
        );
        assert_eq!(
            b.lookup(&key(KeyCode::PageUp), ModifiersState::SHIFT),
            Some(Action::ScrollPageUp)
        );
    }

    #[test]
    fn modifiers_match_exactly_and_ignore_nothing_else() {
        let b = Bindings::default();
        let csa = ModifiersState::CONTROL | ModifiersState::SHIFT | ModifiersState::ALT;
        assert_eq!(b.lookup(&key(KeyCode::KeyC), csa), None);
        assert_eq!(b.lookup(&key(KeyCode::KeyC), ModifiersState::SHIFT), None);
        assert_eq!(
            b.lookup(
                &PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified),
                ModifiersState::empty()
            ),
            None
        );
    }

    #[test]
    fn only_zoom_and_page_scroll_repeat() {
        assert!(Action::ZoomIn.repeats());
        assert!(Action::ScrollPageDown.repeats());
        assert!(!Action::Copy.repeats());
        assert!(!Action::ClosePane.repeats());
        assert!(!Action::NewTab.repeats());
    }
}
