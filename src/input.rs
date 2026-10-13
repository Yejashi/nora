//! N09a: pure keyboard encoding.
//!
//! Translates one winit key event (plain fields, no `KeyEvent`) into
//! terminal bytes or a local action, with no terminal state involved, so it
//! is headless-testable. The window layer (N09b) filters to
//! `ElementState::Pressed`, non-synthetic `KeyboardInput` events and calls
//! [`encode`] with the latest `ModifiersState`.

use alacritty_terminal::term::TermMode;
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

/// What a processed key event does to the terminal (N09a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    /// Send these bytes to the PTY in order.
    Send(Vec<u8>),
    /// Quit the terminal (a local binding fired).
    Quit,
}

/// Local terminal bindings. Default: all enabled.
///
/// Bindings key on the *physical* key, so they stay independent of the
/// keyboard layout's text output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BindingTable {
    /// Ctrl+Q quits (XOFF convention).
    pub ctrl_q_quit: bool,
}

impl Default for BindingTable {
    fn default() -> Self {
        Self { ctrl_q_quit: true }
    }
}

/// Encode one winit key event into terminal bytes (N09a).
///
/// `logical` is the event's layout text / named key, `physical` the
/// layout-independent key code, and `mods` the modifier state at the time
/// of the event.
///
/// `repeat` is accepted for the pinned 6-arg shape; key repeat does not
/// change the encoding. `modes` is accepted for N10+ (mode-dependent
/// input); N09 rows are mode-independent.
///
/// Returns `None` for keys with no N09 meaning (F-keys, arrows,
/// Home/End/PgUp/PgDn, Insert, Delete — N10; `Dead`/`Unidentified`).
pub fn encode(
    logical: &Key,
    physical: &PhysicalKey,
    mods: &ModifiersState,
    repeat: bool,
    bindings: &BindingTable,
    modes: TermMode,
) -> Option<InputAction> {
    // `repeat` and `modes` may become encoding-relevant in N10+; N09 rows
    // are mode-independent and key repeat does not change the bytes.
    let _ = (repeat, modes);

    // Local bindings key on the physical key, ahead of layout decoding,
    // so they fire regardless of the layout text (or lack of one).
    if mods.control_key() && *physical == PhysicalKey::Code(KeyCode::KeyQ) && bindings.ctrl_q_quit {
        return Some(InputAction::Quit);
    }

    match logical {
        Key::Character(text) => {
            if mods.control_key() {
                // Ctrl+letter → 0x01–0x1a (case-insensitive), Ctrl+Space →
                // 0x00. A single keypress carries one codepoint.
                if let Some(byte) = text.chars().next().and_then(ctrl_byte) {
                    return Some(InputAction::Send(vec![byte]));
                }
                // Unpinned in the N09 block (Ctrl + non-letter character):
                // send the logical character's own bytes.
                Some(InputAction::Send(text.as_bytes().to_vec()))
            } else if mods.alt_key() {
                // Alt+character → ESC prefix (xterm meta convention).
                let mut out = Vec::with_capacity(1 + text.len());
                out.push(0x1b);
                out.extend_from_slice(text.as_bytes());
                Some(InputAction::Send(out))
            } else {
                // Plain character → its UTF-8 bytes. Shift+letter arrives
                // as the shifted character and sends those bytes.
                Some(InputAction::Send(text.as_bytes().to_vec()))
            }
        }
        Key::Named(name) => match name {
            NamedKey::Enter => Some(InputAction::Send(vec![0x0d])),
            NamedKey::Tab => Some(InputAction::Send(vec![0x09])),
            // The raw slave keeps the kernel `erase` (0x7f); NOT
            // `Key::to_text()`'s `\x08`.
            NamedKey::Backspace => Some(InputAction::Send(vec![0x7f])),
            NamedKey::Escape => Some(InputAction::Send(vec![0x1b])),
            NamedKey::Space => {
                // Ctrl+Space → NUL (0x00); plain Space → 0x20.
                if mods.control_key() {
                    Some(InputAction::Send(vec![0x00]))
                } else {
                    Some(InputAction::Send(vec![0x20]))
                }
            }
            // F-keys, arrows, Home/End/PgUp/PgDn, Insert, Delete,
            // modifier-only Named presses: no N09 bytes (N10).
            _ => None,
        },
        // Dead keys are never bytes in v0.1 (IME duplication rule);
        // Unidentified has no layout text to encode.
        Key::Dead(_) | Key::Unidentified(_) => None,
    }
}

/// The control byte for Ctrl+`c` (0x00 for space, 0x01–0x1a for letters,
/// case-insensitive), or `None` for characters with no N09 Ctrl byte.
fn ctrl_byte(c: char) -> Option<u8> {
    let lower = c.to_ascii_lowercase();
    if lower.is_ascii_lowercase() {
        let byte = u8::try_from(lower).expect("ASCII lowercase fits in u8");
        Some(byte - b'a' + 1)
    } else if lower == ' ' {
        Some(0x00)
    } else {
        None
    }
}
