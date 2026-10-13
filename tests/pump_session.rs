//! N08b2a headless acceptance suite for the live-loop pump tick
//! (`nora::session::pump::pump_tick`) and the pure render policy
//! (`nora::render::policy::render_policy`).
//!
//! Acceptance source: `docs/status/PROGRESS.md`, "N08b2a acceptance".
//! Each real-PTY test launches the tagged-I/O helper bin
//! (`src/bin/nora_child.rs`, discovered via `CARGO_BIN_EXE_nora_child`)
//! through a real PTY at the headless placeholder size (24 × 80, as in
//! `src/main.rs`) and asserts the card's acceptance lines on
//! `PumpOutcome`s:
//! - the first tick after spawn fetches a snapshot (the pinned fresh
//!   term damage is non-noop), at the spawn dimensions, not terminated;
//! - an idle terminal re-damages only the cursor cell (the pinned no-op
//!   shape), so a tick after a fetched snapshot fetches nothing;
//! - the helper's `READY` write is visible in exactly one tick's
//!   non-noop snapshot; the next idle tick fetches nothing;
//! - a child exit reports `terminated` with the clean exit status, and
//!   the report is sticky: every later tick repeats it (the session
//!   reports `Terminated` on every poll after loop end);
//! - `resize(10, 40)` surfaces as the next fetched snapshot at the new
//!   rows/cols with `Damage::Full`, and idles to `None` after.
//!
//! Every wait is deadline-bounded (10 s) and polls every 10 ms — no
//! sleep guesses. The tick does no I/O of its own (the session loop
//! thread owns PTY I/O), so these tests feed nothing and only pump.

use std::thread;
use std::time::{Duration, Instant};

use nora::engine::{Damage, Snapshot};
use nora::render::policy::{clamp_grid, render_policy, snapshot_is_stale};
use nora::session::io::WriterConfig;
use nora::session::pty::{LaunchSpec, PtySession};
use nora::session::pump::{PumpOutcome, pump_tick};

/// One poll tick inside the deadline-bounded pumps.
const POLL: Duration = Duration::from_millis(10);
/// Hard deadline for every wait in this suite.
const DEADLINE: Duration = Duration::from_secs(10);

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

/// Spawn the helper with `args` through a real PTY at 24 × 80 (the
/// headless placeholder size used by `src/main.rs`).
fn spawn_helper(args: &[&str]) -> PtySession {
    PtySession::spawn(
        LaunchSpec {
            program: helper(),
            args: args.iter().map(|a| a.to_string()).collect(),
            cwd: None,
            env: Vec::new(),
        },
        24,
        80,
        cap(),
    )
    .expect("helper spawns over a real PTY")
}

/// A taken snapshot's grid as one row-major string, so any visible line
/// is a contiguous substring (mirrors `grid_text` in `tests/pty_session.rs`).
fn snapshot_text(snap: &Snapshot) -> String {
    snap.cells.iter().map(|c| c.ch).collect()
}

/// Deadline-bounded tick pump: each iteration runs one [`pump_tick`],
/// hands the outcome to `per_tick` (assertion hook), and returns `Ok`
/// with the first taken snapshot whose grid contains `marker`. Returns
/// `Err` with the last taken grid text after the deadline — never a
/// hang.
fn pump_until(
    session: &mut PtySession,
    marker: &str,
    mut per_tick: impl FnMut(&PumpOutcome),
) -> Result<Snapshot, String> {
    let deadline = Instant::now() + DEADLINE;
    let mut last_taken: Option<String>;
    loop {
        let outcome = pump_tick(session);
        per_tick(&outcome);
        last_taken = outcome.snapshot.as_ref().map(snapshot_text);
        if last_taken
            .as_deref()
            .is_some_and(|grid| grid.contains(marker))
        {
            return Ok(outcome
                .snapshot
                .expect("marker text came from a taken snapshot"));
        }
        if Instant::now() > deadline {
            return Err(format!(
                "deadline exceeded; last taken grid: {last_taken:?}"
            ));
        }
        thread::sleep(POLL);
    }
}

