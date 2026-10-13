//! N10b regression suite: the F-key and keypad encoder rows
//! (`nora::input::encode`) plus their PTY legs.
//!
//! Pure-table legs (headless, no PTY, no display, no clocks):
//! - F1–F4 are the mode-invariant SS3 rows `\x1bOP`/`\x1bOQ`/`\x1bOR`/
//!   `\x1bOS`; F5–F12 are the mode-invariant CSI rows `\x1b[15~`…`\x1b[24~`;
//!   F13+ remains `None` (PC keyboards need Ctrl+Fn); Shift/Ctrl add no
//!   `1;N~` modifier encoding (the plain sequence is sent); Alt prefixes an
//!   `ESC`; Backspace is `0x7f` in both modes;
//! - the numpad is resolved by the authoritative physical `Numpad*`
//!   scancode (pinned winit 0.30.13 folds the logical key to
//!   `Character`, or to `Enter`/`Home`/`End` for KP_Enter/KP_Home/KP_End):
//!   digits send the plain digit in normal mode and the SS3 letter
//!   `p`–`y` in app mode; `.`→`n`, Enter→`M`, `+`/`-`/`*`/`/`→
//!   `k`/`m`/`j`/`l`, `,`→`l`, `=`→`X`; numpad Home/End (physicals
//!   `Numpad7`/`Numpad1`) are app-only `H`/`F` (`None` in normal mode);
//!   app = `APP_CURSOR || APP_KEYPAD` (decision N10-2); a main-row digit or
//!   Enter uses a non-numpad physical and must not false-match;
//!   `Key::Unidentified` still consults the physical numpad scancode.
//!
//! PTY legs (real child, `nora_child check`, deadline-bounded, 100 ms
//! settle before grid inspection per the pinned `pty_read` try-lock / EIO
//! drop window): (a) F1 → the child receives the exact `\x1bOP`; (b)
//! keypad app — the private-mode sequence `\x1b=` (DECKPAM; pinned
//! vte-0.15.0 escape-final dispatch, src/ansi.rs:1816, recorded by the
//! pinned alacritty term as `APP_KEYPAD`, term/mod.rs:2180-2182; the
//! pinned CSI `?` namespace maps mode 6 to `Origin`, NOT the keypad —
//! vte-0.15.0/src/ansi.rs:902) is driven through the shared engine, then
//! `Numpad0` under app mode reaches the child as the exact `\x1bOp`.

use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::term::TermMode;
use nora::engine::Engine;
use nora::input::{BindingTable, InputAction, encode};
use nora::session::io::WriterConfig;
use nora::session::pty::{LaunchSpec, PtySession, SessionEvent};
use winit::keyboard::{
    Key, KeyCode, ModifiersState, NamedKey, NativeKey, NativeKeyCode, PhysicalKey,
};

/// One poll tick inside the deadline-bounded pumps.
const POLL: Duration = Duration::from_millis(10);
/// Hard deadline for every PTY wait in this suite.
const DEADLINE: Duration = Duration::from_secs(10);
/// Settle before grid inspection after a flush to a fast-exiting child
/// (pinned `pty_read` try-lock / EIO drop window, N09a).
const SETTLE: Duration = Duration::from_millis(100);

/// Normal (no app bits) and application-keypad modes, constructed
/// directly: `encode`'s app test is `APP_CURSOR || APP_KEYPAD`
/// (decision N10-2), so each constant carries exactly the bit under test.
const NORMAL: TermMode = TermMode::SHOW_CURSOR;
const APP: TermMode = TermMode::APP_KEYPAD;

/// The helper bin path, auto-discovered by cargo for integration tests.
fn helper() -> String {
    std::env::var("CARGO_BIN_EXE_nora_child")
        .expect("CARGO_BIN_EXE_nora_child must be set by cargo for integration tests")
}

fn cap() -> WriterConfig {
    WriterConfig {
        max_bytes: 256,
        reserved_bytes: 64,
    }
}

/// Lower-case hex for the helper's `check` argv (two-digit pairs).
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `InputAction::Send` with the given bytes, for assertion readability.
fn send(bytes: Vec<u8>) -> InputAction {
    InputAction::Send(bytes)
}

