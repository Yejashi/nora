//! The kitty keyboard protocol (M1.3, N21): the engine keeps the mode
//! stack and answers queries; `nora::input::kitty` encodes keys under the
//! active flags. Expected bytes follow
//! <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>.

use alacritty_terminal::term::TermMode;
use nora::engine::Engine;
use nora::input::kitty::{KeyEvent, KeyEventKind, Kitty, active, encode};
use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

const D: TermMode = TermMode::DISAMBIGUATE_ESC_CODES;
const E: TermMode = TermMode::REPORT_EVENT_TYPES;
const A: TermMode = TermMode::REPORT_ALTERNATE_KEYS;
const K: TermMode = TermMode::REPORT_ALL_KEYS_AS_ESC;
const T: TermMode = TermMode::REPORT_ASSOCIATED_TEXT;

const NONE: ModifiersState = ModifiersState::empty();
const SHIFT: ModifiersState = ModifiersState::SHIFT;
const CTRL: ModifiersState = ModifiersState::CONTROL;
const ALT: ModifiersState = ModifiersState::ALT;

fn kitty_modes(engine: &Engine) -> TermMode {
    engine.modes() & TermMode::KITTY_KEYBOARD_PROTOCOL
}

#[test]
fn engine_keeps_the_stack_and_answers_queries() {
    let mut engine = Engine::new(24, 80);
    assert!(kitty_modes(&engine).is_empty(), "legacy until pushed");
    engine.feed(b"\x1b[>1u");
    assert_eq!(kitty_modes(&engine), D);
    engine.feed(b"\x1b[>11u");
    assert_eq!(kitty_modes(&engine), D | E | K);
    engine.feed(b"\x1b[?u");
    assert_eq!(engine.replies(), vec!["\x1b[?11u".to_string()]);
    engine.feed(b"\x1b[<u");
    assert_eq!(kitty_modes(&engine), D, "pop restores the previous flags");
    engine.feed(b"\x1b[<u");
    assert!(kitty_modes(&engine).is_empty(), "back to legacy");
}

#[test]
fn alternate_screen_has_its_own_stack() {
    let mut engine = Engine::new(24, 80);
    engine.feed(b"\x1b[>1u");
    engine.feed(b"\x1b[?1049h");
    assert!(
        kitty_modes(&engine).is_empty(),
        "the alternate screen starts legacy"
    );
    engine.feed(b"\x1b[>8u\x1b[?1049l");
    assert_eq!(kitty_modes(&engine), D, "the main screen's flags return");
}

/// A key event for the tests: `logical` with modifiers applied,
/// `unshifted` without.
fn key<'a>(
    logical: &'a Key,
    unshifted: &'a Key,
    physical: KeyCode,
    mods: ModifiersState,
    kind: KeyEventKind,
    text: Option<&'a str>,
) -> KeyEvent<'a> {
    KeyEvent {
        logical,
        unshifted,
        physical: PhysicalKey::Code(physical),
        location: KeyLocation::Standard,
        mods,
        kind,
        text,
    }
}

fn send(bytes: &str) -> Kitty {
    Kitty::Send(bytes.as_bytes().to_vec())
}

fn ch(c: &str) -> Key {
    Key::Character(c.into())
}

fn named(n: NamedKey) -> Key {
    Key::Named(n)
}

use KeyEventKind::{Press, Release, Repeat};

#[test]
fn disambiguate_text_and_chords() {
    let (a, upper_a) = (ch("a"), ch("A"));
    let k = |logical, mods, kind| encode(&key(logical, &a, KeyCode::KeyA, mods, kind, None), D);
    assert_eq!(k(&a, NONE, Press), Kitty::Legacy, "plain text stays text");
    assert_eq!(
        k(&upper_a, SHIFT, Press),
        Kitty::Legacy,
        "shifted text stays text"
    );
    assert_eq!(k(&a, CTRL, Press), send("\x1b[97;5u"));
    assert_eq!(k(&a, ALT, Press), send("\x1b[97;3u"));
    assert_eq!(k(&upper_a, CTRL | SHIFT, Press), send("\x1b[97;6u"));
    assert_eq!(
        k(&a, CTRL, Repeat),
        send("\x1b[97;5u"),
        "no event types: a repeat is a press"
    );
    assert_eq!(
        k(&a, CTRL, Release),
        Kitty::Nothing,
        "no event types: no release"
    );
    let space = named(NamedKey::Space);
    let s = |mods| encode(&key(&space, &space, KeyCode::Space, mods, Press, None), D);
    assert_eq!(s(NONE), Kitty::Legacy);
    assert_eq!(s(CTRL), send("\x1b[32;5u"));
}