/// N08b2a (1): the first tick after spawn fetches a snapshot — the
/// pinned fresh `TermDamageState` starts `full: true`
/// (alacritty_terminal-0.26.0 `term/mod.rs` `TermDamageState::new`), and
/// `Term::damage` reports `Full` while that flag is set, so the first
/// probe is non-noop and the tick fetches — at the spawn dimensions,
/// not terminated.
#[test]
fn first_tick_after_spawn_takes_full_snapshot() {
    let mut session = spawn_helper(&[]);

    let outcome = pump_tick(&mut session);
    assert!(!outcome.terminated, "fresh session is live");
    assert!(outcome.status.is_none(), "no exit status while live");
    let snap = outcome
        .snapshot
        .expect("fresh term damage is non-noop (Full), so the first tick must fetch a snapshot");
    assert_eq!(
        (snap.rows, snap.cols),
        (24, 80),
        "spawn dimensions on the first fetched snapshot"
    );
    // The `full` flag is only cleared by `reset_damage`, which runs
    // inside `Engine::snapshot` — nothing has snapshotted before this
    // tick, and a racing `READY` write only expands line spans (it
    // cannot clear the flag), so the first fetched report is Full.
    assert_eq!(
        snap.damage,
        Damage::Full,
        "the first fetched snapshot is fully damaged"
    );

    session.close();
}

/// N08b2a (2): stable screen. After a tick fetched a snapshot, the
/// terminal is idle (the no-args helper blocks on stdin after `READY`),
/// so the next tick fetches nothing: the pinned cursor-cell re-damage
/// on every probe is exactly the no-op shape, and `pump_tick` only
/// fetches on non-noop damage.
#[test]
fn idle_terminal_ticks_fetch_nothing_after_first() {
    let mut session = spawn_helper(&[]);

    // Pump until the helper's startup write (`READY`) is in a taken
    // snapshot: the child then blocks on stdin and the screen is stable
    // — a bare first tick could race the startup write.
    pump_until(&mut session, "READY", |outcome| {
        assert!(
            !outcome.terminated,
            "no-args helper blocks on stdin; loop must stay alive"
        );
        if let Some(taken) = &outcome.snapshot {
            assert_eq!((taken.rows, taken.cols), (24, 80));
        }
    })
    .expect("helper printed READY within the deadline");

    let outcome = pump_tick(&mut session);
    assert!(!outcome.terminated, "helper still blocks on stdin");
    assert!(
        outcome.snapshot.is_none(),
        "cursor-cell re-damage is the pinned no-op shape, so an idle tick fetches nothing"
    );

    session.close();
}

/// N08b2a (3): the helper (no args) writes `READY` once, then blocks on
/// stdin. Exactly one tick fetches a snapshot whose grid contains it,
/// with non-noop damage; the next idle tick fetches nothing (the write's
/// damage was consumed by that one snapshot).
#[test]
fn child_write_reaches_grid_in_exactly_one_tick() {
    let mut session = spawn_helper(&[]);

    let snap = pump_until(&mut session, "READY", |outcome| {
        assert!(
            !outcome.terminated,
            "no-args helper blocks on stdin; loop must stay alive"
        );
        if let Some(taken) = &outcome.snapshot {
            assert_eq!((taken.rows, taken.cols), (24, 80));
        }
    })
    .expect("helper printed READY within the deadline");

    assert!(
        snapshot_text(&snap).contains("READY"),
        "the child's known bytes are in the grid"
    );
    // Non-noop damage: `Full` (the `READY` write raced the first tick's
    // fetch, which is always Full) or `Partial` with at least one real
    // span (the write itself).
    match &snap.damage {
        Damage::Full => {}
        Damage::Partial(spans) => assert!(!spans.is_empty(), "READY write damaged lines"),
    }

    let outcome = pump_tick(&mut session);
    assert!(!outcome.terminated, "helper still blocks on stdin");
    assert!(
        outcome.snapshot.is_none(),
        "the write was consumed by exactly one tick"
    );

    session.close();
}

/// N08b2a (4): a child exit reports `terminated == true` with
/// `status == Some(code 0)` on the first tick that observes it — a
/// clean child exit always precedes loop end, so the status is drained
/// before `Terminated` can be reported. The report is sticky: the
/// session reports `Terminated` on every poll after loop end, so every
/// later tick repeats `terminated` with the status intact.
#[test]
fn child_exit_reports_terminated_with_status_sticky() {
    let mut session = spawn_helper(&["status", "0"]);

    let deadline = Instant::now() + DEADLINE;
    loop {
        let outcome = pump_tick(&mut session);
        if outcome.terminated {
            assert_eq!(
                outcome.status.as_ref().and_then(|s| s.code()),
                Some(0),
                "clean child exit carries its exit status"
            );
            assert!(
                outcome.snapshot.is_none(),
                "a terminated tick probes nothing (pinned tick step 2)"
            );
            break;
        }
        assert!(outcome.status.is_none(), "no status before termination");
        if Instant::now() > deadline {
            panic!("deadline exceeded before the loop ended");
        }
        thread::sleep(POLL);
    }

    for _ in 0..2 {
        let outcome = pump_tick(&mut session);
        assert!(outcome.terminated, "termination is sticky");
        assert_eq!(
            outcome.status.as_ref().and_then(|s| s.code()),
            Some(0),
            "the status stays with every later Terminated report"
        );
        assert!(outcome.snapshot.is_none());
    }

    session.close();
}

