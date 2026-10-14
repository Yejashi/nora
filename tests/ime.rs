//! N22a regression suite for the IME state machine (`nora::ime::ImeState`).
//!
//! Headless: no PTY, no threads, no GPU, no window. Each test drives the
//! pure state machine (preedit, commit, cancel) and asserts the card's
//! acceptance lines: preedit is UI-only state, commit text is produced
//! exactly once, and cancel/clear drop everything without sending.

use nora::ime::ImeState;

/// N22a (1): a fresh IME state is inactive with empty preedit and
/// committed text; `Default` agrees.
#[test]
fn ime_default() {
    let ime = ImeState::new();
    assert!(!ime.is_active());
    assert_eq!(ime.preedit(), "");
    assert_eq!(ime.committed(), "");

    let default = ImeState::default();
    assert!(!default.is_active());
    assert_eq!(default.preedit(), "");
    assert_eq!(default.committed(), "");
}

/// N22a (2): starting a composition activates the IME and starts with
/// an empty preedit.
#[test]
fn ime_start_composition() {
    let mut ime = ImeState::new();
    assert!(!ime.is_active());

    ime.start_composition();
    assert!(ime.is_active());
    assert_eq!(ime.preedit(), "");
    assert_eq!(ime.committed(), "");
}

/// N22a (3): updating the preedit replaces it; the preedit is UI-only
/// state and is readable while the composition is active.
#[test]
fn ime_update_preedit() {
    let mut ime = ImeState::new();
    ime.start_composition();
    ime.update_preedit("nihao");
    assert_eq!(ime.preedit(), "nihao");
    assert!(ime.is_active());

    ime.update_preedit("nihao2");
    assert_eq!(ime.preedit(), "nihao2");
}

/// N22a (4): committing returns the composed preedit exactly once,
/// deactivates the machine, and records the committed text.
#[test]
fn ime_commit() {
    let mut ime = ImeState::new();
    ime.start_composition();
    ime.update_preedit("こんにちは");

    let committed = ime.commit();
    assert_eq!(committed.as_deref(), Some("こんにちは"));
    assert!(!ime.is_active());
    assert_eq!(ime.preedit(), "");
    assert_eq!(ime.committed(), "こんにちは");
}

/// N22a (5): committing an active composition with no preedit and no
/// committed text returns `None` (nothing is sent to the PTY).
#[test]
fn ime_commit_empty() {
    let mut ime = ImeState::new();
    ime.start_composition();
    assert_eq!(ime.commit(), None);
}

/// N22a (6): cancelling clears the active flag, the preedit and any
/// committed text without producing output.
#[test]
fn ime_cancel() {
    let mut ime = ImeState::new();
    ime.start_composition();
    ime.update_preedit("abc");
    ime.cancel();

    assert!(!ime.is_active());
    assert_eq!(ime.preedit(), "");
    assert_eq!(ime.committed(), "");
    assert_eq!(ime.commit(), None);
}

/// N22a (7): committing while the IME is inactive returns `None`,
/// both before any composition and after a finished one.
#[test]
fn ime_inactive_commit() {
    let mut ime = ImeState::new();
    assert_eq!(ime.commit(), None);

    ime.start_composition();
    ime.update_preedit("abc");
    assert_eq!(ime.commit().as_deref(), Some("abc"));
    // The machine deactivated on commit; a further commit is a no-op.
    assert_eq!(ime.commit(), None);
}

/// N22a (8): `clear` resets the active flag, preedit and committed
/// text even mid-composition.
#[test]
fn ime_clear() {
    let mut ime = ImeState::new();
    ime.start_composition();
    ime.update_preedit("abc");
    ime.clear();

    assert!(!ime.is_active());
    assert_eq!(ime.preedit(), "");
    assert_eq!(ime.committed(), "");
}

/// N22a (9): successive compositions each commit their own preedit
/// exactly once; a commit with no active composition returns `None`.
#[test]
fn ime_multiple_commits() {
    let mut ime = ImeState::new();
    ime.start_composition();
    ime.update_preedit("hello");
    assert_eq!(ime.commit().as_deref(), Some("hello"));

    ime.start_composition();
    ime.update_preedit("world");
    assert_eq!(ime.commit().as_deref(), Some("world"));

    assert_eq!(ime.commit(), None);
}