/// Encode one press (explicit logical + physical keys) under the default
/// binding table.
fn enc(
    logical: &Key,
    physical: &PhysicalKey,
    mods: ModifiersState,
    modes: TermMode,
) -> Option<InputAction> {
    encode(
        logical,
        physical,
        &mods,
        false,
        &BindingTable::default(),
        modes,
    )
}

/// The physical key for a `Named` press whose physical identity the row
/// does not inspect (F-keys, Backspace, Unidentified legs).
fn plain_physical() -> PhysicalKey {
    PhysicalKey::Unidentified(NativeKeyCode::Unidentified)
}

/// The engine's current grid as one row-major string (any visible line is
/// a contiguous substring).
fn grid_text(session: &mut PtySession) -> String {
    let snap = session.engine().snapshot();
    snap.cells.iter().map(|c| c.ch).collect()
}

/// Deadline-bounded pump: each tick drains `poll_events()`, re-reads the
/// grid, and hands both to `satisfied`. Returns `Ok` on the first
/// satisfied tick; `Err` after the 10 s deadline with the last grid text.
fn pump(
    session: &mut PtySession,
    mut satisfied: impl FnMut(&str, &[SessionEvent]) -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + DEADLINE;
    let mut events: Vec<SessionEvent> = Vec::new();
    let mut last;
    loop {
        let mut fresh = session.poll_events();
        events.append(&mut fresh);
        last = grid_text(session);
        if satisfied(&last, &events) {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!(
                "deadline exceeded; last grid: {last:?}; saw {} event(s)",
                events.len()
            ));
        }
        thread::sleep(POLL);
    }
}

/// True if `events` contains a `Terminated` whose exit status has code `code`.
fn exited_with(events: &[SessionEvent], code: i32) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            SessionEvent::Terminated { status: Some(status), .. }
                if status.code() == Some(code)
        )
    })
}

// ---------------------------------------------------------------------------
// (a) F-keys — mode-invariant rows
// ---------------------------------------------------------------------------

/// F1–F4 are the SS3 rows `ESC O P`/`Q`/`R`/`S`, identical in normal and
/// application modes (the xterm F1–F4 SS3 forms have no `~` variant).
#[test]
fn f1_to_f4_are_ss3_in_both_modes() {
    let rows = [
        (NamedKey::F1, vec![0x1b, b'O', b'P']),
        (NamedKey::F2, vec![0x1b, b'O', b'Q']),
        (NamedKey::F3, vec![0x1b, b'O', b'R']),
        (NamedKey::F4, vec![0x1b, b'O', b'S']),
    ];
    for (name, bytes) in rows {
        assert_eq!(
            enc(
                &Key::Named(name),
                &plain_physical(),
                ModifiersState::empty(),
                NORMAL
            ),
            Some(send(bytes.clone())),
            "{name:?} normal"
        );
        assert_eq!(
            enc(
                &Key::Named(name),
                &plain_physical(),
                ModifiersState::empty(),
                APP
            ),
            Some(send(bytes)),
            "{name:?} app"
        );
    }
}

/// F5–F12 are the CSI rows `ESC [ 15~`/`17~`/`18~`/`19~`/`20~`/`21~`/`23~`/
/// `24~`, identical in normal and application modes.
#[test]
fn f5_to_f12_are_csi_in_both_modes() {
    let rows = [
        (NamedKey::F5, vec![0x1b, b'[', b'1', b'5', b'~']),
        (NamedKey::F6, vec![0x1b, b'[', b'1', b'7', b'~']),
        (NamedKey::F7, vec![0x1b, b'[', b'1', b'8', b'~']),
        (NamedKey::F8, vec![0x1b, b'[', b'1', b'9', b'~']),
        (NamedKey::F9, vec![0x1b, b'[', b'2', b'0', b'~']),
        (NamedKey::F10, vec![0x1b, b'[', b'2', b'1', b'~']),
        (NamedKey::F11, vec![0x1b, b'[', b'2', b'3', b'~']),
        (NamedKey::F12, vec![0x1b, b'[', b'2', b'4', b'~']),
    ];
    for (name, bytes) in rows {
        assert_eq!(
            enc(
                &Key::Named(name),
                &plain_physical(),
                ModifiersState::empty(),
                NORMAL
            ),
            Some(send(bytes.clone())),
            "{name:?} normal"
        );
        assert_eq!(
            enc(
                &Key::Named(name),
                &plain_physical(),
                ModifiersState::empty(),
                APP
            ),
            Some(send(bytes)),
            "{name:?} app"
        );
    }
}

