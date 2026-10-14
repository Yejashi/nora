//! N09a: pure keyboard encoding.
//!
//! Translates one winit key event (plain fields, no `KeyEvent`) into
//! terminal bytes or a local action, with no terminal state involved, so it
//! is headless-testable. The window layer (N09b) filters to
//! `ElementState::Pressed`, non-synthetic `KeyboardInput` events and calls
//! [`encode`] with the latest `ModifiersState`.

pub mod focus;
pub mod mouse;
pub mod paste;

use crate::engine::KittyKeyboardFlags;
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
/// Returns `None` for keys with no meaning in v0.1 (F13+ — deferred to a
/// future slice; `Dead`; modifier-only presses). F1-F12, the
/// cursor/navigation keys, and the numpad keys are encoded (N10a/N10b).
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
            // N10b: numpad digit / decimal / comma / equal / operator.
            // The physical `Numpad*` scancode is authoritative (winit folds
            // the logical key to `Character`); it wins over the Ctrl / Alt
            // / plain branches below (Shift/Ctrl add no encoding — the plain
            // sequence is sent; Alt prefixes an `ESC`).
            if let Some(key) = keypad_key(physical, mods, modes) {
                return Some(key);
            }
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
            // N10b: numpad Enter / Home / End (physical `NumpadEnter`/
            // `Numpad7`/`Numpad1` disambiguates from the main row; winit folds
            // KP_Enter/KP_Home/KP_End to `Enter`/`Home`/`End`). App mode =
            // `APP_CURSOR || APP_KEYPAD` (decision N10-2).
            NamedKey::Enter if *physical == PhysicalKey::Code(KeyCode::NumpadEnter) => {
                keypad_key(physical, mods, modes)
            }
            // N10b: numpad Home / End (app positions 7/1) — app: SS3 letter,
            // normal: `None` (no mode-invariant form). Their DIGIT role is the
            // `Character` arm (`keypad_key`).
            NamedKey::Home if *physical == PhysicalKey::Code(KeyCode::Numpad7) => {
                numpad_seq(None, Some(b'H'), mods, modes)
            }
            NamedKey::End if *physical == PhysicalKey::Code(KeyCode::Numpad1) => {
                numpad_seq(None, Some(b'F'), mods, modes)
            }
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
            // Insert/Delete/PgUp/PgDn are mode-invariant.
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
            // N10b: F1-F12 (mode-invariant; F13+ remain `None` — PC keyboards
            // need Ctrl+Fn). Shift/Ctrl add no `1;N~` encoding (N21); Alt
            // prefixes an `ESC` per the N09 rule.
            NamedKey::F1 => alt_prefix(vec![0x1b, b'O', b'P'], mods),
            NamedKey::F2 => alt_prefix(vec![0x1b, b'O', b'Q'], mods),
            NamedKey::F3 => alt_prefix(vec![0x1b, b'O', b'R'], mods),
            NamedKey::F4 => alt_prefix(vec![0x1b, b'O', b'S'], mods),
            NamedKey::F5 => alt_prefix(vec![0x1b, b'[', b'1', b'5', b'~'], mods),
            NamedKey::F6 => alt_prefix(vec![0x1b, b'[', b'1', b'7', b'~'], mods),
            NamedKey::F7 => alt_prefix(vec![0x1b, b'[', b'1', b'8', b'~'], mods),
            NamedKey::F8 => alt_prefix(vec![0x1b, b'[', b'1', b'9', b'~'], mods),
            NamedKey::F9 => alt_prefix(vec![0x1b, b'[', b'2', b'0', b'~'], mods),
            NamedKey::F10 => alt_prefix(vec![0x1b, b'[', b'2', b'1', b'~'], mods),
            NamedKey::F11 => alt_prefix(vec![0x1b, b'[', b'2', b'3', b'~'], mods),
            NamedKey::F12 => alt_prefix(vec![0x1b, b'[', b'2', b'4', b'~'], mods),
            _ => None,
        },
        // Dead keys are never bytes in v0.1 (IME duplication rule).
        Key::Dead(_) => None,
        // N10b: a numpad operator may arrive `Unidentified` (winit folds it
        // to `Unidentified` when xkbcommon has no character for the keysym);
        // the physical `Numpad*` scancode is still authoritative.
        Key::Unidentified(_) => keypad_key(physical, mods, modes),
    }
}

