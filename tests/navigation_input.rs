//! N10a regression suite: the cursor-key / core-navigation encoder rows
//! (`nora::input::encode`) plus their PTY legs.
//!
//! Pure-table legs (headless, no PTY, no display, no clocks):
//! - arrows + Home/End are DECCKM-conditional on [`TermMode::APP_CURSOR`]:
//!   normal sends `ESC [ <letter>`, application sends `ESC O <letter>` (the
//!   discriminating pair is `\x1b[A` vs `\x1bOA`);
//! - Insert/Delete/PageUp/PageDown are mode-invariant `ESC [ <n>~` (their
//!   DECCKM scope excludes them);
//! - Alt prefixes `ESC` (the N09 Alt rule); Shift/Ctrl add no `1;N~`
//!   modifier encoding (the plain sequence is sent); Backspace is `0x7f`
//!   in both modes;
//! - F-keys, `Key::Dead`, `Key::Unidentified` → `None` (F-keys are N10b).
//!
//! PTY legs (real child, `nora_child check`, deadline-bounded, 100 ms
//! settle before grid inspection per the pinned `pty_read` try-lock / EIO
//! drop window): one leg per mode class — normal arrow, app arrow, edit
//! key. The app leg drives `\x1b[?1h` through the shared engine so the
//! pinned `alacritty_terminal` term sets `APP_CURSOR`, then feeds the
//! encoded app-arrow bytes.
//!
//! Mode-switch leg (fully headless, [`Engine`]): `\x1b[?1h` sets
//! `APP_CURSOR` and `\x1b[?1l` clears it (in-tree accessor
//! `Engine::modes`, src/engine.rs:425); the pinned DECRPM (`\x1b[?1$p`)
//! replies `\x1b[?1;2$y` while APP is reset and `\x1b[?1;1$y` once set —
//! `ModeState` Set=1 / Reset=2 (`From<bool>`), private mode 1 =
//! `CursorKeys` (pinned alacritty_terminal term/mod.rs:2050, 2090-2094,
//! 2277-2290; vte-0.15.0 ansi.rs:900).

use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::term::TermMode;
use nora::engine::Engine;
use nora::input::{BindingTable, InputAction, encode};
use nora::session::io::WriterConfig;
use nora::session::pty::{LaunchSpec, PtySession, SessionEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey, NativeKey, NativeKeyCode, PhysicalKey};

/// One poll tick inside the deadline-bounded pumps.
const POLL: Duration = Duration::from_millis(10);
/// Hard deadline for every PTY wait in this suite.
const DEADLINE: Duration = Duration::from_secs(10);
/// Settle before grid inspection after a flush to a fast-exiting child
/// (pinned `pty_read` try-lock / EIO drop window, N09a).
const SETTLE: Duration = Duration::from_millis(100);

/// Normal (DECCKM off) and application (`APP_CURSOR`) cursor-key modes.
/// `encode` inspects only `APP_CURSOR`, so each constant carries exactly
/// the bit under test.
const NORMAL: TermMode = TermMode::SHOW_CURSOR;
const APP: TermMode = TermMode::APP_CURSOR;

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

/// The physical key for a `Named` press (no N10a row inspects it).
fn nav_physical() -> PhysicalKey {
    PhysicalKey::Unidentified(NativeKeyCode::Unidentified)
}

/// Encode one `Named` press under the given `mods` and `modes`.
fn enc_named(name: NamedKey, mods: ModifiersState, modes: TermMode) -> Option<InputAction> {
    encode(
        &Key::Named(name),
        &nav_physical(),
        &mods,
        false,
        &BindingTable::default(),
        modes,
    )
}

// ---------------------------------------------------------------------------
// (a) Pure table
// ---------------------------------------------------------------------------