#[test]
fn disambiguate_escape_enter_tab_backspace() {
    let k = |n: NamedKey, code, mods| {
        let key_ = named(n);
        encode(&key(&key_, &key_, code, mods, Press, None), D)
    };
    assert_eq!(k(NamedKey::Escape, KeyCode::Escape, NONE), send("\x1b[27u"));
    assert_eq!(
        k(NamedKey::Escape, KeyCode::Escape, SHIFT),
        send("\x1b[27;2u")
    );
    assert_eq!(k(NamedKey::Enter, KeyCode::Enter, NONE), Kitty::Legacy);
    assert_eq!(k(NamedKey::Enter, KeyCode::Enter, CTRL), send("\x1b[13;5u"));
    assert_eq!(k(NamedKey::Tab, KeyCode::Tab, SHIFT), send("\x1b[9;2u"));
    assert_eq!(
        k(NamedKey::Backspace, KeyCode::Backspace, NONE),
        Kitty::Legacy
    );
    assert_eq!(
        k(NamedKey::Backspace, KeyCode::Backspace, ALT),
        send("\x1b[127;3u")
    );
}

#[test]
fn functional_keys_keep_their_legacy_forms() {
    let k = |n: NamedKey, code, mods, modes| {
        let key_ = named(n);
        encode(&key(&key_, &key_, code, mods, Press, None), modes)
    };
    assert_eq!(
        k(NamedKey::ArrowUp, KeyCode::ArrowUp, NONE, D),
        Kitty::Legacy
    );
    assert_eq!(
        k(NamedKey::ArrowUp, KeyCode::ArrowUp, CTRL, D),
        send("\x1b[1;5A")
    );
    assert_eq!(k(NamedKey::F1, KeyCode::F1, NONE, D), Kitty::Legacy);
    assert_eq!(k(NamedKey::F1, KeyCode::F1, SHIFT, D), send("\x1b[1;2P"));
    assert_eq!(k(NamedKey::F3, KeyCode::F3, SHIFT, D), send("\x1b[13;2~"));
    assert_eq!(k(NamedKey::F5, KeyCode::F5, CTRL, D), send("\x1b[15;5~"));
    assert_eq!(
        k(NamedKey::Delete, KeyCode::Delete, ALT, D),
        send("\x1b[3;3~")
    );
    assert_eq!(k(NamedKey::F13, KeyCode::F13, NONE, D), send("\x1b[57376u"));
    assert_eq!(
        k(NamedKey::ArrowUp, KeyCode::ArrowUp, NONE, K),
        send("\x1b[A")
    );
    assert_eq!(k(NamedKey::F5, KeyCode::F5, NONE, K), send("\x1b[15~"));
}

#[test]
fn keypad_keys_are_distinct() {
    let one = ch("1");
    let enter = named(NamedKey::Enter);
    let home = named(NamedKey::Home);
    let kp = |logical: &Key, code| {
        let mut event = key(logical, logical, code, NONE, Press, Some("1"));
        event.location = KeyLocation::Numpad;
        encode(&event, D)
    };
    assert_eq!(kp(&one, KeyCode::Numpad1), send("\x1b[57400u"));
    assert_eq!(kp(&enter, KeyCode::NumpadEnter), send("\x1b[57414u"));
    assert_eq!(
        kp(&home, KeyCode::Numpad7),
        send("\x1b[57423u"),
        "Num Lock off"
    );
}