/// Encode one key event using the kitty keyboard protocol (N21b).
///
/// `kitty_flags` is the protocol's negotiated flag set (the top of the
/// engine's mode stack; `KittyKeyboardFlags::NONE` = legacy mode). When
/// the protocol is disabled the function returns `None` and the caller
/// falls back to the legacy [`encode`]; otherwise the bytes are:
///
/// - the key code (2 bytes, big-endian) — always;
/// - the event type (1 byte: 1 = down, 2 = repeat, 3 = up) — only when
///   `REPORT_EVENTS` is negotiated;
/// - the system modifiers (1 byte: shift = 1, ctrl = 2, alt = 4,
///   super/logo = 8) — only when `SYSTEM` is negotiated.
///
/// The window layer filters to `Pressed` events, so `repeat` selects
/// down (1) vs repeat (2); up (3) is reserved for a future release
/// event.
pub fn encode_kitty(
    key: &Key,
    physical: &PhysicalKey,
    mods: &ModifiersState,
    repeat: bool,
    kitty_flags: KittyKeyboardFlags,
) -> Option<Vec<u8>> {
    // The encoding is physical-key driven; the logical key is accepted
    // for API symmetry with the legacy `encode`.
    let _ = key;
    if kitty_flags == KittyKeyboardFlags::NONE {
        return None;
    }

    let mut bytes = Vec::new();

    // 1. The key code (2 bytes, big-endian).
    let key_code = key_code_from_physical(physical);
    bytes.extend_from_slice(&key_code.to_be_bytes());

    // 2. The event type (1 byte), only when REPORT_EVENTS is negotiated.
    if kitty_flags.contains(KittyKeyboardFlags::REPORT_EVENTS) {
        let event_type = if repeat { 2 } else { 1 }; // 1 = down, 2 = repeat, 3 = up
        bytes.push(event_type);
    }

    // 3. The system modifiers (1 byte), only when SYSTEM is negotiated.
    if kitty_flags.contains(KittyKeyboardFlags::SYSTEM) {
        bytes.push(system_mods_from_modifiers(mods));
    }

    Some(bytes)
}

/// The kitty key code for a physical key (N21b): the winit `KeyCode`
/// discriminant for `Code` physicals, 0 for `Unidentified` physicals
/// (the kitty convention for keys without a code).
fn key_code_from_physical(physical: &PhysicalKey) -> u16 {
    match physical {
        PhysicalKey::Code(code) => *code as u16,
        PhysicalKey::Unidentified(_) => 0,
    }
}

/// The system modifier byte for a winit modifier state (N21b):
/// shift = 1, ctrl = 2, alt = 4, super/logo = 8 (OR-combined).
fn system_mods_from_modifiers(mods: &ModifiersState) -> u8 {
    let mut sys_mods = 0u8;
    if mods.shift_key() {
        sys_mods |= 1;
    }
    if mods.control_key() {
        sys_mods |= 2;
    }
    if mods.alt_key() {
        sys_mods |= 4;
    }
    if mods.super_key() {
        sys_mods |= 8;
    }
    sys_mods
}

/// N10b: prefix an `ESC` for Alt (per the N09/N10a Alt rule); Shift/Ctrl
/// add no `1;N~` modifier encoding in v0.1 (the plain sequence is sent).
/// Used by the mode-invariant F-key rows.
fn alt_prefix(bytes: Vec<u8>, mods: &ModifiersState) -> Option<InputAction> {
    let mut bytes = bytes;
    if mods.alt_key() {
        bytes.insert(0, 0x1b);
    }
    Some(InputAction::Send(bytes))
}