/// The discriminating normal-vs-app arrow pair, plus the other three
/// arrows, in both modes: `\x1b[A`/`\x1bOA` (Up), and Down/Left/Right
/// follow the same letter under the same DECCKM condition.
#[test]
fn arrows_normal_vs_application() {
    assert_eq!(
        enc_named(NamedKey::ArrowUp, ModifiersState::empty(), NORMAL),
        Some(send(vec![0x1b, b'[', b'A'])),
        "Up normal"
    );
    assert_eq!(
        enc_named(NamedKey::ArrowUp, ModifiersState::empty(), APP),
        Some(send(vec![0x1b, b'O', b'A'])),
        "Up app"
    );
    assert_eq!(
        enc_named(NamedKey::ArrowDown, ModifiersState::empty(), NORMAL),
        Some(send(vec![0x1b, b'[', b'B'])),
        "Down normal"
    );
    assert_eq!(
        enc_named(NamedKey::ArrowDown, ModifiersState::empty(), APP),
        Some(send(vec![0x1b, b'O', b'B'])),
        "Down app"
    );
    assert_eq!(
        enc_named(NamedKey::ArrowLeft, ModifiersState::empty(), NORMAL),
        Some(send(vec![0x1b, b'[', b'D'])),
        "Left normal"
    );
    assert_eq!(
        enc_named(NamedKey::ArrowLeft, ModifiersState::empty(), APP),
        Some(send(vec![0x1b, b'O', b'D'])),
        "Left app"
    );
    assert_eq!(
        enc_named(NamedKey::ArrowRight, ModifiersState::empty(), NORMAL),
        Some(send(vec![0x1b, b'[', b'C'])),
        "Right normal"
    );
    assert_eq!(
        enc_named(NamedKey::ArrowRight, ModifiersState::empty(), APP),
        Some(send(vec![0x1b, b'O', b'C'])),
        "Right app"
    );
}

/// Home/End are DECCKM-conditional like the arrows: `\x1b[H`/`\x1b[O` and
/// `\x1b[F`/`\x1bO` (app), in both modes.
#[test]
fn home_end_normal_vs_application() {
    assert_eq!(
        enc_named(NamedKey::Home, ModifiersState::empty(), NORMAL),
        Some(send(vec![0x1b, b'[', b'H'])),
        "Home normal"
    );
    assert_eq!(
        enc_named(NamedKey::Home, ModifiersState::empty(), APP),
        Some(send(vec![0x1b, b'O', b'H'])),
        "Home app"
    );
    assert_eq!(
        enc_named(NamedKey::End, ModifiersState::empty(), NORMAL),
        Some(send(vec![0x1b, b'[', b'F'])),
        "End normal"
    );
    assert_eq!(
        enc_named(NamedKey::End, ModifiersState::empty(), APP),
        Some(send(vec![0x1b, b'O', b'F'])),
        "End app"
    );
}

/// Insert/Delete/PageUp/PageDown send the same bytes in both modes (their
/// DECCKM scope excludes them).
#[test]
fn edit_keys_mode_invariant() {
    let rows = [
        (NamedKey::Insert, vec![0x1b, b'[', b'2', b'~']),
        (NamedKey::Delete, vec![0x1b, b'[', b'3', b'~']),
        (NamedKey::PageUp, vec![0x1b, b'[', b'5', b'~']),
        (NamedKey::PageDown, vec![0x1b, b'[', b'6', b'~']),
    ];
    for (name, bytes) in rows {
        assert_eq!(
            enc_named(name, ModifiersState::empty(), NORMAL),
            Some(send(bytes.clone())),
            "{name:?} normal"
        );
        assert_eq!(
            enc_named(name, ModifiersState::empty(), APP),
            Some(send(bytes)),
            "{name:?} app"
        );
    }
}

