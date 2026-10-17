//! N09a: pure keyboard encoding.
//!
//! Translates one winit key event (plain fields, no `KeyEvent`) into
//! terminal bytes or a local action, with no terminal state involved, so it
//! is headless-testable. The window layer (N09b) filters to
//! `ElementState::Pressed`, non-synthetic `KeyboardInput` events and calls
//! [`encode`] with the latest `ModifiersState`.

pub mod bindings;
pub mod focus;
pub mod kitty;
pub mod mouse;
pub mod paste;

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

/// Encoder-level local bindings.
///
/// `ctrl_q_quit` (off by default) makes Ctrl+Q quit instead of sending
/// XON (0x11); it exists for users who want it, but the default sends the
/// byte, as every other terminal does. UI actions (copy, tabs, zoom, ...)
/// live in [`bindings::Bindings`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct BindingTable {
    /// Ctrl+Q quits instead of sending XON.
    pub ctrl_q_quit: bool,
}

/// Encode one winit key event into terminal bytes, following xterm's
/// "PC-Style Function Keys" rules (also alacritty's): unmodified cursor
/// keys honour DECCKM; a modified cursor, editing or function key carries
/// the xterm modifier parameter (`CSI 1;m X`, `CSI n;m ~`); Alt prefixes
/// ESC on keys without a parameter; Ctrl maps the C0 control set.
///
/// `logical` is the layout's text / named key, `physical` the
/// layout-independent key code, `mods` the modifier state. `repeat` does
/// not change the bytes. Returns `None` for keys with no byte encoding
/// (dead keys, modifier-only presses, F13+).
pub fn encode(
    logical: &Key,
    physical: &PhysicalKey,
    mods: &ModifiersState,
    repeat: bool,
    bindings: &BindingTable,
    modes: TermMode,
) -> Option<InputAction> {
    let _ = repeat;
    if bindings.ctrl_q_quit && mods.control_key() && *physical == PhysicalKey::Code(KeyCode::KeyQ) {
        return Some(InputAction::Quit);
    }
    let send = |bytes: Vec<u8>| Some(InputAction::Send(bytes));
    match logical {
        Key::Character(text) => {
            // Numpad digits/operators: the physical scancode is
            // authoritative (winit folds them to characters).
            if let Some(key) = keypad_key(physical, mods, modes, Some(text)) {
                return Some(key);
            }
            let mut chars = text.chars();
            let single = match (chars.next(), chars.next()) {
                (Some(c), None) => Some(c),
                _ => None,
            };
            if mods.control_key()
                && let Some(byte) =
                    single.and_then(|c| ctrl_byte(c).or_else(|| us_ctrl_byte(c, physical)))
            {
                return send(alt_prefixed(vec![byte], mods));
            }
            send(alt_prefixed(text.as_bytes().to_vec(), mods))
        }
        Key::Named(name) => match name {
            NamedKey::Enter if *physical == PhysicalKey::Code(KeyCode::NumpadEnter) => {
                keypad_key(physical, mods, modes, None)
            }
            NamedKey::Enter => send(alt_prefixed(vec![0x0d], mods)),
            NamedKey::Tab if mods.shift_key() => send(alt_prefixed(b"\x1b[Z".to_vec(), mods)),
            NamedKey::Tab => send(alt_prefixed(vec![0x09], mods)),
            // DEL is the cooked tty's default erase character; Ctrl+
            // Backspace sends BS (0x08), as xterm and alacritty do.
            NamedKey::Backspace if mods.control_key() => send(alt_prefixed(vec![0x08], mods)),
            NamedKey::Backspace => send(alt_prefixed(vec![0x7f], mods)),
            NamedKey::Escape => send(alt_prefixed(vec![0x1b], mods)),
            NamedKey::Space if mods.control_key() => send(alt_prefixed(vec![0x00], mods)),
            NamedKey::Space => send(alt_prefixed(vec![0x20], mods)),
            NamedKey::ArrowUp => send(cursor_key(b'A', mods, modes)),
            NamedKey::ArrowDown => send(cursor_key(b'B', mods, modes)),
            NamedKey::ArrowRight => send(cursor_key(b'C', mods, modes)),
            NamedKey::ArrowLeft => send(cursor_key(b'D', mods, modes)),
            NamedKey::Home => send(cursor_key(b'H', mods, modes)),
            NamedKey::End => send(cursor_key(b'F', mods, modes)),
            NamedKey::Insert => send(tilde_key(2, mods)),
            NamedKey::Delete => send(tilde_key(3, mods)),
            NamedKey::PageUp => send(tilde_key(5, mods)),
            NamedKey::PageDown => send(tilde_key(6, mods)),
            NamedKey::F1 => send(ss3_function_key(b'P', mods)),
            NamedKey::F2 => send(ss3_function_key(b'Q', mods)),
            NamedKey::F3 => send(ss3_function_key(b'R', mods)),
            NamedKey::F4 => send(ss3_function_key(b'S', mods)),
            NamedKey::F5 => send(tilde_key(15, mods)),
            NamedKey::F6 => send(tilde_key(17, mods)),
            NamedKey::F7 => send(tilde_key(18, mods)),
            NamedKey::F8 => send(tilde_key(19, mods)),
            NamedKey::F9 => send(tilde_key(20, mods)),
            NamedKey::F10 => send(tilde_key(21, mods)),
            NamedKey::F11 => send(tilde_key(23, mods)),
            NamedKey::F12 => send(tilde_key(24, mods)),
            _ => None,
        },
        // Dead keys compose through the IME; they are never bytes.
        Key::Dead(_) => None,
        // A numpad operator may arrive `Unidentified` when xkbcommon has
        // no character for the keysym; the scancode is still authoritative.
        Key::Unidentified(_) => keypad_key(physical, mods, modes, None),
    }
}

