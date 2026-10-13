//! N09a regression suite for the pure keyboard encoder
//! (`nora::input::encode`).
//!
//! Every test is headless: plain winit key fields (no `KeyEvent`), no PTY,
//! no display, no clocks. All rows are encoded under
//! `TermMode::SHOW_CURSOR` — N09 rows are mode-independent (the 6-arg
//! `encode` signature pins the `modes` parameter, lead decision N10-1) — and
//! under the default binding table unless a test says otherwise.
//!
//! Pinned byte table (verified against `src/input.rs`):
//! - plain `Character` → its own UTF-8 bytes; Shift+letter arrives as the
//!   shifted character and sends those bytes;
//! - `Named` → Enter `0x0d`, Tab `0x09`, Backspace `0x7f` (the raw slave's
//!   kernel `erase`, not `Key::to_text()`'s `\x08`), Escape `0x1b`, Space
//!   `0x20` / Ctrl+Space `0x00`;
//! - Ctrl+letter → `0x01`–`0x1a` case-insensitively; Ctrl + non-letter
//!   character → the character's own bytes;
//! - Alt+character → `0x1b` prefix + the character's bytes;
//! - `Key::Dead` / `Key::Unidentified` / `Named` rows with no N09 meaning
//!   (modifier keys, arrows, F-keys, Home) → `None`;
//! - physical Ctrl+Q: the binding fires before layout decoding (`Quit`);
//!   with `ctrl_q_quit: false`, a `Character("q")` falls through to the
//!   Ctrl-letter row (`Send([0x11])`) and an `Unidentified` logical key has
//!   nothing left to encode (`None`);
//! - `repeat: true` encodes exactly like a fresh press.

use alacritty_terminal::term::TermMode;
use nora::input::{BindingTable, InputAction, encode};
use winit::keyboard::{
    Key, KeyCode, ModifiersState, NamedKey, NativeKey, NativeKeyCode, PhysicalKey,
};

/// The mode every N09 row is encoded under (see module docs).
const MODES: TermMode = TermMode::SHOW_CURSOR;

/// `InputAction::Send` with the given bytes, for assertion readability.
fn send(bytes: Vec<u8>) -> InputAction {
    InputAction::Send(bytes)
}

/// The physical key for a letter press. Non-letters use `Unidentified` —
/// the physical code only matters to the Ctrl+Q binding, which the tests
/// drive explicitly with `KeyCode::KeyQ`.
fn letter_physical(c: char) -> PhysicalKey {
    match c {
        'a' | 'A' => PhysicalKey::Code(KeyCode::KeyA),
        'q' | 'Q' => PhysicalKey::Code(KeyCode::KeyQ),
        'z' | 'Z' => PhysicalKey::Code(KeyCode::KeyZ),
        _ => PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
    }
}

/// Encode one press under the default binding table: the given logical and
/// physical keys, no repeat, mode [`MODES`].
fn enc(logical: Key, physical: PhysicalKey, mods: ModifiersState) -> Option<InputAction> {
    encode(
        &logical,
        &physical,
        &mods,
        false,
        &BindingTable::default(),
        MODES,
    )
}

/// One `Character` press under `mods` (`ModifiersState::empty()` for none).
fn enc_char_mods(text: &str, mods: ModifiersState) -> Option<InputAction> {
    let c = text.chars().next().expect("non-empty key text");
    enc(Key::Character(text.into()), letter_physical(c), mods)
}

/// One `Named` press under `mods`; the physical key is `Unidentified`
/// (no N09 row inspects it for these keys).
fn enc_named(name: NamedKey, mods: ModifiersState) -> Option<InputAction> {
    enc(
        Key::Named(name),
        PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
        mods,
    )
}

#[test]
fn plain_ascii_characters_send_their_own_bytes() {
    assert_eq!(
        enc_char_mods("a", ModifiersState::empty()),
        Some(send(vec![0x61]))
    );
    // Shift+Z arrives as "Z" and sends its own byte.
    assert_eq!(
        enc_char_mods("Z", ModifiersState::empty()),
        Some(send(vec![0x5a]))
    );
}