/// F13+ has no v0.1 bytes: PC keyboards need Ctrl+Fn to reach them
/// (deferred), so the encoder sends nothing in either mode.
#[test]
fn f13_stays_none() {
    for modes in [NORMAL, APP] {
        assert_eq!(
            enc(
                &Key::Named(NamedKey::F13),
                &plain_physical(),
                ModifiersState::empty(),
                modes
            ),
            None,
            "F13 must stay None under {modes:?}"
        );
    }
}

/// Shift+ and Ctrl+ F5 send the plain CSI row (no `1;N~` modifier
/// encoding in v0.1); a combined Shift+Ctrl press is still plain.
#[test]
fn shift_and_ctrl_fkeys_send_the_plain_sequence() {
    let mods = [
        ModifiersState::SHIFT,
        ModifiersState::CONTROL,
        ModifiersState::SHIFT | ModifiersState::CONTROL,
    ];
    for m in mods {
        assert_eq!(
            enc(&Key::Named(NamedKey::F5), &plain_physical(), m, NORMAL),
            Some(send(vec![0x1b, b'[', b'1', b'5', b'~'])),
            "plain F5 under {m:?}"
        );
    }
}

/// Alt prefixes an `ESC`, leaving the base row unchanged (the N09 Alt
/// rule): Alt+F1 → `\x1b\x1bOP`; Alt+Numpad5 under app mode →
/// `\x1b\x1bOu`.
#[test]
fn alt_prefixes_the_fkey_and_numpad_sequences() {
    assert_eq!(
        enc(
            &Key::Named(NamedKey::F1),
            &plain_physical(),
            ModifiersState::ALT,
            NORMAL
        ),
        Some(send(vec![0x1b, 0x1b, b'O', b'P'])),
        "Alt+F1"
    );
    let physical = PhysicalKey::Code(KeyCode::Numpad5);
    assert_eq!(
        enc(
            &Key::Character("5".into()),
            &physical,
            ModifiersState::ALT,
            APP
        ),
        Some(send(vec![0x1b, 0x1b, b'O', b'u'])),
        "Alt+Numpad5 app"
    );
}

/// Backspace is `0x7f` in both modes (the raw slave's kernel `erase`; the
/// N09 row is mode-invariant).
#[test]
fn backspace_is_0x7f_in_both_modes() {
    assert_eq!(
        enc(
            &Key::Named(NamedKey::Backspace),
            &plain_physical(),
            ModifiersState::empty(),
            NORMAL
        ),
        Some(send(vec![0x7f])),
        "Backspace normal"
    );
    assert_eq!(
        enc(
            &Key::Named(NamedKey::Backspace),
            &plain_physical(),
            ModifiersState::empty(),
            APP
        ),
        Some(send(vec![0x7f])),
        "Backspace app"
    );
}

// ---------------------------------------------------------------------------
// (b) Keypad — physical-scancode rows, both modes
// ---------------------------------------------------------------------------

/// `Numpad0` sends the plain `0` in normal mode and the SS3 letter `p`
/// in app mode; `APP_CURSOR` alone also takes the SS3 form (decision
/// N10-2: app = `APP_CURSOR || APP_KEYPAD`).
#[test]
fn numpad0_plain_digit_vs_ss3() {
    let physical = PhysicalKey::Code(KeyCode::Numpad0);
    assert_eq!(
        enc(
            &Key::Character("0".into()),
            &physical,
            ModifiersState::empty(),
            NORMAL
        ),
        Some(send(vec![0x30])),
        "Numpad0 normal"
    );
    assert_eq!(
        enc(
            &Key::Character("0".into()),
            &physical,
            ModifiersState::empty(),
            APP
        ),
        Some(send(vec![0x1b, b'O', b'p'])),
        "Numpad0 app (APP_KEYPAD)"
    );
    assert_eq!(
        enc(
            &Key::Character("0".into()),
            &physical,
            ModifiersState::empty(),
            TermMode::APP_CURSOR
        ),
        Some(send(vec![0x1b, b'O', b'p'])),
        "Numpad0 app (APP_CURSOR alone)"
    );
}

