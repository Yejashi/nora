//! N21a regression suite for the kitty keyboard protocol mode stack
//! (`nora::engine`), and N21b regression suite for the frontend
//! encoding (`nora::input::encode_kitty`).
//!
//! Every test is headless: it drives a fresh [`Engine`] directly — no PTY,
//! no display, no clocks. The kitty keyboard protocol is negotiated by the
//! application as a push/pop mode stack; the engine owns the stack and the
//! enabled flag, and the frontend encoding (N21b) reads the negotiated
//! flags through [`Engine::kitty_mode`]. The default is legacy mode
//! (protocol disabled, empty stack).
//!
//! Pinned N21b byte table (verified against `src/input.rs`):
//! - key code (2 bytes, big-endian) — always;
//! - event type (1 byte: 1 = down, 2 = repeat, 3 = up) — only when
//!   `REPORT_EVENTS` is negotiated;
//! - system modifiers (1 byte: shift = 1, ctrl = 2, alt = 4,
//!   super/logo = 8) — only when `SYSTEM` is negotiated;
//! - `KittyKeyboardFlags::NONE` (legacy mode) → `None` (the caller falls
//!   back to the legacy encoder).

use alacritty_terminal::term::TermMode;
use nora::engine::{Engine, KittyKeyboardFlags, KittyKeyboardModes, MAX_KITTY_STACK};
use nora::input::{BindingTable, InputAction, encode, encode_kitty};
use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};

/// A fresh headless engine (24×80, no scrollback), in legacy mode.
fn make_engine() -> Engine {
    Engine::new(24, 80)
}

#[test]
fn kitty_modes_default() {
    // The type's default is legacy: protocol disabled, empty stack.
    let modes = KittyKeyboardModes::default();
    assert!(!modes.enabled);
    assert!(modes.stack.is_empty());

    // A fresh engine starts in legacy mode: query is NONE, stack is empty.
    let engine = make_engine();
    assert_eq!(engine.kitty_mode(), KittyKeyboardFlags::NONE);
    assert!(engine.kitty_mode_stack().is_empty());
}

#[test]
fn kitty_mode_push() {
    let mut engine = make_engine();
    // Legacy before any push.
    assert_eq!(engine.kitty_mode(), KittyKeyboardFlags::NONE);
    assert!(engine.kitty_mode_stack().is_empty());

    // Pushing enables the protocol; the pushed flags become the query.
    engine.push_kitty_mode(KittyKeyboardFlags::DISAMBIGUATE);
    assert_eq!(engine.kitty_mode(), KittyKeyboardFlags::DISAMBIGUATE);
    assert_eq!(
        engine.kitty_mode_stack(),
        &[KittyKeyboardFlags::DISAMBIGUATE]
    );
}

#[test]
fn kitty_mode_pop() {
    let mut engine = make_engine();
    engine.push_kitty_mode(KittyKeyboardFlags::DISAMBIGUATE);
    engine.push_kitty_mode(KittyKeyboardFlags::REPORT_EVENTS);
    assert_eq!(engine.kitty_mode(), KittyKeyboardFlags::REPORT_EVENTS);

    // Popping the top restores the previous mode.
    assert_eq!(
        engine.pop_kitty_mode(),
        Some(KittyKeyboardFlags::REPORT_EVENTS)
    );
    assert_eq!(engine.kitty_mode(), KittyKeyboardFlags::DISAMBIGUATE);
    assert_eq!(
        engine.kitty_mode_stack(),
        &[KittyKeyboardFlags::DISAMBIGUATE]
    );

    // Popping the last entry returns to legacy mode (query NONE).
    assert_eq!(
        engine.pop_kitty_mode(),
        Some(KittyKeyboardFlags::DISAMBIGUATE)
    );
    assert_eq!(engine.kitty_mode(), KittyKeyboardFlags::NONE);
    assert!(engine.kitty_mode_stack().is_empty());
}

#[test]
fn kitty_mode_pop_empty() {
    let mut engine = make_engine();
    // Popping an empty stack returns None and leaves legacy mode intact.
    assert_eq!(engine.pop_kitty_mode(), None);
    assert_eq!(engine.kitty_mode(), KittyKeyboardFlags::NONE);
}

#[test]
fn kitty_mode_query() {
    let mut engine = make_engine();
    engine.push_kitty_mode(KittyKeyboardFlags::DISAMBIGUATE);
    engine.push_kitty_mode(KittyKeyboardFlags::REPORT_EVENTS | KittyKeyboardFlags::SYSTEM);
    // The query is the most recently pushed flags.
    assert_eq!(
        engine.kitty_mode(),
        KittyKeyboardFlags::REPORT_EVENTS | KittyKeyboardFlags::SYSTEM
    );
}