/// N22a (10): the preedit is empty by default and stays empty until a
/// composition updates it.
#[test]
fn ime_preedit_empty() {
    let ime = ImeState::new();
    assert_eq!(ime.preedit(), "");

    let mut ime = ImeState::new();
    ime.start_composition();
    assert_eq!(ime.preedit(), "");
}

// ---------------------------------------------------------------------------
// N22b UI fixtures: the window event handler folds the platform `Ime`
// events into `ImeState` in this order — `Enabled` → `start_composition`,
// `Preedit` → `update_preedit`, `Commit` → `commit`, `Disabled` →
// `cancel`. These fixtures drive the machine exactly as the window does
// and assert the card's acceptance lines: preedit is UI-only, commit is
// produced exactly once, and cancel clears all state.
// ---------------------------------------------------------------------------

/// N22b (1): the `Ime::Enabled` event starts a composition via the UI —
/// the machine activates with an empty preedit.
#[test]
fn ime_ui_start() {
    let mut ime = ImeState::new();
    assert!(!ime.is_active());

    // `Ime::Enabled` → `start_composition`.
    ime.start_composition();
    assert!(ime.is_active());
    assert_eq!(ime.preedit(), "");
    assert_eq!(ime.committed(), "");
}

/// N22b (2): `Ime::Preedit` events update the preedit via the UI; the
/// preedit is UI-only state, readable while composing and never sent.
#[test]
fn ime_ui_preedit() {
    let mut ime = ImeState::new();
    ime.start_composition(); // `Ime::Enabled`

    // `Ime::Preedit("nihao")` → `update_preedit`.
    ime.update_preedit("nihao");
    assert_eq!(ime.preedit(), "nihao");
    assert!(ime.is_active());
    assert_eq!(ime.committed(), "");

    // A later `Ime::Preedit` replaces the text.
    ime.update_preedit("nihao2");
    assert_eq!(ime.preedit(), "nihao2");
}

/// N22b (3): the `Ime::Commit` event commits via the UI — the composed
/// preedit is produced exactly once, sent to the PTY (here: returned),
/// and the machine deactivates.
#[test]
fn ime_ui_commit() {
    let mut ime = ImeState::new();
    ime.start_composition(); // `Ime::Enabled`
    ime.update_preedit("こんにちは"); // `Ime::Preedit`

    // `Ime::Commit` → `commit`.
    let committed = ime.commit();
    assert_eq!(committed.as_deref(), Some("こんにちは"));
    assert!(!ime.is_active());
    assert_eq!(ime.preedit(), "");
    assert_eq!(ime.committed(), "こんにちは");

    // A second `Ime::Commit` with no active composition sends nothing.
    assert_eq!(ime.commit(), None);
}

/// N22b (4): the `Ime::Disabled` event cancels via the UI — the active
/// flag, preedit and committed text are all cleared without sending.
#[test]
fn ime_ui_cancel() {
    let mut ime = ImeState::new();
    ime.start_composition(); // `Ime::Enabled`
    ime.update_preedit("abc"); // `Ime::Preedit`

    // `Ime::Disabled` → `cancel`.
    ime.cancel();
    assert!(!ime.is_active());
    assert_eq!(ime.preedit(), "");
    assert_eq!(ime.committed(), "");
    assert_eq!(ime.commit(), None);
}

/// N22b (5): committing when the IME is disabled (no active composition)
/// returns `None` — nothing is sent to the PTY.
#[test]
fn ime_ui_disabled() {
    let mut ime = ImeState::new();
    // No `Ime::Enabled` has arrived; the machine is inactive.
    assert_eq!(ime.commit(), None);

    // After a `Ime::Disabled` (cancel), a commit also returns `None`.
    ime.start_composition();
    ime.update_preedit("abc");
    ime.cancel(); // `Ime::Disabled`
    assert_eq!(ime.commit(), None);
}
