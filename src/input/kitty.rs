//! The kitty keyboard protocol (M1.3, N21):
//! <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>.
//!
//! An application enables progressive enhancements with `CSI > flags u`.
//! The engine (alacritty_terminal with `kitty_keyboard` on) keeps the
//! per-screen mode stack, answers `CSI ? u`, and exposes the active flags
//! as [`TermMode`] bits; this module turns one key event into bytes under
//! those flags. All five flags are implemented, since the query reports
//! whatever flags the application set:
//!
//! - 1 disambiguate: Esc, keypad keys, and keys with Ctrl/Alt/Super as
//!   `CSI code;mods u`;
//! - 2 event types: repeat (`:2`) and release (`:3`) of keys sent as
//!   escape codes;
//! - 4 alternate keys: `code:shifted:base-layout`;
//! - 8 all keys as escape codes, modifier keys included;
//! - 16 associated text (with 8): the text as a third field.
//!
//! Where the protocol keeps the legacy bytes (text without Ctrl/Alt/Super,
//! unmodified Enter/Tab/Backspace, an unmodified functional key press),
//! [`encode`] answers [`Kitty::Legacy`], so DECCKM and the keypad modes stay
//! in the legacy encoder. Lock modifiers are never reported: winit does not
//! expose their state.

use alacritty_terminal::term::TermMode;
use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

/// The kind of a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventKind {
    Press,
    Repeat,
    Release,
}

/// One key event, as the protocol needs it.
#[derive(Debug, Clone)]
pub struct KeyEvent<'a> {
    /// The key with the layout and modifiers applied.
    pub logical: &'a Key,
    /// The same key without modifiers (`key_without_modifiers`).
    pub unshifted: &'a Key,
    pub physical: PhysicalKey,
    pub location: KeyLocation,
    pub mods: ModifiersState,
    pub kind: KeyEventKind,
    /// The text the key produces, if any.
    pub text: Option<&'a str>,
}

/// What to send for a key event under the kitty protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kitty {
    /// These bytes.
    Send(Vec<u8>),
    /// The legacy encoding of the key (press and repeat only).
    Legacy,
    /// Nothing (a release that is not reported, a modifier key).
    Nothing,
}

/// How a key is identified on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
    /// A text key: its unshifted codepoint, as `CSI code u`.
    Text(u32),
    /// Escape, Enter, Tab, Backspace (`CSI code u`, legacy bytes when
    /// allowed).
    Control(u32),
    /// A key with a protocol number, `CSI code u` (keypad, F13+, media,
    /// modifier keys).
    Numbered(u32),
    /// A key in its legacy form: `CSI 1;mods X` or `CSI n;mods ~`.
    Letter(u8),
    Tilde(u32),
}

/// Whether any kitty keyboard flag is active.
pub fn active(modes: TermMode) -> bool {
    modes.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL)
}