/// A main-row digit uses a non-numpad physical: the plain `Character`
/// branch wins and no false numpad match fires, in both modes.
#[test]
fn main_row_digit_is_not_a_numpad() {
    let physical = PhysicalKey::Code(KeyCode::Digit5);
    assert_eq!(
        enc(
            &Key::Character("5".into()),
            &physical,
            ModifiersState::empty(),
            NORMAL
        ),
        Some(send(vec![0x35])),
        "Digit5 normal"
    );
    assert_eq!(
        enc(
            &Key::Character("5".into()),
            &physical,
            ModifiersState::empty(),
            APP
        ),
        Some(send(vec![0x35])),
        "Digit5 app"
    );
}

/// A main-row Enter uses the main-row physical: the guarded `NumpadEnter`
/// arm does not match, and `0x0d` is sent in both modes.
#[test]
fn main_row_enter_is_not_a_numpad_enter() {
    let physical = PhysicalKey::Code(KeyCode::Enter);
    assert_eq!(
        enc(
            &Key::Named(NamedKey::Enter),
            &physical,
            ModifiersState::empty(),
            NORMAL
        ),
        Some(send(vec![0x0d])),
        "Enter normal"
    );
    assert_eq!(
        enc(
            &Key::Named(NamedKey::Enter),
            &physical,
            ModifiersState::empty(),
            APP
        ),
        Some(send(vec![0x0d])),
        "Enter app"
    );
}

/// One numpad row: the plain byte in normal mode, the SS3 letter in app
/// mode (winit delivers the logical `Character` / `Enter` with the
/// authoritative `Numpad*` physical).
fn numpad_row(logical: Key, code: KeyCode, normal: u8, ss3: u8) {
    let physical = PhysicalKey::Code(code);
    assert_eq!(
        enc(&logical, &physical, ModifiersState::empty(), NORMAL),
        Some(send(vec![normal])),
        "{code:?} normal"
    );
    assert_eq!(
        enc(&logical, &physical, ModifiersState::empty(), APP),
        Some(send(vec![0x1b, b'O', ss3])),
        "{code:?} app"
    );
}

/// The pinned numpad special-key pairs: `.`→`n`, Enter→`M`,
/// `+`/`-`/`*`/`/`→`k`/`m`/`j`/`l`, `,`→`l`, `=`→`X`.
#[test]
fn numpad_special_keys_plain_vs_ss3() {
    numpad_row(
        Key::Character(".".into()),
        KeyCode::NumpadDecimal,
        0x2e,
        b'n',
    );
    numpad_row(
        Key::Named(NamedKey::Enter),
        KeyCode::NumpadEnter,
        0x0d,
        b'M',
    );
    numpad_row(Key::Character("+".into()), KeyCode::NumpadAdd, 0x2b, b'k');
    numpad_row(
        Key::Character("-".into()),
        KeyCode::NumpadSubtract,
        0x2d,
        b'm',
    );
    numpad_row(
        Key::Character("*".into()),
        KeyCode::NumpadMultiply,
        0x2a,
        b'j',
    );
    numpad_row(
        Key::Character("/".into()),
        KeyCode::NumpadDivide,
        0x2f,
        b'l',
    );
    numpad_row(Key::Character(",".into()), KeyCode::NumpadComma, 0x2c, b'l');
    numpad_row(Key::Character("=".into()), KeyCode::NumpadEqual, 0x3d, b'X');
}