#[test]
fn kitty_mode_query_disabled() {
    // A fresh engine has the protocol disabled: the query is NONE.
    let engine = make_engine();
    assert_eq!(engine.kitty_mode(), KittyKeyboardFlags::NONE);

    // After the stack is fully popped, the protocol is disabled again.
    let mut engine = make_engine();
    engine.push_kitty_mode(KittyKeyboardFlags::DISAMBIGUATE);
    engine.pop_kitty_mode();
    assert_eq!(engine.kitty_mode(), KittyKeyboardFlags::NONE);
}

#[test]
fn kitty_mode_stack() {
    let mut engine = make_engine();
    engine.push_kitty_mode(KittyKeyboardFlags::DISAMBIGUATE);
    engine.push_kitty_mode(KittyKeyboardFlags::REPORT_EVENTS);
    engine.push_kitty_mode(KittyKeyboardFlags::SYSTEM);

    // Oldest first, most recent last.
    assert_eq!(
        engine.kitty_mode_stack(),
        &[
            KittyKeyboardFlags::DISAMBIGUATE,
            KittyKeyboardFlags::REPORT_EVENTS,
            KittyKeyboardFlags::SYSTEM,
        ]
    );

    // Pop from the top.
    assert_eq!(engine.pop_kitty_mode(), Some(KittyKeyboardFlags::SYSTEM));
    assert_eq!(
        engine.kitty_mode_stack(),
        &[
            KittyKeyboardFlags::DISAMBIGUATE,
            KittyKeyboardFlags::REPORT_EVENTS,
        ]
    );
}

#[test]
fn kitty_mode_stack_bounded() {
    let mut engine = make_engine();
    // Fill the stack to the bound, then push one more: the oldest entry
    // drops off the front and the stack stays at capacity.
    for _ in 0..MAX_KITTY_STACK {
        engine.push_kitty_mode(KittyKeyboardFlags::DISAMBIGUATE);
    }
    engine.push_kitty_mode(KittyKeyboardFlags::REPORT_EVENTS);
    assert_eq!(engine.kitty_mode_stack().len(), MAX_KITTY_STACK);
    // The most recently pushed entry wins the query.
    assert_eq!(engine.kitty_mode(), KittyKeyboardFlags::REPORT_EVENTS);
}

#[test]
fn kitty_flags_contains() {
    let flags = KittyKeyboardFlags::DISAMBIGUATE | KittyKeyboardFlags::REPORT_EVENTS;
    // Each set bit is contained individually and as the combined set.
    assert!(flags.contains(KittyKeyboardFlags::DISAMBIGUATE));
    assert!(flags.contains(KittyKeyboardFlags::REPORT_EVENTS));
    assert!(flags.contains(KittyKeyboardFlags::DISAMBIGUATE | KittyKeyboardFlags::REPORT_EVENTS));
    // A bit not in the set is not contained.
    assert!(!flags.contains(KittyKeyboardFlags::SYSTEM));
    // NONE is contained in every set; ALL only when every bit is set.
    assert!(KittyKeyboardFlags::NONE.contains(KittyKeyboardFlags::NONE));
    assert!(flags.contains(KittyKeyboardFlags::NONE));
    assert!(!flags.contains(KittyKeyboardFlags::ALL));
}

#[test]
fn kitty_flags_set_clear() {
    let mut flags = KittyKeyboardFlags::NONE;
    flags.set(KittyKeyboardFlags::DISAMBIGUATE);
    assert!(flags.contains(KittyKeyboardFlags::DISAMBIGUATE));

    flags.set(KittyKeyboardFlags::SYSTEM);
    assert_eq!(
        flags,
        KittyKeyboardFlags::DISAMBIGUATE | KittyKeyboardFlags::SYSTEM
    );

    flags.clear(KittyKeyboardFlags::DISAMBIGUATE);
    assert_eq!(flags, KittyKeyboardFlags::SYSTEM);
    assert!(!flags.contains(KittyKeyboardFlags::DISAMBIGUATE));

    flags.clear(KittyKeyboardFlags::SYSTEM);
    assert_eq!(flags, KittyKeyboardFlags::NONE);
}

#[test]
fn kitty_flags_bitwise() {
    let a = KittyKeyboardFlags::DISAMBIGUATE;
    let b = KittyKeyboardFlags::REPORT_EVENTS;

    // OR combines distinct bits.
    let or = a | b;
    assert_eq!(or, KittyKeyboardFlags(0b11));
    assert!(or.contains(KittyKeyboardFlags::DISAMBIGUATE));
    assert!(or.contains(KittyKeyboardFlags::REPORT_EVENTS));

    // AND keeps only the shared bits.
    assert_eq!(or & a, a);
    assert_eq!(a & b, KittyKeyboardFlags::NONE);

    // OR is idempotent for shared bits.
    assert_eq!(a | a, a);
}

// ---------------------------------------------------------------------
// N21b: frontend encoding (`nora::input::encode_kitty`).
// ---------------------------------------------------------------------

/// The big-endian 2-byte key code for physical `KeyCode::KeyA`, as the
/// pinned N21b format emits it.
fn key_a_code() -> [u8; 2] {
    (KeyCode::KeyA as u16).to_be_bytes()
}