/// Encode `event` under the active kitty flags in `modes` (at least one
/// must be set, see [`active`]).
pub fn encode(event: &KeyEvent, modes: TermMode) -> Kitty {
    let all = modes.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    let disambiguate = all || modes.contains(TermMode::DISAMBIGUATE_ESC_CODES);
    let event_types = modes.contains(TermMode::REPORT_EVENT_TYPES);
    let alternates = modes.contains(TermMode::REPORT_ALTERNATE_KEYS);
    let with_text = all && modes.contains(TermMode::REPORT_ASSOCIATED_TEXT);

    let Some(wire) = wire(event) else {
        return if event.kind == KeyEventKind::Release {
            Kitty::Nothing
        } else {
            Kitty::Legacy
        };
    };
    let mods = modifier_bits(event.mods);
    let chord = event.mods.control_key() || event.mods.alt_key() || event.mods.super_key();
    let release = event.kind == KeyEventKind::Release;
    // Without event types, a repeat is another press and a release is
    // never sent.
    if release && !event_types {
        return Kitty::Nothing;
    }
    let typed = event_types && event.kind != KeyEventKind::Press;

    match wire {
        Wire::Text(code) => {
            if !all && !(disambiguate && chord) {
                // Text stays text; its release is not an escape code.
                return if release {
                    Kitty::Nothing
                } else {
                    Kitty::Legacy
                };
            }
            let key = if alternates {
                alternate_keys(code, event)
            } else {
                code.to_string()
            };
            let text = with_text
                .then(|| associated_text(event))
                .flatten()
                .filter(|_| !release);
            Kitty::Send(csi_u(&key, mods, typed.then_some(event.kind), text))
        }
        Wire::Control(code) => {
            let escape = code == 27;
            let legacy_ok = !all && !(escape && disambiguate) && (mods == 0 || !disambiguate);
            if legacy_ok {
                return if release {
                    Kitty::Nothing
                } else {
                    Kitty::Legacy
                };
            }
            // Enter, Tab and Backspace releases only with "all keys".
            if release && !all && !escape {
                return Kitty::Nothing;
            }
            Kitty::Send(csi_u(
                &code.to_string(),
                mods,
                typed.then_some(event.kind),
                None,
            ))
        }
        Wire::Numbered(code) => {
            if is_modifier_key(code) && !all {
                return Kitty::Nothing;
            }
            if !disambiguate {
                return if release {
                    Kitty::Nothing
                } else {
                    Kitty::Legacy
                };
            }
            Kitty::Send(csi_u(
                &code.to_string(),
                mods,
                typed.then_some(event.kind),
                None,
            ))
        }
        Wire::Letter(final_byte) => {
            if !all && mods == 0 && !typed {
                return Kitty::Legacy;
            }
            let params = params(mods, typed.then_some(event.kind));
            let mut out = b"\x1b[".to_vec();
            if !params.is_empty() {
                out.extend_from_slice(b"1;");
                out.extend_from_slice(params.as_bytes());
            }
            out.push(final_byte);
            Kitty::Send(out)
        }
        Wire::Tilde(number) => {
            if !all && mods == 0 && !typed {
                return Kitty::Legacy;
            }
            let params = params(mods, typed.then_some(event.kind));
            let mut out = format!("\x1b[{number}").into_bytes();
            if !params.is_empty() {
                out.push(b';');
                out.extend_from_slice(params.as_bytes());
            }
            out.push(b'~');
            Kitty::Send(out)
        }
    }
}

/// `CSI key ; mods[:event] ; text u`, with empty trailing fields left out.
fn csi_u(key: &str, mods: u8, kind: Option<KeyEventKind>, text: Option<String>) -> Vec<u8> {
    let params = params(mods, kind);
    let mut out = format!("\x1b[{key}");
    if !params.is_empty() || text.is_some() {
        out.push(';');
        out.push_str(&params);
    }
    if let Some(text) = text {
        out.push(';');
        out.push_str(&text);
    }
    out.push('u');
    out.into_bytes()
}

/// The modifier field: `1 + bits`, with `:event` for a repeat or release;
/// empty when there is neither.
fn params(mods: u8, kind: Option<KeyEventKind>) -> String {
    let event = match kind {
        Some(KeyEventKind::Repeat) => Some(2),
        Some(KeyEventKind::Release) => Some(3),
        Some(KeyEventKind::Press) | None => None,
    };
    match (mods, event) {
        (0, None) => String::new(),
        (m, None) => (1 + m).to_string(),
        (m, Some(e)) => format!("{}:{e}", 1 + m),
    }
}

/// Shift 1, Alt 2, Ctrl 4, Super 8.
fn modifier_bits(mods: ModifiersState) -> u8 {
    u8::from(mods.shift_key())
        | u8::from(mods.alt_key()) << 1
        | u8::from(mods.control_key()) << 2
        | u8::from(mods.super_key()) << 3
}

/// `code:shifted:base`: the shifted key when Shift changes it, and the key
/// at the same position on a US layout when that differs.
fn alternate_keys(code: u32, event: &KeyEvent) -> String {
    let shifted = match event.logical {
        Key::Character(text) if event.mods.shift_key() => single_char(text)
            .map(u32::from)
            .filter(|&shifted| shifted != code),
        _ => None,
    };
    let base = match event.physical {
        PhysicalKey::Code(key) => us_layout(key).map(u32::from).filter(|&base| base != code),
        PhysicalKey::Unidentified(_) => None,
    };
    match (shifted, base) {
        (None, None) => code.to_string(),
        (Some(s), None) => format!("{code}:{s}"),
        (None, Some(b)) => format!("{code}::{b}"),
        (Some(s), Some(b)) => format!("{code}:{s}:{b}"),
    }
}