/// Alt prefixes an `ESC` (the N09 Alt rule), leaving the base sequence
/// unchanged; verified for a DECCKM-conditional key (Up) in both modes and
/// for a mode-invariant key (Delete).
#[test]
fn alt_prefixes_the_navigation_sequence() {
    assert_eq!(
        enc_named(NamedKey::ArrowUp, ModifiersState::ALT, NORMAL),
        Some(send(vec![0x1b, 0x1b, b'[', b'A'])),
        "Alt+Up normal"
    );
    assert_eq!(
        enc_named(NamedKey::ArrowUp, ModifiersState::ALT, APP),
        Some(send(vec![0x1b, 0x1b, b'O', b'A'])),
        "Alt+Up app"
    );
    assert_eq!(
        enc_named(NamedKey::Delete, ModifiersState::ALT, NORMAL),
        Some(send(vec![0x1b, 0x1b, b'[', b'3', b'~'])),
        "Alt+Delete"
    );
}

/// Shift+ and Ctrl+ arrow send the plain sequence (no `1;N~` modifier
/// encoding in v0.1); a combined Shift+Ctrl press is still plain.
#[test]
fn shift_and_ctrl_arrow_send_the_plain_sequence() {
    let mods = [
        ModifiersState::SHIFT,
        ModifiersState::CONTROL,
        ModifiersState::SHIFT | ModifiersState::CONTROL,
    ];
    for m in mods {
        assert_eq!(
            enc_named(NamedKey::ArrowUp, m, NORMAL),
            Some(send(vec![0x1b, b'[', b'A'])),
            "plain arrow under {m:?}"
        );
        assert_eq!(
            enc_named(NamedKey::ArrowUp, m, APP),
            Some(send(vec![0x1b, b'O', b'A'])),
            "plain arrow under {m:?} (app)"
        );
    }
}

/// Backspace is `0x7f` in both modes (the raw slave's kernel `erase`; the
/// N09 row is mode-invariant).
#[test]
fn backspace_is_0x7f_in_both_modes() {
    assert_eq!(
        enc_named(NamedKey::Backspace, ModifiersState::empty(), NORMAL),
        Some(send(vec![0x7f])),
        "Backspace normal"
    );
    assert_eq!(
        enc_named(NamedKey::Backspace, ModifiersState::empty(), APP),
        Some(send(vec![0x7f])),
        "Backspace app"
    );
}

/// F-keys stay `None` (N10b); `Key::Dead` and `Key::Unidentified` send
/// nothing (the N09 `None` rows are unchanged).
#[test]
fn fkeys_dead_unidentified_send_nothing() {
    for name in [NamedKey::F1, NamedKey::F2] {
        assert_eq!(
            enc_named(name, ModifiersState::empty(), NORMAL),
            None,
            "{name:?} has no N10a bytes"
        );
        assert_eq!(
            enc_named(name, ModifiersState::empty(), APP),
            None,
            "{name:?} has no N10a bytes (app)"
        );
    }
    let physical = nav_physical();
    assert_eq!(
        encode(
            &Key::Dead(None),
            &physical,
            &ModifiersState::empty(),
            false,
            &BindingTable::default(),
            NORMAL
        ),
        None,
        "Dead (no char)"
    );
    assert_eq!(
        encode(
            &Key::Dead(Some('a')),
            &physical,
            &ModifiersState::empty(),
            false,
            &BindingTable::default(),
            APP
        ),
        None,
        "Dead (composed char)"
    );
    assert_eq!(
        encode(
            &Key::Unidentified(NativeKey::Xkb(0)),
            &physical,
            &ModifiersState::empty(),
            false,
            &BindingTable::default(),
            NORMAL
        ),
        None,
        "Unidentified"
    );
}

// ---------------------------------------------------------------------------
// (b) PTY legs — one per mode class
// ---------------------------------------------------------------------------

