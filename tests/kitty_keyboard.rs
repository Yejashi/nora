//! N21a regression suite for the kitty keyboard protocol mode stack
//! (`nora::engine`).
//!
//! Every test is headless: it drives a fresh [`Engine`] directly — no PTY,
//! no display, no clocks. The kitty keyboard protocol is negotiated by the
//! application as a push/pop mode stack; the engine owns the stack and the
//! enabled flag, and the frontend encoding (N21b) reads the negotiated
//! flags through [`Engine::kitty_mode`]. The default is legacy mode
//! (protocol disabled, empty stack).

use nora::engine::{Engine, KittyKeyboardFlags, KittyKeyboardModes, MAX_KITTY_STACK};

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