#[test]
fn plain_multibyte_characters_send_utf8() {
    assert_eq!(
        enc_char_mods("é", ModifiersState::empty()),
        Some(send(vec![0xc3, 0xa9]))
    );
    assert_eq!(
        enc_char_mods("ü", ModifiersState::empty()),
        Some(send(vec![0xc3, 0xbc]))
    );
}

#[test]
fn named_space_plain_and_ctrl() {
    assert_eq!(
        enc_named(NamedKey::Space, ModifiersState::empty()),
        Some(send(vec![0x20]))
    );
    // Ctrl+Space is NUL, not a space.
    assert_eq!(
        enc_named(NamedKey::Space, ModifiersState::CONTROL),
        Some(send(vec![0x00]))
    );
}

#[test]
fn named_edit_keys() {
    assert_eq!(
        enc_named(NamedKey::Enter, ModifiersState::empty()),
        Some(send(vec![0x0d]))
    );
    assert_eq!(
        enc_named(NamedKey::Tab, ModifiersState::empty()),
        Some(send(vec![0x09]))
    );
    // The raw slave keeps the kernel `erase`, not `Key::to_text()`'s `\x08`.
    assert_eq!(
        enc_named(NamedKey::Backspace, ModifiersState::empty()),
        Some(send(vec![0x7f]))
    );
    assert_eq!(
        enc_named(NamedKey::Escape, ModifiersState::empty()),
        Some(send(vec![0x1b]))
    );
}

/// The encoder's Ctrl-letter rows: with the Ctrl+Q local binding disabled,
/// all 26 letters map to `0x01`–`0x1a`. Under the default table the
/// physical KeyQ binding intercepts `q` first (see the `ctrl_q` tests).
#[test]
fn ctrl_letters_are_the_ascii_control_range() {
    let bindings = BindingTable { ctrl_q_quit: false };
    for c in 'a'..='z' {
        let text: String = c.to_string();
        let logical = Key::Character(text.into());
        let physical = letter_physical(c);
        let got = encode(
            &logical,
            &physical,
            &ModifiersState::CONTROL,
            false,
            &bindings,
            MODES,
        );
        assert_eq!(got, Some(send(vec![c as u8 - b'a' + 1])), "Ctrl+{c}");
    }
}

#[test]
fn ctrl_a_and_ctrl_z_with_default_bindings() {
    assert_eq!(
        enc_char_mods("a", ModifiersState::CONTROL),
        Some(send(vec![0x01]))
    );
    assert_eq!(
        enc_char_mods("z", ModifiersState::CONTROL),
        Some(send(vec![0x1a]))
    );
}

#[test]
fn ctrl_uppercase_with_shift_is_case_insensitive() {
    // Shift+letter arrives as the uppercase character; `ctrl_byte`
    // lowercases before mapping, so the row is case-insensitive.
    let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
    assert_eq!(
        enc(
            Key::Character("A".into()),
            PhysicalKey::Code(KeyCode::KeyA),
            mods
        ),
        Some(send(vec![0x01]))
    );
    // Ctrl+Shift+Q (uppercase-with-shift) is still 0x11 once the local
    // binding is disabled.
    let bindings = BindingTable { ctrl_q_quit: false };
    let logical = Key::Character("Q".into());
    let physical = PhysicalKey::Code(KeyCode::KeyQ);
    let got = encode(&logical, &physical, &mods, false, &bindings, MODES);
    assert_eq!(got, Some(send(vec![0x11])));
}

#[test]
fn ctrl_non_letter_character_sends_its_own_bytes() {
    // Unpinned in the N09 block: `ctrl_byte` returns `None` for
    // non-letters, so the encoder sends the character's own bytes.
    assert_eq!(
        enc_char_mods("é", ModifiersState::CONTROL),
        Some(send(vec![0xc3, 0xa9]))
    );
}