/// N10a: a normal-mode arrow encoded by `encode` reaches the real child as
/// the exact contiguous bytes `\x1b[A` (deadline-bounded, P1 shape).
#[test]
fn normal_arrow_reaches_the_child() {
    let input = encode(
        &Key::Named(NamedKey::ArrowUp),
        &nav_physical(),
        &ModifiersState::empty(),
        false,
        &BindingTable::default(),
        NORMAL,
    )
    .expect("normal-mode Up must encode");
    let InputAction::Send(input) = input else {
        panic!("normal-mode Up must encode to Send");
    };
    assert_eq!(input, vec![0x1b, 0x5b, 0x41]);

    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec![
                "check".to_string(),
                "N10NORM".to_string(),
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
    session
        .flush(256)
        .expect("flush must forward the arrow bytes");

    // Settle before inspecting: the check child matches and exits within
    // microseconds of the flush, so the flush tick's own snapshot
    // (main-thread term lock) deterministically contends in the pinned
    // `pty_read` try-lock / EIO drop window. 100 ms is three orders of
    // magnitude over the observed microsecond-scale window.
    thread::sleep(SETTLE);

    pump(&mut session, |text, events| {
        text.contains("CHECK:N10NORM") && exited_with(events, 0)
    })
    .expect("child matched the exact normal arrow and exited cleanly within the deadline");

    session.close();
}

/// N10a: with `APP_CURSOR` set, the same physical Up encodes `\x1bOA` and
/// those bytes reach the real child. The app mode is driven through the
/// shared engine (`\x1b[?1h`) so the pinned term sets `APP_CURSOR` — not
/// asserted by hand.
#[test]
fn app_arrow_reaches_the_child() {
    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec![
                "check".to_string(),
                "N10APP".to_string(),
                String::new(),
                // App Up: `\x1bOA`.
                "1b4f41".to_string(),
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

    // Drive DECCKM on through the shared engine: the pinned term sets
    // `APP_CURSOR` (alacritty_terminal term/mod.rs:2001-2005), and the
    // in-tree accessor reflects it. Flush + settle per the drive idiom.
    session.engine().feed(b"\x1b[?1h");
    session
        .flush(256)
        .expect("flush must process the DECCKM drive");
    thread::sleep(SETTLE);
    assert!(
        session.engine().modes().contains(TermMode::APP_CURSOR),
        "DECCKM-on must set APP_CURSOR on the shared engine"
    );

    let input = encode(
        &Key::Named(NamedKey::ArrowUp),
        &nav_physical(),
        &ModifiersState::empty(),
        false,
        &BindingTable::default(),
        session.engine().modes(),
    )
    .expect("app-mode Up must encode");
    let InputAction::Send(input) = input else {
        panic!("app-mode Up must encode to Send");
    };
    assert_eq!(input, vec![0x1b, 0x4f, 0x41]);

    assert_eq!(session.feed(&input), Ok(input.len()));
    session
        .flush(256)
        .expect("flush must forward the app arrow bytes");

    thread::sleep(SETTLE);

    pump(&mut session, |text, events| {
        text.contains("CHECK:N10APP") && exited_with(events, 0)
    })
    .expect("child matched the exact app arrow and exited cleanly within the deadline");

    session.close();
}

/// N10a: a mode-invariant edit key (Delete, `\x1b[3~`) encoded in normal
/// mode reaches the real child as the exact contiguous bytes
/// (deadline-bounded, P1 shape).
#[test]
fn edit_key_reaches_the_child() {
    let input = encode(
        &Key::Named(NamedKey::Delete),
        &nav_physical(),
        &ModifiersState::empty(),
        false,
        &BindingTable::default(),
        NORMAL,
    )
    .expect("Delete must encode");
    let InputAction::Send(input) = input else {
        panic!("Delete must encode to Send");
    };
    assert_eq!(input, vec![0x1b, 0x5b, 0x33, 0x7e]);

    let mut session = PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: vec![
                "check".to_string(),
                "N10EDIT".to_string(),
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
    session
        .flush(256)
        .expect("flush must forward the edit-key bytes");

    thread::sleep(SETTLE);

    pump(&mut session, |text, events| {
        text.contains("CHECK:N10EDIT") && exited_with(events, 0)
    })
    .expect("child matched the exact edit key and exited cleanly within the deadline");

    session.close();
}

// ---------------------------------------------------------------------------
// (c) Mode-switch leg — fully headless, on a standalone Engine
// ---------------------------------------------------------------------------

/// N10a: DECCKM set (`\x1b[?1h`) turns on `APP_CURSOR` and clears the
/// bit only on reset (`\x1b[?1l`), read back through the in-tree accessor
/// `Engine::modes` (src/engine.rs:425). `encode` tracks the flip: Up is
/// `\x1bOA` while set and `\x1b[A` once reset.
#[test]
fn decckm_sets_and_clears_app_cursor() {
    let mut engine = Engine::new(24, 80);

    // A fresh term starts with APP_CURSOR off (alacritty_term TermMode::
    // default() = SHOW_CURSOR only, term/mod.rs:113-115, 443).
    assert!(
        !engine.modes().contains(TermMode::APP_CURSOR),
        "fresh engine must start with APP_CURSOR off"
    );
    assert_eq!(
        encode(
            &Key::Named(NamedKey::ArrowUp),
            &nav_physical(),
            &ModifiersState::empty(),
            false,
            &BindingTable::default(),
            engine.modes(),
        ),
        Some(send(vec![0x1b, 0x5b, 0x41])),
        "Up in normal mode"
    );

    // DECCKM on: the pinned term sets APP_CURSOR (term/mod.rs:2001-2005).
    engine.feed(b"\x1b[?1h");
    assert!(
        engine.modes().contains(TermMode::APP_CURSOR),
        "\\x1b[?1h must set APP_CURSOR"
    );
    assert_eq!(
        encode(
            &Key::Named(NamedKey::ArrowUp),
            &nav_physical(),
            &ModifiersState::empty(),
            false,
            &BindingTable::default(),
            engine.modes(),
        ),
        Some(send(vec![0x1b, 0x4f, 0x41])),
        "Up in app mode"
    );

    // DECCKM off: the bit is cleared and encode returns to the plain row.
    engine.feed(b"\x1b[?1l");
    assert!(
        !engine.modes().contains(TermMode::APP_CURSOR),
        "\\x1b[?1l must clear APP_CURSOR"
    );
    assert_eq!(
        encode(
            &Key::Named(NamedKey::ArrowUp),
            &nav_physical(),
            &ModifiersState::empty(),
            false,
            &BindingTable::default(),
            engine.modes(),
        ),
        Some(send(vec![0x1b, 0x5b, 0x41])),
        "Up back in normal mode"
    );
}

/// N10a bonus: the pinned DECRPM (`\x1b[?1$p`) reports the cursor-key mode
/// as `\x1b[?1;<state>$y`, where `state` is `ModeState` Reset=2 while
/// `APP_CURSOR` is off and Set=1 once it is on (alacritty_terminal
/// term/mod.rs:2050, 2090-2094, 2277-2290). The reply is captured
/// headlessly through `Engine::replies`.
#[test]
fn decrpm_reports_the_cursor_key_mode() {
    let mut engine = Engine::new(24, 80);

    // Fresh term: APP_CURSOR off → ModeState::Reset (2).
    engine.feed(b"\x1b[?1$p");
    assert!(
        engine.replies().contains(&"\x1b[?1;2$y".to_string()),
        "DECRPM with APP off must reply \\x1b[?1;2$y; saw {:?}",
        engine.replies()
    );

    // DECCKM on: APP_CURSOR set → ModeState::Set (1).
    engine.feed(b"\x1b[?1h");
    engine.feed(b"\x1b[?1$p");
    assert!(
        engine.replies().contains(&"\x1b[?1;1$y".to_string()),
        "DECRPM with APP on must reply \\x1b[?1;1$y; saw {:?}",
        engine.replies()
    );
}