/// The xterm modifier parameter: 1 + Shift(1) + Alt(2) + Ctrl(4) +
/// Super(8), or `None` when no modifier is held.
fn modifier_param(mods: &ModifiersState) -> Option<u8> {
    let mut bits = 0;
    if mods.shift_key() {
        bits |= 1;
    }
    if mods.alt_key() {
        bits |= 2;
    }
    if mods.control_key() {
        bits |= 4;
    }
    if mods.super_key() {
        bits |= 8;
    }
    (bits != 0).then_some(1 + bits)
}

/// ESC-prefix `bytes` when Alt is held (xterm's metaSendsEscape).
fn alt_prefixed(mut bytes: Vec<u8>, mods: &ModifiersState) -> Vec<u8> {
    if mods.alt_key() {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// Arrow keys and Home/End: `CSI 1;m X` when modified, else `SS3 X` under
/// DECCKM (application cursor keys) and `CSI X` otherwise.
fn cursor_key(final_byte: u8, mods: &ModifiersState, modes: TermMode) -> Vec<u8> {
    match modifier_param(mods) {
        Some(m) => format!("\x1b[1;{m}{}", final_byte as char).into_bytes(),
        None if modes.contains(TermMode::APP_CURSOR) => vec![0x1b, b'O', final_byte],
        None => vec![0x1b, b'[', final_byte],
    }
}

/// Editing and function keys of the `CSI n ~` family: `CSI n;m ~` when
/// modified.
fn tilde_key(code: u8, mods: &ModifiersState) -> Vec<u8> {
    match modifier_param(mods) {
        Some(m) => format!("\x1b[{code};{m}~").into_bytes(),
        None => format!("\x1b[{code}~").into_bytes(),
    }
}

/// F1-F4: `SS3 P..S` unmodified, `CSI 1;m P..S` modified.
fn ss3_function_key(final_byte: u8, mods: &ModifiersState) -> Vec<u8> {
    match modifier_param(mods) {
        Some(m) => format!("\x1b[1;{m}{}", final_byte as char).into_bytes(),
        None => vec![0x1b, b'O', final_byte],
    }
}

/// A numpad key: application keypad (DECKPAM) sends `SS3 p..y` and the
/// operator finals; normal mode sends the layout's `text` for the key
/// (a German keypad decimal gives `,`), or the US character when the
/// layout reports none. Only DECKPAM selects the application form
/// (DECCKM is about cursor keys).
fn keypad_key(
    physical: &PhysicalKey,
    mods: &ModifiersState,
    modes: TermMode,
    text: Option<&str>,
) -> Option<InputAction> {
    let (plain, ss3): (u8, u8) = match physical {
        PhysicalKey::Code(KeyCode::Numpad0) => (b'0', b'p'),
        PhysicalKey::Code(KeyCode::Numpad1) => (b'1', b'q'),
        PhysicalKey::Code(KeyCode::Numpad2) => (b'2', b'r'),
        PhysicalKey::Code(KeyCode::Numpad3) => (b'3', b's'),
        PhysicalKey::Code(KeyCode::Numpad4) => (b'4', b't'),
        PhysicalKey::Code(KeyCode::Numpad5) => (b'5', b'u'),
        PhysicalKey::Code(KeyCode::Numpad6) => (b'6', b'v'),
        PhysicalKey::Code(KeyCode::Numpad7) => (b'7', b'w'),
        PhysicalKey::Code(KeyCode::Numpad8) => (b'8', b'x'),
        PhysicalKey::Code(KeyCode::Numpad9) => (b'9', b'y'),
        PhysicalKey::Code(KeyCode::NumpadDecimal) => (b'.', b'n'),
        PhysicalKey::Code(KeyCode::NumpadEnter) => (0x0d, b'M'),
        PhysicalKey::Code(KeyCode::NumpadAdd) => (b'+', b'k'),
        PhysicalKey::Code(KeyCode::NumpadSubtract) => (b'-', b'm'),
        PhysicalKey::Code(KeyCode::NumpadMultiply) => (b'*', b'j'),
        PhysicalKey::Code(KeyCode::NumpadDivide) => (b'/', b'o'),
        PhysicalKey::Code(KeyCode::NumpadComma) => (b',', b'l'),
        PhysicalKey::Code(KeyCode::NumpadEqual) => (b'=', b'X'),
        _ => return None,
    };
    let bytes = if modes.contains(TermMode::APP_KEYPAD) {
        vec![0x1b, b'O', ss3]
    } else if let Some(text) = text.filter(|text| !text.is_empty()) {
        text.as_bytes().to_vec()
    } else {
        vec![plain]
    };
    Some(InputAction::Send(alt_prefixed(bytes, mods)))
}

/// The control byte for a Ctrl chord whose layout character `c` has none:
/// winit's logical key ignores Ctrl, so on a non-Latin layout Ctrl+C
/// arrives as `с`. A non-ASCII character falls back to the key at the same
/// position on a US layout (as foot and kitty do), so Ctrl+C still sends
/// ETX. ASCII characters keep their own meaning.
fn us_ctrl_byte(c: char, physical: &PhysicalKey) -> Option<u8> {
    if c.is_ascii() {
        return None;
    }
    match physical {
        PhysicalKey::Code(code) => kitty::us_layout(*code).and_then(ctrl_byte),
        PhysicalKey::Unidentified(_) => None,
    }
}

/// The C0 control byte Ctrl+`c` produces, per xterm: letters to 0x01-0x1a,
/// `@`/space/`2` to NUL, `[`/`3` to ESC, `\`/`4` to FS, `]`/`5` to GS,
/// `^`/`~`/`6` to RS, `_`/`/`/`7` to US, `?`/`8` to DEL.
fn ctrl_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        '@' | ' ' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '~' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}
