//! N07a regression tests: owned visible snapshots for the headless
//! terminal adapter (`nora::engine`) — cell extras (zerowidth, underline
//! color, hyperlink), the per-capture epoch, resize damage and the
//! two-slot [`SnapshotPool`] (`nora::render::snapshot`).
//!
//! Headless and deadline-bounded: no PTY, no threads, no sleeps.

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, Rgb};

use nora::engine::{CellText, Damage, Engine, LineDamage, Snapshot};
use nora::render::snapshot::SnapshotPool;

/// Cell at (row, col) in a row-major snapshot.
fn cell(snap: &Snapshot, row: usize, col: usize) -> &CellText {
    &snap.cells[row * snap.cols + col]
}

/// The first snapshot of a fresh engine consumes the initial full damage
/// (the pinned `Term` starts fully damaged).
fn first(engine: &mut Engine) -> Snapshot {
    let snap = engine.snapshot();
    assert_eq!(snap.damage, Damage::Full);
    snap
}

#[test]
fn first_snapshot_of_fresh_engine_is_full() {
    let mut engine = Engine::new(24, 80);
    let snap = first(&mut engine);
    assert_eq!(snap.epoch, 1);
    assert_eq!(snap.rows, 24);
    assert_eq!(snap.cols, 80);
}

#[test]
fn noop_snapshot_reports_cursor_row_only() {
    let mut engine = Engine::new(24, 80);
    first(&mut engine);

    // No writes, no cursor movement: the pinned `Term::damage` always
    // re-damages the cursor cell (alacritty_terminal-0.26.0 term/mod.rs:
    // 477-485), and that is the only damage — row 0, a single-column span
    // at the cursor.
    let snap = engine.snapshot();
    assert_eq!(
        snap.damage,
        Damage::Partial(vec![LineDamage {
            line: 0,
            left: 0,
            right: 0
        }])
    );
}

#[test]
fn cursor_move_without_writes_damages_old_and_new_rows() {
    let mut engine = Engine::new(24, 80);
    first(&mut engine);

    // CUP to row 5, column 10 (1-based) => viewport (4, 9), no writes.
    engine.feed(b"\x1b[5;10H");

    let snap = engine.snapshot();
    assert_eq!(
        snap.damage,
        Damage::Partial(vec![
            LineDamage {
                line: 0,
                left: 0,
                right: 0
            },
            LineDamage {
                line: 4,
                left: 9,
                right: 9
            },
        ])
    );
    assert_eq!(snap.cursor, Some((4, 9)));
}

#[test]
fn fresh_24x80_snapshot_has_exactly_1920_cells() {
    let mut engine = Engine::new(24, 80);
    let snap = first(&mut engine);
    assert_eq!(snap.cells.len(), 24 * 80);

    // A plain ASCII cell carries no extras.
    let plain = &snap.cells[0];
    assert!(plain.zerowidth.is_empty());
    assert_eq!(plain.underline_color, None);
    assert_eq!(plain.hyperlink, None);
    assert_eq!(plain.width(), 1);
}

#[test]
fn resize_marks_full_and_updates_dimensions() {
    let mut engine = Engine::new(10, 20);
    first(&mut engine);

    // The pinned `Term::resize` sets full damage (alacritty_terminal-0.26.0
    // term/mod.rs:699-701).
    engine.resize(24, 80);

    let snap = engine.snapshot();
    assert_eq!(snap.damage, Damage::Full);
    assert_eq!(snap.rows, 24);
    assert_eq!(snap.cols, 80);
    assert_eq!(snap.cells.len(), 24 * 80);
    assert_eq!(engine.rows(), 24);
    assert_eq!(engine.cols(), 80);
}

#[test]
fn osc4_palette_change_reports_full_and_updates_colors() {
    let mut engine = Engine::new(24, 80);
    let first = first(&mut engine);
    assert_eq!(first.colors[1], None);

    // OSC 4 color set, four-hex-digit form (vte-0.15.0 ansi.rs:1363-1386).
    // The pinned `Term::set_color` marks the terminal fully damaged on
    // change (alacritty_terminal-0.26.0 term/mod.rs:1662-1671).
    engine.feed(b"\x1b]4;1;rgb:ff/00/00\x07");

    let snap = engine.snapshot();
    assert_eq!(snap.damage, Damage::Full);
    assert_eq!(snap.colors[1], Some(Rgb { r: 255, g: 0, b: 0 }));
}