/// One plain (non-repeat) press of physical `KeyCode::KeyA` under the
/// given flags, no modifiers.
fn kitty_press(flags: KittyKeyboardFlags) -> Option<Vec<u8>> {
    encode_kitty(
        &Key::Character("a".into()),
        &PhysicalKey::Code(KeyCode::KeyA),
        &ModifiersState::empty(),
        false,
        flags,
    )
}

#[test]
fn encode_kitty_none() {
    // Legacy mode (protocol disabled): no kitty encoding — the caller
    // falls back to the legacy encoder.
    assert_eq!(kitty_press(KittyKeyboardFlags::NONE), None);
}

#[test]
fn encode_kitty_basic() {
    // DISAMBIGUATE only: the key code alone (2 bytes, big-endian) — no
    // event type, no system modifiers.
    let code = key_a_code();
    assert_eq!(
        kitty_press(KittyKeyboardFlags::DISAMBIGUATE),
        Some(vec![code[0], code[1]])
    );
}

#[test]
fn encode_kitty_modifiers() {
    // REPORT_EVENTS | SYSTEM: key code, then event type, then system
    // modifiers — in that order.
    let code = key_a_code();
    let down = encode_kitty(
        &Key::Character("a".into()),
        &PhysicalKey::Code(KeyCode::KeyA),
        &ModifiersState::empty(),
        false,
        KittyKeyboardFlags::REPORT_EVENTS | KittyKeyboardFlags::SYSTEM,
    );
    assert_eq!(down, Some(vec![code[0], code[1], 1, 0])); // down, no mods

    // Shift held: the system modifier byte carries the shift bit.
    let shifted = encode_kitty(
        &Key::Character("a".into()),
        &PhysicalKey::Code(KeyCode::KeyA),
        &ModifiersState::SHIFT,
        false,
        KittyKeyboardFlags::REPORT_EVENTS | KittyKeyboardFlags::SYSTEM,
    );
    assert_eq!(shifted, Some(vec![code[0], code[1], 1, 1]));
}

#[test]
fn encode_kitty_repeat() {
    // REPORT_EVENTS: a fresh press is event type 1 (down), an auto-
    // repeat is event type 2 (repeat).
    let code = key_a_code();
    assert_eq!(
        encode_kitty(
            &Key::Character("a".into()),
            &PhysicalKey::Code(KeyCode::KeyA),
            &ModifiersState::empty(),
            false,
            KittyKeyboardFlags::REPORT_EVENTS,
        ),
        Some(vec![code[0], code[1], 1])
    );
    assert_eq!(
        encode_kitty(
            &Key::Character("a".into()),
            &PhysicalKey::Code(KeyCode::KeyA),
            &ModifiersState::empty(),
            true,
            KittyKeyboardFlags::REPORT_EVENTS,
        ),
        Some(vec![code[0], code[1], 2])
    );
}

#[test]
fn encode_kitty_system_mods() {
    // SYSTEM: each modifier maps to its pinned bit (shift = 1, ctrl = 2,
    // alt = 4, super/logo = 8); combinations OR together.
    let code = key_a_code();
    let sys = |mods: ModifiersState| {
        encode_kitty(
            &Key::Character("a".into()),
            &PhysicalKey::Code(KeyCode::KeyA),
            &mods,
            false,
            KittyKeyboardFlags::SYSTEM,
        )
        .expect("SYSTEM negotiation encodes the press")
    };
    assert_eq!(
        sys(ModifiersState::empty()).as_slice(),
        &[code[0], code[1], 0][..]
    );
    assert_eq!(
        sys(ModifiersState::SHIFT).as_slice(),
        &[code[0], code[1], 1][..]
    );
    assert_eq!(
        sys(ModifiersState::CONTROL).as_slice(),
        &[code[0], code[1], 2][..]
    );
    assert_eq!(
        sys(ModifiersState::ALT).as_slice(),
        &[code[0], code[1], 4][..]
    );
    assert_eq!(
        sys(ModifiersState::SUPER).as_slice(),
        &[code[0], code[1], 8][..]
    );
    assert_eq!(
        sys(ModifiersState::SHIFT | ModifiersState::CONTROL).as_slice(),
        &[code[0], code[1], 3][..]
    );
}

#[test]
fn encode_kitty_fallback() {
    // Legacy mode: `encode_kitty` yields nothing and the legacy
    // `encode` still produces the legacy bytes (the window's fallback
    // path).
    let logical = Key::Character("a".into());
    let physical = PhysicalKey::Code(KeyCode::KeyA);
    let mods = ModifiersState::empty();

    assert_eq!(
        encode_kitty(&logical, &physical, &mods, false, KittyKeyboardFlags::NONE),
        None
    );
    assert_eq!(
        encode(
            &logical,
            &physical,
            &mods,
            false,
            &BindingTable::default(),
            TermMode::SHOW_CURSOR,
        ),
        Some(InputAction::Send(vec![b'a']))
    );
}