/// Numpad Home/End (physicals `Numpad7`/`Numpad1`, winit-folds to
/// `Home`/`End`) have no mode-invariant form: `None` in normal mode, the
/// SS3 `H`/`F` in app mode.
#[test]
fn numpad_home_end_are_app_only() {
    let home = PhysicalKey::Code(KeyCode::Numpad7);
    assert_eq!(
        enc(
            &Key::Named(NamedKey::Home),
            &home,
            ModifiersState::empty(),
            NORMAL
        ),
        None,
        "numpad Home normal"
    );
    assert_eq!(
        enc(
            &Key::Named(NamedKey::Home),
            &home,
            ModifiersState::empty(),
            APP
        ),
        Some(send(vec![0x1b, b'O', b'H'])),
        "numpad Home app"
    );
    let end = PhysicalKey::Code(KeyCode::Numpad1);
    assert_eq!(
        enc(
            &Key::Named(NamedKey::End),
            &end,
            ModifiersState::empty(),
            NORMAL
        ),
        None,
        "numpad End normal"
    );
    assert_eq!(
        enc(
            &Key::Named(NamedKey::End),
            &end,
            ModifiersState::empty(),
            APP
        ),
        Some(send(vec![0x1b, b'O', b'F'])),
        "numpad End app"
    );
}

/// A numpad operator may arrive `Unidentified` (winit folds it when
/// xkbcommon has no character for the keysym); the physical `Numpad*`
/// scancode is still authoritative.
#[test]
fn unidentified_logical_still_resolves_the_numpad_physical() {
    let physical = PhysicalKey::Code(KeyCode::Numpad5);
    assert_eq!(
        enc(
            &Key::Unidentified(NativeKey::Xkb(0)),
            &physical,
            ModifiersState::empty(),
            NORMAL
        ),
        Some(send(vec![0x35])),
        "Unidentified Numpad5 normal"
    );
    assert_eq!(
        enc(
            &Key::Unidentified(NativeKey::Xkb(0)),
            &physical,
            ModifiersState::empty(),
            APP
        ),
        Some(send(vec![0x1b, b'O', b'u'])),
        "Unidentified Numpad5 app"
    );
}

// ---------------------------------------------------------------------------
// (c) Mode switch — fully headless, on a standalone Engine
// ---------------------------------------------------------------------------

/// N10b: the pinned DECKPAM pair `\x1b=`/`\x1b>` sets and clears
/// `APP_KEYPAD` on a standalone [`Engine`], read back through the in-tree
/// accessor `Engine::modes` (src/engine.rs:425). The pinned vte 0.15.0
/// escape-final dispatch maps `=`/`>` to the keypad handlers
/// (src/ansi.rs:1816-1817); the pinned alacritty term records them as
/// `APP_KEYPAD` (term/mod.rs:2180-2188). The clear path matters: a later
/// `\x1b>` returns `Numpad0` to the plain `0` row.
#[test]
fn deckpam_sets_and_clears_app_keypad() {
    let mut engine = Engine::new(24, 80);

    // A fresh term starts with APP_KEYPAD off.
    assert!(
        !engine.modes().contains(TermMode::APP_KEYPAD),
        "fresh engine must start with APP_KEYPAD off"
    );
    assert_eq!(
        enc(
            &Key::Character("0".into()),
            &PhysicalKey::Code(KeyCode::Numpad0),
            ModifiersState::empty(),
            engine.modes(),
        ),
        Some(send(vec![0x30])),
        "Numpad0 in normal mode"
    );

    engine.feed(b"\x1b=");
    assert!(
        engine.modes().contains(TermMode::APP_KEYPAD),
        "\\x1b= must set APP_KEYPAD"
    );
    assert_eq!(
        enc(
            &Key::Character("0".into()),
            &PhysicalKey::Code(KeyCode::Numpad0),
            ModifiersState::empty(),
            engine.modes(),
        ),
        Some(send(vec![0x1b, b'O', b'p'])),
        "Numpad0 in app mode"
    );

    engine.feed(b"\x1b>");
    assert!(
        !engine.modes().contains(TermMode::APP_KEYPAD),
        "\\x1b> must clear APP_KEYPAD"
    );
    assert_eq!(
        enc(
            &Key::Character("0".into()),
            &PhysicalKey::Code(KeyCode::Numpad0),
            ModifiersState::empty(),
            engine.modes(),
        ),
        Some(send(vec![0x30])),
        "Numpad0 back in normal mode"
    );
}