#[test]
fn hyperlink_extra_captured_owned() {
    let mut engine = Engine::new(4, 80);

    // OSC 8 (vte-0.15.0 ansi.rs:1390-1419): `\x1b]8;id=abc;URI\x07` sets
    // the link for subsequent characters; `\x1b]8;;\x07` (empty URI)
    // clears it.
    engine.feed(b"\x1b]8;id=abc;https://example.com\x07A\x1b]8;;\x07B");

    let snap = first(&mut engine);
    assert_eq!(cell(&snap, 0, 0).ch, 'A');
    assert_eq!(
        cell(&snap, 0, 0).hyperlink,
        Some(("abc".to_string(), "https://example.com".to_string()))
    );
    assert_eq!(cell(&snap, 0, 1).ch, 'B');
    assert_eq!(cell(&snap, 0, 1).hyperlink, None);
}

#[test]
fn underline_color_extra_captured() {
    let mut engine = Engine::new(4, 80);

    // SGR `58;2;r;g;b` (vte-0.15.0 ansi.rs:1881-1888 maps it to
    // `Attr::UnderlineColor`; the pinned term applies it through the
    // cursor template, alacritty_terminal-0.26.0 term/mod.rs:1887).
    engine.feed(b"\x1b[58;2;255;0;128mX");

    let snap = first(&mut engine);
    assert_eq!(cell(&snap, 0, 0).ch, 'X');
    assert_eq!(
        cell(&snap, 0, 0).underline_color,
        Some(Color::Spec(Rgb {
            r: 255,
            g: 0,
            b: 128
        }))
    );
    assert_eq!(cell(&snap, 0, 1).underline_color, None);
}

#[test]
fn zerowidth_extra_captured_on_wide_cell() {
    let mut engine = Engine::new(4, 80);

    // A width-0 character follows its fullwidth base; the pinned
    // `Term::input` width-0 path pushes it onto the preceding fullwidth
    // cell (alacritty_terminal-0.26.0 term/mod.rs:1059-1083).
    engine.feed("あ\u{301}".as_bytes());

    let snap = first(&mut engine);
    let base = cell(&snap, 0, 0);
    assert_eq!(base.ch, '\u{3042}');
    assert!(base.flags.contains(Flags::WIDE_CHAR));
    assert_eq!(base.zerowidth, vec!['\u{301}']);
    assert_eq!(base.width(), 2);

    // The wide-char spacer cell is width 0 and carries no extras.
    let spacer = cell(&snap, 0, 1);
    assert_eq!(spacer.width(), 0);
    assert!(spacer.zerowidth.is_empty());
}

#[test]
fn epoch_increments_per_capture_and_is_excluded_from_equality() {
    let mut engine = Engine::new(4, 8);
    let first = engine.snapshot();
    let second = engine.snapshot();
    let third = engine.snapshot();

    assert_eq!(first.epoch, 1);
    assert_eq!(second.epoch, 2);
    assert_eq!(third.epoch, 3);

    // Two no-op captures of the same screen are content-identical even
    // though their epochs differ: epoch is excluded from `PartialEq`
    // (content identity, not capture identity).
    assert_eq!(second, third);
    assert_ne!(second.epoch, third.epoch);
}

#[test]
fn pool_starts_empty() {
    let pool = SnapshotPool::new();
    assert_eq!(pool.len(), 0);
    assert_eq!(pool.current(), None);
    assert_eq!(pool.spare(), None);
}

#[test]
fn pool_reuses_slots_and_never_exceeds_two() {
    let mut engine = Engine::new(2, 4);
    let a = first(&mut engine); // epoch 1
    engine.feed(b"X");
    let b = engine.snapshot(); // epoch 2, cell (0, 0) = 'X'
    let c = engine.snapshot(); // epoch 3, no content change
    let (a_copy, b_copy, c_copy) = (a.clone(), b.clone(), c.clone());

    let mut pool = SnapshotPool::new();

    // Empty -> 1: storing fills the current slot.
    assert_eq!(pool.store_current(a), None);
    assert_eq!(pool.len(), 1);
    assert_eq!(pool.current(), Some(&a_copy));
    assert_eq!(pool.spare(), None);

    // Storing again reuses the current slot and hands back the previous
    // snapshot instead of growing.
    let prev = pool.store_current(b).expect("previous current");
    assert_eq!(pool.len(), 1);
    assert_eq!(pool.current(), Some(&b_copy));
    // The superseded snapshot's owned content is unchanged.
    assert_eq!(prev, a_copy);

    // The swap path exercises both slots: the returned snapshot goes
    // current, the previous current becomes the spare replacement.
    pool.swap();
    pool.store_current(prev);
    assert_eq!(pool.len(), 2);
    assert_eq!(pool.current(), Some(&a_copy));
    assert_eq!(pool.spare(), Some(&b_copy));

    // A third store still never grows the pool: it replaces current and
    // hands back the superseded snapshot.
    let prev = pool.store_current(c).expect("previous current");
    assert_eq!(pool.len(), 2);
    assert_eq!(pool.current(), Some(&c_copy));
    assert_eq!(pool.spare(), Some(&b_copy));
    assert_eq!(prev, a_copy);
}