/// The key's text as colon-separated codepoints, without control
/// characters.
fn associated_text(event: &KeyEvent) -> Option<String> {
    let text = event.text?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }
    let codes: Vec<String> = text.chars().map(|c| u32::from(c).to_string()).collect();
    Some(codes.join(":"))
}

fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// How the key is identified, or `None` for a key the protocol has no
/// code for (it keeps its legacy handling).
fn wire(event: &KeyEvent) -> Option<Wire> {
    if let PhysicalKey::Code(code) = event.physical
        && let Some(number) = modifier_key(code)
    {
        return Some(Wire::Numbered(number));
    }
    if event.location == KeyLocation::Numpad {
        return keypad(event.logical).map(Wire::Numbered);
    }
    match event.logical {
        Key::Named(NamedKey::Escape) => Some(Wire::Control(27)),
        Key::Named(NamedKey::Enter) => Some(Wire::Control(13)),
        Key::Named(NamedKey::Tab) => Some(Wire::Control(9)),
        Key::Named(NamedKey::Backspace) => Some(Wire::Control(127)),
        Key::Named(NamedKey::Space) => Some(Wire::Text(32)),
        Key::Named(named) => functional(*named),
        Key::Character(_) => match event.unshifted {
            Key::Character(text) => single_char(text)
                .map(|c| c.to_lowercase().next().unwrap_or(c))
                .map(|c| Wire::Text(u32::from(c))),
            _ => None,
        },
        Key::Dead(_) | Key::Unidentified(_) => None,
    }
}

/// Keys with a legacy CSI form, and the protocol numbers of other
/// functional keys.
fn functional(key: NamedKey) -> Option<Wire> {
    use NamedKey::*;
    let wire = match key {
        ArrowUp => Wire::Letter(b'A'),
        ArrowDown => Wire::Letter(b'B'),
        ArrowRight => Wire::Letter(b'C'),
        ArrowLeft => Wire::Letter(b'D'),
        Home => Wire::Letter(b'H'),
        End => Wire::Letter(b'F'),
        F1 => Wire::Letter(b'P'),
        F2 => Wire::Letter(b'Q'),
        // `CSI 1;m R` would read as a cursor position report.
        F3 => Wire::Tilde(13),
        F4 => Wire::Letter(b'S'),
        Insert => Wire::Tilde(2),
        Delete => Wire::Tilde(3),
        PageUp => Wire::Tilde(5),
        PageDown => Wire::Tilde(6),
        F5 => Wire::Tilde(15),
        F6 => Wire::Tilde(17),
        F7 => Wire::Tilde(18),
        F8 => Wire::Tilde(19),
        F9 => Wire::Tilde(20),
        F10 => Wire::Tilde(21),
        F11 => Wire::Tilde(23),
        F12 => Wire::Tilde(24),
        F13 => Wire::Numbered(57376),
        F14 => Wire::Numbered(57377),
        F15 => Wire::Numbered(57378),
        F16 => Wire::Numbered(57379),
        F17 => Wire::Numbered(57380),
        F18 => Wire::Numbered(57381),
        F19 => Wire::Numbered(57382),
        F20 => Wire::Numbered(57383),
        F21 => Wire::Numbered(57384),
        F22 => Wire::Numbered(57385),
        F23 => Wire::Numbered(57386),
        F24 => Wire::Numbered(57387),
        F25 => Wire::Numbered(57388),
        F26 => Wire::Numbered(57389),
        F27 => Wire::Numbered(57390),
        F28 => Wire::Numbered(57391),
        F29 => Wire::Numbered(57392),
        F30 => Wire::Numbered(57393),
        F31 => Wire::Numbered(57394),
        F32 => Wire::Numbered(57395),
        F33 => Wire::Numbered(57396),
        F34 => Wire::Numbered(57397),
        F35 => Wire::Numbered(57398),
        PrintScreen => Wire::Numbered(57361),
        Pause => Wire::Numbered(57362),
        ContextMenu => Wire::Numbered(57363),
        MediaPlay => Wire::Numbered(57428),
        MediaPause => Wire::Numbered(57429),
        MediaPlayPause => Wire::Numbered(57430),
        MediaStop => Wire::Numbered(57432),
        MediaFastForward => Wire::Numbered(57433),
        MediaRewind => Wire::Numbered(57434),
        MediaTrackNext => Wire::Numbered(57435),
        MediaTrackPrevious => Wire::Numbered(57436),
        MediaRecord => Wire::Numbered(57437),
        AudioVolumeDown => Wire::Numbered(57438),
        AudioVolumeUp => Wire::Numbered(57439),
        AudioVolumeMute => Wire::Numbered(57440),
        _ => return None,
    };
    Some(wire)
}

