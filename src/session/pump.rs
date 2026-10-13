//! One live-loop tick against a real PTY session (N08b2a).
//!
//! [`pump_tick`] drains the session's event reports and, while the
//! session lives, probes the engine for pending damage: it fetches a
//! snapshot only when the damage is not the pinned cursor-cell no-op
//! shape. The tick does no I/O of its own (the session loop thread owns
//! PTY I/O), no clocks, no global state, no new dependencies; borrows
//! are sequential `&mut` calls, so the term mutex is never held across
//! anything but the pinned engine calls.

use std::process::ExitStatus;

use super::pty::{PtySession, SessionEvent};
use crate::engine::Snapshot;
use crate::render::term::is_noop_damage;

/// The outcome of one [`pump_tick`].
#[derive(Clone, Debug)]
pub struct PumpOutcome {
    /// The session loop has ended.
    pub terminated: bool,
    /// `Some` on a clean child exit, `None` when the loop died from an
    /// I/O or registration error (mirrors `SessionEvent::Terminated`).
    pub status: Option<ExitStatus>,
    /// A snapshot fetched this tick (damage was non-noop), if any.
    pub snapshot: Option<Snapshot>,
}

/// Run one tick: (1) drain `session.poll_events()` — the first
/// `SessionEvent::Terminated` sets `terminated` + `status`; (2) if not
/// terminated, probe the engine and, only when the damage is not the
/// pinned no-op shape, fetch a snapshot.
pub fn pump_tick(session: &mut PtySession) -> PumpOutcome {
    let mut terminated = false;
    let mut status: Option<ExitStatus> = None;
    for event in session.poll_events() {
        if terminated {
            continue;
        }
        match event {
            SessionEvent::Terminated { status: s, .. } => {
                terminated = true;
                status = s;
            }
        }
    }

    let mut snapshot = None;
    if !terminated {
        let (damage, cursor) = session.engine().probe();
        if !is_noop_damage(&damage, cursor) {
            snapshot = Some(session.engine().snapshot());
        }
    }

    PumpOutcome {
        terminated,
        status,
        snapshot,
    }
}
