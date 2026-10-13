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
/// Returns `None` for keys with no meaning in v0.1 (F-keys — N10b;
/// `Dead`/`Unidentified`; modifier-only presses).
pub fn encode(
    logical: &Key,
    physical: &PhysicalKey,
    mods: &ModifiersState,
    repeat: bool,
    bindings: &BindingTable,
    modes: TermMode,
) -> Option<InputAction> {
    // `repeat` may become encoding-relevant in N10+; key repeat does not
    // change the bytes in v0.1. `modes` drives the N10a navigation rows
    // (DECCKM cursor keys); N09 rows remain mode-independent.
    let _ = repeat;

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
            // N10a: cursor-key / core navigation (lead decisions
            // N10-1/N10-2). Arrows + Home/End are DECCKM-conditional;
            // Insert/Delete/PgUp/PgDn are mode-invariant. F-keys stay
            // `None` (N10b); modifier-only Named presses fall through.
            NamedKey::ArrowUp
            | NamedKey::ArrowDown
            | NamedKey::ArrowLeft
            | NamedKey::ArrowRight
            | NamedKey::Home
            | NamedKey::End
            | NamedKey::Insert
            | NamedKey::Delete
            | NamedKey::PageUp
            | NamedKey::PageDown => navigation_key(name, mods, modes),
            _ => None,
        },
        // Dead keys are never bytes in v0.1 (IME duplication rule);
        // Unidentified has no layout text to encode.
        Key::Dead(_) | Key::Unidentified(_) => None,
    }
}

/// N10a: cursor-key / core navigation rows (lead decisions N10-1/N10-2).
///
/// Arrows and Home/End are DECCKM-conditional on [`TermMode::APP_CURSOR`]:
/// normal mode sends `ESC [ <letter>`, application mode sends
/// `ESC O <letter>`. Insert/Delete/PageUp/PageDown are mode-invariant
/// `ESC [ <n>~` (their DECCKM scope excludes them). Shift/Ctrl add no
/// `1;N~` modifier encoding in v0.1 (the plain sequence is sent); Alt
/// prefixes an `ESC`, exactly per the N09 Alt rule for characters.
fn navigation_key(name: &NamedKey, mods: &ModifiersState, modes: TermMode) -> Option<InputAction> {
    let bytes = match name {
        // DECCKM-conditional arrows + Home/End.
        NamedKey::ArrowUp => cursor_key(b'A', modes),
        NamedKey::ArrowDown => cursor_key(b'B', modes),
        NamedKey::ArrowLeft => cursor_key(b'D', modes),
        NamedKey::ArrowRight => cursor_key(b'C', modes),
        NamedKey::Home => cursor_key(b'H', modes),
        NamedKey::End => cursor_key(b'F', modes),
        // Mode-invariant edit keys.
        NamedKey::Insert => vec![0x1b, b'[', b'2', b'~'],
        NamedKey::Delete => vec![0x1b, b'[', b'3', b'~'],
        NamedKey::PageUp => vec![0x1b, b'[', b'5', b'~'],
        NamedKey::PageDown => vec![0x1b, b'[', b'6', b'~'],
        _ => return None,
    };
    // Alt prefixes an ESC (xterm meta convention), mirroring the N09
    // character rule; the base sequence itself is unchanged by Shift/Ctrl.
    if mods.alt_key() {
        let mut out = Vec::with_capacity(1 + bytes.len());
        out.push(0x1b);
        out.extend_from_slice(&bytes);
        Some(InputAction::Send(out))
    } else {
        Some(InputAction::Send(bytes))
    }
}

/// One DECCKM-conditional arrow/Home/End key: `ESC [ <c>` in normal mode,
/// `ESC O <c>` in application mode (`APP_CURSOR`).
fn cursor_key(c: u8, modes: TermMode) -> Vec<u8> {
    if modes.contains(TermMode::APP_CURSOR) {
        vec![0x1b, b'O', c]
    } else {
        vec![0x1b, b'[', c]
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