/// Keypad keys (by what they produce, so Num Lock is respected).
fn keypad(logical: &Key) -> Option<u32> {
    let code = match logical {
        Key::Character(text) => match single_char(text)? {
            d @ '0'..='9' => 57399 + (u32::from(d) - u32::from('0')),
            '.' => 57409,
            '/' => 57410,
            '*' => 57411,
            '-' => 57412,
            '+' => 57413,
            '=' => 57415,
            ',' => 57416,
            _ => return None,
        },
        Key::Named(named) => match named {
            NamedKey::Enter => 57414,
            NamedKey::ArrowLeft => 57417,
            NamedKey::ArrowRight => 57418,
            NamedKey::ArrowUp => 57419,
            NamedKey::ArrowDown => 57420,
            NamedKey::PageUp => 57421,
            NamedKey::PageDown => 57422,
            NamedKey::Home => 57423,
            NamedKey::End => 57424,
            NamedKey::Insert => 57425,
            NamedKey::Delete => 57426,
            NamedKey::Clear => 57427,
            _ => return None,
        },
        _ => return None,
    };
    Some(code)
}

/// Modifier and lock keys, reported only with "all keys as escape codes".
fn modifier_key(code: KeyCode) -> Option<u32> {
    let number = match code {
        KeyCode::CapsLock => 57358,
        KeyCode::ScrollLock => 57359,
        KeyCode::NumLock => 57360,
        KeyCode::ShiftLeft => 57441,
        KeyCode::ControlLeft => 57442,
        KeyCode::AltLeft => 57443,
        KeyCode::SuperLeft => 57444,
        KeyCode::ShiftRight => 57447,
        KeyCode::ControlRight => 57448,
        KeyCode::AltRight => 57449,
        KeyCode::SuperRight => 57450,
        _ => return None,
    };
    Some(number)
}

fn is_modifier_key(number: u32) -> bool {
    (57358..=57360).contains(&number) || (57441..=57454).contains(&number)
}

/// The character at `key` on a US layout (the protocol's base layout).
pub(crate) fn us_layout(key: KeyCode) -> Option<char> {
    use KeyCode::*;
    let c = match key {
        KeyA => 'a',
        KeyB => 'b',
        KeyC => 'c',
        KeyD => 'd',
        KeyE => 'e',
        KeyF => 'f',
        KeyG => 'g',
        KeyH => 'h',
        KeyI => 'i',
        KeyJ => 'j',
        KeyK => 'k',
        KeyL => 'l',
        KeyM => 'm',
        KeyN => 'n',
        KeyO => 'o',
        KeyP => 'p',
        KeyQ => 'q',
        KeyR => 'r',
        KeyS => 's',
        KeyT => 't',
        KeyU => 'u',
        KeyV => 'v',
        KeyW => 'w',
        KeyX => 'x',
        KeyY => 'y',
        KeyZ => 'z',
        Digit0 => '0',
        Digit1 => '1',
        Digit2 => '2',
        Digit3 => '3',
        Digit4 => '4',
        Digit5 => '5',
        Digit6 => '6',
        Digit7 => '7',
        Digit8 => '8',
        Digit9 => '9',
        Minus => '-',
        Equal => '=',
        BracketLeft => '[',
        BracketRight => ']',
        Backslash => '\\',
        Semicolon => ';',
        Quote => '\'',
        Backquote => '`',
        Comma => ',',
        Period => '.',
        Slash => '/',
        Space => ' ',
        _ => return None,
    };
    Some(c)
}