#[test]
fn alt_prefixes_the_character_with_escape() {
    assert_eq!(
        enc_char_mods("a", ModifiersState::ALT),
        Some(send(vec![0x1b, 0x61]))
    );
    // The xterm meta convention extends to multibyte characters:
    // 0x1b + UTF-8 bytes.
    assert_eq!(
        enc_char_mods("é", ModifiersState::ALT),
        Some(send(vec![0x1b, 0xc3, 0xa9]))
    );
}

#[test]
fn key_repeat_encodes_like_a_fresh_press() {
    let logical = Key::Character("a".into());
    let physical = PhysicalKey::Code(KeyCode::KeyA);
    let got = encode(
        &logical,
        &physical,
        &ModifiersState::empty(),
        true,
        &BindingTable::default(),
        MODES,
    );
    assert_eq!(got, Some(send(vec![0x61])));
}

#[test]
fn dead_and_unidentified_keys_send_nothing() {
    // Dead keys are never bytes in v0.1 (the composed character arrives as
    // the later `Character` press); `Unidentified` has no layout text.
    let physical = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    assert_eq!(
        enc(Key::Dead(None), physical, ModifiersState::empty()),
        None
    );
    assert_eq!(
        enc(Key::Dead(Some('a')), physical, ModifiersState::empty()),
        None
    );
    assert_eq!(
        enc(
            Key::Unidentified(NativeKey::Xkb(0)),
            physical,
            ModifiersState::empty()
        ),
        None
    );
}

#[test]
fn modifier_only_and_navigation_named_keys_send_nothing() {
    // `Named` rows with no N09 meaning (modifier key, arrows, F-key, Home)
    // stay `None` until N10.
    for name in [
        NamedKey::Shift,
        NamedKey::ArrowUp,
        NamedKey::ArrowDown,
        NamedKey::ArrowLeft,
        NamedKey::ArrowRight,
        NamedKey::Home,
        NamedKey::F1,
    ] {
        assert_eq!(
            enc_named(name, ModifiersState::empty()),
            None,
            "{name:?} has no N09 bytes"
        );
    }
}

#[test]
fn ctrl_q_default_binding_quits() {
    // Without Ctrl, the same physical key is a plain letter press.
    assert_eq!(
        enc_char_mods("q", ModifiersState::empty()),
        Some(send(vec![0x71]))
    );
    // With Ctrl, the physical-key binding fires for either case of the
    // logical character.
    assert_eq!(
        enc(
            Key::Character("q".into()),
            PhysicalKey::Code(KeyCode::KeyQ),
            ModifiersState::CONTROL
        ),
        Some(InputAction::Quit)
    );
    assert_eq!(
        enc(
            Key::Character("Q".into()),
            PhysicalKey::Code(KeyCode::KeyQ),
            ModifiersState::CONTROL
        ),
        Some(InputAction::Quit)
    );
}

#[test]
fn ctrl_q_disabled_falls_through_to_the_encoder() {
    // With `ctrl_q_quit: false` the binding no longer fires: a
    // `Character("q")` takes the Ctrl-letter row (0x11), and an
    // `Unidentified` logical key has nothing left to encode.
    let bindings = BindingTable { ctrl_q_quit: false };
    let physical = PhysicalKey::Code(KeyCode::KeyQ);
    let mods = ModifiersState::CONTROL;
    assert_eq!(
        encode(
            &Key::Character("q".into()),
            &physical,
            &mods,
            false,
            &bindings,
            MODES
        ),
        Some(send(vec![0x11]))
    );
    assert_eq!(
        encode(
            &Key::Unidentified(NativeKey::Xkb(0)),
            &physical,
            &mods,
            false,
            &bindings,
            MODES
        ),
        None
    );
}

#[test]
fn ctrl_q_binding_precedes_layout_decoding() {
    // The binding keys on the physical key: even with no layout text at
    // all, the press still quits under the default table.
    let logical = Key::Unidentified(NativeKey::Xkb(0));
    let physical = PhysicalKey::Code(KeyCode::KeyQ);
    assert_eq!(
        encode(
            &logical,
            &physical,
            &ModifiersState::CONTROL,
            false,
            &BindingTable::default(),
            MODES
        ),
        Some(InputAction::Quit)
    );
}