// ---------------------------------------------------------------------------
// (d) PTY legs — one per class
// ---------------------------------------------------------------------------

/// N10b: F1 encoded by `encode` reaches the real child as the exact
/// contiguous bytes `\x1bOP` (deadline-bounded, P1 shape).
#[test]
fn f1_reaches_the_child() {
    let input = enc(
        &Key::Named(NamedKey::F1),
        &plain_physical(),
        ModifiersState::empty(),
        NORMAL,
    )
    .expect("F1 must encode");
    let InputAction::Send(input) = input else {
        panic!("F1 must encode to Send");
    };
    assert_eq!(input, vec![0x1b, 0x4f, 0x50]);

    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec![
                "check".to_string(),
                "N10BF1".to_string(),
                String::new(),
                hex(&input),
            ],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    pump(&mut session, |text, _events| text.contains("READY"))
        .expect("child printed READY within the deadline");

    assert_eq!(session.feed(&input), Ok(input.len()));
    session.flush(256).expect("flush must forward the F1 bytes");

    // Settle before inspecting: the check child matches and exits within
    // microseconds of the flush, so the flush tick's own snapshot
    // (main-thread term lock) deterministically contends in the pinned
    // `pty_read` try-lock / EIO drop window. 100 ms is three orders of
    // magnitude over the observed microsecond-scale window.
    thread::sleep(SETTLE);

    pump(&mut session, |text, events| {
        text.contains("CHECK:N10BF1") && exited_with(events, 0)
    })
    .expect("child matched the exact F1 bytes and exited cleanly within the deadline");

    session.close();
}

/// N10b: with the keypad application mode set, `Numpad0` encodes
/// `\x1bOp` and those bytes reach the real child. The mode is driven
/// through the shared engine by the pinned DECKPAM sequence `\x1b=` —
/// NOT `CSI ?6h`, which the pinned stack maps to `Origin`
/// (vte-0.15.0/src/ansi.rs:902), so it would not set the keypad.
#[test]
fn app_keypad_numpad0_reaches_the_child() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec![
                "check".to_string(),
                "N10BKP".to_string(),
                String::new(),
                // App Numpad0: `\x1bOp`.
                "1b4f70".to_string(),
            ],
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY");

    pump(&mut session, |text, _events| text.contains("READY"))
        .expect("child printed READY within the deadline");

    // Drive DECKPAM on through the shared engine: the pinned vte 0.15.0
    // escape-final dispatch (`=`, src/ansi.rs:1816) records
    // `APP_KEYPAD` on the pinned alacritty term (term/mod.rs:2180-2182),
    // and the in-tree accessor reflects it. Flush + settle per the drive
    // idiom (N10a app-arrow leg).
    session.engine().feed(b"\x1b=");
    session
        .flush(256)
        .expect("flush must process the DECKPAM drive");
    thread::sleep(SETTLE);
    assert!(
        session.engine().modes().contains(TermMode::APP_KEYPAD),
        "DECKPAM must set APP_KEYPAD on the shared engine"
    );

    let input = enc(
        &Key::Character("0".into()),
        &PhysicalKey::Code(KeyCode::Numpad0),
        ModifiersState::empty(),
        session.engine().modes(),
    )
    .expect("app-mode Numpad0 must encode");
    let InputAction::Send(input) = input else {
        panic!("app-mode Numpad0 must encode to Send");
    };
    assert_eq!(input, vec![0x1b, 0x4f, 0x70]);

    assert_eq!(session.feed(&input), Ok(input.len()));
    session
        .flush(256)
        .expect("flush must forward the app numpad bytes");

    thread::sleep(SETTLE);

    pump(&mut session, |text, events| {
        text.contains("CHECK:N10BKP") && exited_with(events, 0)
    })
    .expect("child matched the exact app numpad bytes and exited cleanly within the deadline");

    session.close();
}
