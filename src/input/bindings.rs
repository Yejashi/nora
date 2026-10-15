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
    ReloadConfig,
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
            (K::F5, cs, ReloadConfig),
        ];
        Self {
            entries: table
                .iter()
                .map(|&(key, mods, action)| KeyBinding { key, mods, action })
                .collect(),
        }
    }
}

/// What a configured binding does: bind the chord to an action, or free
/// it (`none`) so the key reaches the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingTarget {
    Action(Action),
    Unbind,
}

/// Parse one configured binding: a chord like `"ctrl+shift+c"` (modifiers
/// `ctrl`, `shift`, `alt`, `super`; one key) and an action name.
pub fn parse_binding(
    keys: &str,
    action: &str,
) -> Result<(KeyCode, ModifiersState, BindingTarget), String> {
    let mut mods = ModifiersState::empty();
    let mut key: Option<KeyCode> = None;
    for part in keys.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "ctrl" | "control" => mods |= ModifiersState::CONTROL,
            "shift" => mods |= ModifiersState::SHIFT,
            "alt" => mods |= ModifiersState::ALT,
            "super" | "logo" | "meta" => mods |= ModifiersState::SUPER,
            name => {
                if key.is_some() {
                    return Err("more than one key in the chord".into());
                }
                key = Some(key_code(name).ok_or_else(|| format!("unknown key '{name}'"))?);
            }
        }
    }
    let key = key.ok_or("the chord has no key")?;
    let target = match action.trim() {
        "none" => BindingTarget::Unbind,
        name => BindingTarget::Action(
            action_named(name).ok_or_else(|| format!("unknown action '{name}'"))?,
        ),
    };
    Ok((key, mods, target))
}

/// Key names accepted in chords.
fn key_code(name: &str) -> Option<KeyCode> {
    use KeyCode as K;
    let letter = |c: char| match c {
        'a' => K::KeyA,
        'b' => K::KeyB,
        'c' => K::KeyC,
        'd' => K::KeyD,
        'e' => K::KeyE,
        'f' => K::KeyF,
        'g' => K::KeyG,
        'h' => K::KeyH,
        'i' => K::KeyI,
        'j' => K::KeyJ,
        'k' => K::KeyK,
        'l' => K::KeyL,
        'm' => K::KeyM,
        'n' => K::KeyN,
        'o' => K::KeyO,
        'p' => K::KeyP,
        'q' => K::KeyQ,
        'r' => K::KeyR,
        's' => K::KeyS,
        't' => K::KeyT,
        'u' => K::KeyU,
        'v' => K::KeyV,
        'w' => K::KeyW,
        'x' => K::KeyX,
        'y' => K::KeyY,
        _ => K::KeyZ,
    };
    let digit = [
        K::Digit0,
        K::Digit1,
        K::Digit2,
        K::Digit3,
        K::Digit4,
        K::Digit5,
        K::Digit6,
        K::Digit7,
        K::Digit8,
        K::Digit9,
    ];
    let function = [
        K::F1,
        K::F2,
        K::F3,
        K::F4,
        K::F5,
        K::F6,
        K::F7,
        K::F8,
        K::F9,
        K::F10,
        K::F11,
        K::F12,
    ];
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_lowercase() {
            return Some(letter(c));
        }
        if let Some(d) = c.to_digit(10) {
            return Some(digit[d as usize]);
        }
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<usize>().ok())
        && (1..=12).contains(&n)
    {
        return Some(function[n - 1]);
    }
    Some(match name {
        "enter" | "return" => K::Enter,
        "tab" => K::Tab,
        "space" => K::Space,
        "escape" | "esc" => K::Escape,
        "backspace" => K::Backspace,
        "delete" => K::Delete,
        "insert" => K::Insert,
        "home" => K::Home,
        "end" => K::End,
        "pageup" => K::PageUp,
        "pagedown" => K::PageDown,
        "left" => K::ArrowLeft,
        "right" => K::ArrowRight,
        "up" => K::ArrowUp,
        "down" => K::ArrowDown,
        "minus" | "-" => K::Minus,
        "equal" | "plus" | "=" => K::Equal,
        "backslash" | "\\" => K::Backslash,
        "slash" | "/" => K::Slash,
        "comma" | "," => K::Comma,
        "period" | "." => K::Period,
        "semicolon" | ";" => K::Semicolon,
        "bracketleft" | "[" => K::BracketLeft,
        "bracketright" | "]" => K::BracketRight,
        _ => return None,
    })
}

