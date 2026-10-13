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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

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

/// `NORA_DEBUG` gates the window-loop debug lines (N08b2).
fn debug_enabled() -> bool {
    std::env::var_os("NORA_DEBUG").is_some()
}

/// Window-loop events from the pump thread (the ONLY pump→window protocol).
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// The pump fetched a snapshot (stored in the shared slot); non-noop damage.
    DirtyPulse,
    /// The session child exited (exit code, or None if the loop died without one).
    ChildExit(Option<i32>),
}

/// Window→pump message.
pub enum PumpMsg {
    /// Resize the PTY session to (rows, cols) and re-pump.
    Resize(usize, usize),
}

/// Bounded pump thread: owns the session; ticks until terminated or stopped.
///
/// Each tick: (1) [`pump_tick`]; (2) on the FIRST tick with
/// `terminated == true`: send [`AppEvent::ChildExit`], `session.close()`,
/// break; (3) if a snapshot was fetched: store it in the slot and send
/// [`AppEvent::DirtyPulse`]; (4) drain [`PumpMsg::Resize`] and resize the
/// session; (5) if `stop` is set: break; else sleep 10 ms.
pub fn pump_thread(
    mut session: PtySession,
    slot: Arc<Mutex<Option<Snapshot>>>,
    events: winit::event_loop::EventLoopProxy<AppEvent>,
    commands: mpsc::Receiver<PumpMsg>,
    stop: Arc<AtomicBool>,
) {
    loop {
        let outcome = pump_tick(&mut session);

        if outcome.terminated {
            // N06b: surface the last title set by the child, on stderr,
            // before the loop folds `ChildExited`.
            if let Some(title) = session.title() {
                eprintln!("title: {title}");
            }
            let _ = events.send_event(AppEvent::ChildExit(
                outcome.status.and_then(|status| status.code()),
            ));
            session.close();
            break;
        }

        if let Some(snapshot) = outcome.snapshot {
            let (rows, cols) = (snapshot.rows, snapshot.cols);
            *slot.lock().unwrap() = Some(snapshot);
            if debug_enabled() {
                eprintln!("debug: pump: stored snapshot rows={rows} cols={cols}");
            }
            let _ = events.send_event(AppEvent::DirtyPulse);
        }

        while let Ok(PumpMsg::Resize(rows, cols)) = commands.try_recv() {
            if debug_enabled() {
                eprintln!("debug: pump: resize message {rows}x{cols}");
            }
            session.resize(rows as u16, cols as u16);
        }

        if stop.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