/// N08b2a (5): resize through the gate. Spawn 24 × 80, `resize(10, 40)`,
/// then pump: the next fetched snapshot is at the new rows/cols with
/// `Damage::Full` (the pinned `Term::resize` marks the terminal fully
/// damaged, and only a snapshot's `reset_damage` can clear it). After
/// the helper's `READY` is consumed the terminal is idle, so the next
/// tick fetches nothing.
#[test]
fn resize_surfaces_as_full_snapshot_at_new_dims() {
    let mut session = spawn_helper(&[]);

    session.resize(10, 40);

    let mut first_taken = false;
    pump_until(&mut session, "READY", |outcome| {
        assert!(
            !outcome.terminated,
            "no-args helper blocks on stdin; loop must stay alive"
        );
        if let Some(taken) = &outcome.snapshot {
            assert_eq!(
                (taken.rows, taken.cols),
                (10, 40),
                "fetched at the new size"
            );
            if !first_taken {
                first_taken = true;
                assert_eq!(
                    taken.damage,
                    Damage::Full,
                    "resize marks the terminal fully damaged"
                );
            }
        }
    })
    .expect("helper printed READY within the deadline");

    let outcome = pump_tick(&mut session);
    assert!(!outcome.terminated, "helper still blocks on stdin");
    assert!(
        outcome.snapshot.is_none(),
        "idle terminal re-damages only the cursor cell (pinned no-op shape)"
    );

    session.close();
}

/// N08b2a (6): the pure policy — terminated ⇒ `pump == false &&
/// redraw == false`; live + snapshot taken ⇒ both true; live + no
/// snapshot ⇒ `pump == true, redraw == false`.
#[test]
fn render_policy_pure_cases() {
    for snapshot_taken in [false, true] {
        let policy = render_policy(true, snapshot_taken);
        assert!(
            !policy.pump && !policy.redraw,
            "a terminated session neither pumps nor redraws"
        );
    }

    let policy = render_policy(false, true);
    assert!(
        policy.pump && policy.redraw,
        "live tick with a fetched snapshot"
    );

    let policy = render_policy(false, false);
    assert!(
        policy.pump && !policy.redraw,
        "live tick without a fetch only pumps"
    );
}

/// N11b (1): the pure staleness tag — a fetched snapshot is stale iff
/// its geometry differs from the current window grid; the geometry
/// pair is the staleness tag in this single-window architecture.
#[test]
fn snapshot_is_stale_pure_cases() {
    assert!(
        !snapshot_is_stale(24, 80, 24, 80),
        "equal geometry is fresh"
    );
    assert!(snapshot_is_stale(25, 80, 24, 80), "rows differ: stale");
    assert!(snapshot_is_stale(24, 81, 24, 80), "cols differ: stale");
    assert!(snapshot_is_stale(25, 81, 24, 80), "both axes differ: stale");
    assert!(
        snapshot_is_stale(0, 0, 25, 66),
        "a zero-geometry snapshot is stale against a live grid"
    );
}

/// N11b (2): per axis, zero clamps to the previous grid and non-zero
/// takes the new — a tiny/minimized window must never request zero
/// columns (the PTY retains the last valid geometry).
#[test]
fn clamp_grid_pure_cases() {
    assert_eq!(
        clamp_grid(0, 0, 24, 80),
        (24, 80),
        "both axes zero: the previous pair is returned unchanged"
    );
    assert_eq!(
        clamp_grid(0, 40, 24, 80),
        (24, 40),
        "zero rows clamp to the previous rows"
    );
    assert_eq!(
        clamp_grid(12, 0, 24, 80),
        (12, 80),
        "zero cols clamp to the previous cols"
    );
    assert_eq!(
        clamp_grid(12, 40, 24, 80),
        (12, 40),
        "non-zero axes take the new geometry"
    );
}