#[test]
fn event_types() {
    let a = ch("a");
    let k = |mods, kind| encode(&key(&a, &a, KeyCode::KeyA, mods, kind, None), D | E);
    assert_eq!(k(CTRL, Press), send("\x1b[97;5u"));
    assert_eq!(k(CTRL, Repeat), send("\x1b[97;5:2u"));
    assert_eq!(k(CTRL, Release), send("\x1b[97;5:3u"));
    assert_eq!(
        k(NONE, Release),
        Kitty::Nothing,
        "text is not an escape code"
    );
    let up = named(NamedKey::ArrowUp);
    let arrow = |kind| encode(&key(&up, &up, KeyCode::ArrowUp, NONE, kind, None), D | E);
    assert_eq!(arrow(Press), Kitty::Legacy);
    assert_eq!(arrow(Repeat), send("\x1b[1;1:2A"));
    assert_eq!(arrow(Release), send("\x1b[1;1:3A"));
    let esc = named(NamedKey::Escape);
    let enter = named(NamedKey::Enter);
    assert_eq!(
        encode(
            &key(&esc, &esc, KeyCode::Escape, NONE, Release, None),
            D | E
        ),
        send("\x1b[27;1:3u")
    );
    assert_eq!(
        encode(
            &key(&enter, &enter, KeyCode::Enter, NONE, Release, None),
            D | E
        ),
        Kitty::Nothing,
        "Enter's release only with all keys"
    );
}

#[test]
fn all_keys_as_escape_codes() {
    let (a, upper_a) = (ch("a"), ch("A"));
    let k = |logical, mods, kind, modes| {
        encode(&key(logical, &a, KeyCode::KeyA, mods, kind, None), modes)
    };
    assert_eq!(k(&a, NONE, Press, K), send("\x1b[97u"));
    assert_eq!(k(&upper_a, SHIFT, Press, K), send("\x1b[97;2u"));
    assert_eq!(
        k(&a, NONE, Release, K),
        Kitty::Nothing,
        "releases need event types"
    );
    assert_eq!(k(&a, NONE, Release, K | E), send("\x1b[97;1:3u"));
    let enter = named(NamedKey::Enter);
    assert_eq!(
        encode(&key(&enter, &enter, KeyCode::Enter, NONE, Press, None), K),
        send("\x1b[13u")
    );
    assert_eq!(
        encode(
            &key(&enter, &enter, KeyCode::Enter, NONE, Release, None),
            K | E
        ),
        send("\x1b[13;1:3u")
    );
    let shift = named(NamedKey::Shift);
    let modifier = |modes| {
        encode(
            &key(&shift, &shift, KeyCode::ShiftLeft, SHIFT, Press, None),
            modes,
        )
    };
    assert_eq!(
        modifier(D),
        Kitty::Nothing,
        "modifier keys only with all keys"
    );
    assert_eq!(modifier(K), send("\x1b[57441;2u"));
}

#[test]
fn associated_text() {
    let (a, upper_a) = (ch("a"), ch("A"));
    let k = |logical, mods, kind, text| {
        encode(
            &key(logical, &a, KeyCode::KeyA, mods, kind, text),
            K | T | E,
        )
    };
    assert_eq!(k(&a, NONE, Press, Some("a")), send("\x1b[97;;97u"));
    assert_eq!(k(&upper_a, SHIFT, Press, Some("A")), send("\x1b[97;2;65u"));
    assert_eq!(
        k(&a, NONE, Release, Some("a")),
        send("\x1b[97;1:3u"),
        "no text on release"
    );
    assert_eq!(
        encode(&key(&a, &a, KeyCode::KeyA, NONE, Press, Some("a")), K),
        send("\x1b[97u"),
        "text only with flag 16"
    );
}

#[test]
fn alternate_keys() {
    let (one, bang) = (ch("1"), ch("!"));
    let k = |logical, mods| {
        encode(
            &key(logical, &one, KeyCode::Digit1, mods, Press, None),
            K | A,
        )
    };
    assert_eq!(k(&one, NONE), send("\x1b[49u"));
    assert_eq!(k(&bang, SHIFT), send("\x1b[49:33;2u"));
    // An AZERTY layout: the key at US `q` types `a`.
    let a = ch("a");
    assert_eq!(
        encode(&key(&a, &a, KeyCode::KeyQ, CTRL, Press, None), D | A),
        send("\x1b[97::113;5u")
    );
}

#[test]
fn active_only_with_a_flag() {
    assert!(!active(TermMode::SHOW_CURSOR | TermMode::APP_CURSOR));
    assert!(active(TermMode::SHOW_CURSOR | D));
    assert!(active(E));
}