/// Action names accepted in the config.
fn action_named(name: &str) -> Option<Action> {
    Some(match name {
        "copy" => Action::Copy,
        "paste" => Action::Paste,
        "new_tab" => Action::NewTab,
        "next_tab" => Action::NextTab,
        "previous_tab" | "prev_tab" => Action::PrevTab,
        "close_pane" => Action::ClosePane,
        "split_right" => Action::SplitRight,
        "split_down" => Action::SplitDown,
        "focus_left" => Action::FocusPane(Direction::Left),
        "focus_right" => Action::FocusPane(Direction::Right),
        "focus_up" => Action::FocusPane(Direction::Up),
        "focus_down" => Action::FocusPane(Direction::Down),
        "search" => Action::ToggleSearch,
        "zoom_in" => Action::ZoomIn,
        "zoom_out" => Action::ZoomOut,
        "zoom_reset" => Action::ZoomReset,
        "scroll_page_up" => Action::ScrollPageUp,
        "scroll_page_down" => Action::ScrollPageDown,
        "scroll_to_top" => Action::ScrollToTop,
        "scroll_to_bottom" => Action::ScrollToBottom,
        "reload_config" => Action::ReloadConfig,
        _ => return None,
    })
}

impl Bindings {
    /// The defaults with configured `(keys, action)` entries applied in
    /// order: a chord's last entry wins, and `none` removes the chord.
    /// Entries that do not parse are skipped (config validation reports
    /// them before this runs).
    pub fn with_config(entries: &[(String, String)]) -> Self {
        let mut table = Self::default();
        for (keys, action) in entries {
            let Ok((key, mods, target)) = parse_binding(keys, action) else {
                continue;
            };
            table.entries.retain(|b| !(b.key == key && b.mods == mods));
            if let BindingTarget::Action(action) = target {
                table.entries.insert(0, KeyBinding { key, mods, action });
            }
        }
        table
    }

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
    fn configured_bindings_override_and_unbind() {
        let entries = vec![
            ("ctrl+shift+c".to_string(), "none".to_string()),
            ("alt+t".to_string(), "new_tab".to_string()),
            ("ctrl+shift+f".to_string(), "copy".to_string()),
        ];
        let b = Bindings::with_config(&entries);
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(b.lookup(&key(KeyCode::KeyC), cs), None, "unbound");
        assert_eq!(
            b.lookup(&key(KeyCode::KeyT), ModifiersState::ALT),
            Some(Action::NewTab)
        );
        assert_eq!(b.lookup(&key(KeyCode::KeyF), cs), Some(Action::Copy));
        assert_eq!(b.lookup(&key(KeyCode::KeyV), cs), Some(Action::Paste));
    }

    #[test]
    fn binding_parse_errors_name_the_problem() {
        assert!(parse_binding("ctrl+shift+c", "copy").is_ok());
        assert!(parse_binding("Ctrl + PageUp", "previous_tab").is_ok());
        assert!(
            parse_binding("ctrl+f13", "copy")
                .unwrap_err()
                .contains("unknown key")
        );
        assert!(
            parse_binding("ctrl+a+b", "copy")
                .unwrap_err()
                .contains("more than one")
        );
        assert!(
            parse_binding("ctrl+shift", "copy")
                .unwrap_err()
                .contains("no key")
        );
        assert!(
            parse_binding("ctrl+a", "explode")
                .unwrap_err()
                .contains("unknown action")
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