/// N10b numpad resolver: `app` = `APP_CURSOR || APP_KEYPAD` (decision
/// N10-2). App mode sends the SS3 letter (`ESC O <letter>`); normal mode
/// sends the plain byte (`None` for the numpad Home/End, which have no
/// mode-invariant form). Shift/Ctrl add no `1;N~` encoding (the plain
/// sequence is sent); Alt prefixes an `ESC` per the N09/N10a rule.
fn numpad_seq(
    plain: Option<u8>,
    ss3: Option<u8>,
    mods: &ModifiersState,
    modes: TermMode,
) -> Option<InputAction> {
    let app = modes.contains(TermMode::APP_CURSOR) || modes.contains(TermMode::APP_KEYPAD);
    let mut bytes = if app {
        ss3.map(|letter| vec![0x1b, b'O', letter])?
    } else {
        plain.map(|byte| vec![byte])?
    };
    if mods.alt_key() {
        bytes.insert(0, 0x1b);
    }
    Some(InputAction::Send(bytes))
}

/// N10b numpad keys, resolved by the authoritative physical `Numpad*`
/// scancode (winit folds the logical key to `Character`, or to
/// `Enter`/`Home`/`End` for KP_Enter/KP_Home/KP_End). Returns the
/// (normal byte, app SS3 letter) for each; `None` for non-numpad physicals
/// (so main-row keys fall through to the `Character` / `Named` handling).
/// SS3 letters pinned from the xterm ctlseqs numeric + editing keypads:
/// 0-9 = `p`-`y` (2625-2649), Home/End = `H`/`F` (2492-2527), `.` = `n`,
/// Enter = `M`, `+`/`-`/`*`/`/` = `k`/`m`/`j`/`l`, `,` = `l`, `=` = `X`
/// (2625-2649).
fn keypad_key(
    physical: &PhysicalKey,
    mods: &ModifiersState,
    modes: TermMode,
) -> Option<InputAction> {
    let (plain, ss3): (Option<u8>, Option<u8>) = match physical {
        PhysicalKey::Code(KeyCode::Numpad0) => (Some(b'0'), Some(b'p')),
        PhysicalKey::Code(KeyCode::Numpad1) => (Some(b'1'), Some(b'q')),
        PhysicalKey::Code(KeyCode::Numpad2) => (Some(b'2'), Some(b'r')),
        PhysicalKey::Code(KeyCode::Numpad3) => (Some(b'3'), Some(b's')),
        PhysicalKey::Code(KeyCode::Numpad4) => (Some(b'4'), Some(b't')),
        PhysicalKey::Code(KeyCode::Numpad5) => (Some(b'5'), Some(b'u')),
        PhysicalKey::Code(KeyCode::Numpad6) => (Some(b'6'), Some(b'v')),
        PhysicalKey::Code(KeyCode::Numpad7) => (Some(b'7'), Some(b'w')),
        PhysicalKey::Code(KeyCode::Numpad8) => (Some(b'8'), Some(b'x')),
        PhysicalKey::Code(KeyCode::Numpad9) => (Some(b'9'), Some(b'y')),
        PhysicalKey::Code(KeyCode::NumpadDecimal) => (Some(b'.'), Some(b'n')),
        PhysicalKey::Code(KeyCode::NumpadEnter) => (Some(0x0d), Some(b'M')),
        PhysicalKey::Code(KeyCode::NumpadAdd) => (Some(b'+'), Some(b'k')),
        PhysicalKey::Code(KeyCode::NumpadSubtract) => (Some(b'-'), Some(b'm')),
        PhysicalKey::Code(KeyCode::NumpadMultiply) => (Some(b'*'), Some(b'j')),
        PhysicalKey::Code(KeyCode::NumpadDivide) => (Some(b'/'), Some(b'l')),
        PhysicalKey::Code(KeyCode::NumpadComma) => (Some(b','), Some(b'l')),
        PhysicalKey::Code(KeyCode::NumpadEqual) => (Some(b'='), Some(b'X')),
        _ => return None,
    };
    numpad_seq(plain, ss3, mods, modes)
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
