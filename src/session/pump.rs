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
use std::time::{Duration, Instant};

use super::pty::{DRAIN_TIMEOUT, PtySession, SessionEvent};
use crate::engine::{KittyKeyboardFlags, Snapshot};
use crate::input::paste::{PasteValidation, validate_paste};
use crate::render::term::is_noop_damage;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::TermMode;

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
    /// The engine's live `TermMode` changed (N10b); the window updates its
    /// `encode` mode source.
    Modes(TermMode),
    /// The engine's live kitty keyboard flags changed (N21b); the window
    /// updates its `encode_kitty` flag source.
    KittyModes(KittyKeyboardFlags),
    /// The selected text was copied (N14b); the window thread copies it to
    /// the clipboard (or debug-prints it for now).
    Copied(String),
}

/// Window→pump message.
pub enum PumpMsg {
    /// Resize the PTY session to (rows, cols) and re-pump.
    Resize(usize, usize),
    /// Encoded keystroke bytes; window→PTY, ordered after earlier messages.
    Input(Vec<u8>),
    /// Scroll the engine's viewport (N14a); applied on the next tick.
    Scroll(Scroll),
    /// Select the viewport range
    /// `(start_row, start_col, end_row, end_col)` (N14b); applied on the
    /// next tick.
    Select(usize, usize, usize, usize),
    /// Clear the selection (N14b); applied on the next tick.
    ClearSelection,
    /// Copy the selection and emit [`AppEvent::Copied`] (N14b); applied on
    /// the next tick.
    Copy,
    /// A paste operation (N17b): validated by the pure paste policy
    /// (size cap, bracketed paste markers, newline normalization) and
    /// queued into the pending input, streamed to the PTY in order with
    /// the keystrokes, bounded by the writer's app cap.
    Paste {
        /// The paste text (raw, before validation).
        text: String,
        /// Whether bracketed paste mode is active (the engine's live
        /// `TermMode::BRACKETED_PASTE`).
        bracketed: bool,
    },
    /// Cancel the in-flight paste (N17b): the not-yet-fed paste bytes are
    /// dropped; keystrokes are unaffected.
    CancelPaste,
}

/// The outcome of applying a paste to the pump's input state (N17b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteApply {
    /// The paste was validated and queued; the count of bytes queued.
    Queued(usize),
    /// The paste exceeded `crate::input::paste::MAX_PASTE_SIZE`; the final
    /// size and the cap.
    TooLarge { size: usize, max: usize },
}

/// Apply a paste to the pump's input state (N17b): validate with
/// [`validate_paste`], and on success append the bytes to
/// `pending_input` and record the in-flight block as the half-open range
/// `[start, end)`. On rejection, `pending_input` and `paste_block` are
/// left unchanged.
///
/// A new paste replaces any previously tracked block: only the most
/// recent paste is cancellable.
pub fn apply_paste(
    pending_input: &mut Vec<u8>,
    paste_block: &mut Option<(usize, usize)>,
    text: &str,
    bracketed: bool,
) -> PasteApply {
    match validate_paste(text, bracketed) {
        PasteValidation::Ok { bytes, .. } => {
            let start = pending_input.len();
            pending_input.extend_from_slice(&bytes);
            *paste_block = Some((start, pending_input.len()));
            PasteApply::Queued(bytes.len())
        }
        PasteValidation::TooLarge { size, max } => PasteApply::TooLarge { size, max },
    }
}

/// Cancel the in-flight paste (N17b): remove its bytes from
/// `pending_input` and clear the block. Keystrokes are unaffected.
/// Returns the count of bytes dropped.
pub fn cancel_paste(
    pending_input: &mut Vec<u8>,
    paste_block: &mut Option<(usize, usize)>,
) -> usize {
    match paste_block.take() {
        Some((start, end)) => {
            let dropped = end - start;
            pending_input.drain(start..end);
            dropped
        }
        None => 0,
    }
}

/// Advance the in-flight paste block after `drained` bytes were fed from
/// the front of `pending_input` (N17b). The block shifts left by
/// `drained`; when it is fully drained it becomes `None`.
pub fn advance_paste_block(paste_block: &mut Option<(usize, usize)>, drained: usize) {
    if let Some((start, end)) = *paste_block {
        let start = start.saturating_sub(drained);
        let end = end.saturating_sub(drained);
        *paste_block = if start == end {
            None
        } else {
            Some((start, end))
        };
    }
}

/// Feed `pending_input` to the session through the bounded writer, in
/// order, until the budget is full, the input is empty, or the loop is
/// dead (N09b, extended for N17b paste tracking). Advances
/// `paste_block` as bytes drain, so an in-flight paste's block stays
/// aligned with `pending_input`. Returns the total bytes fed this call.
pub fn feed_pending(
    session: &mut PtySession,
    pending_input: &mut Vec<u8>,
    paste_block: &mut Option<(usize, usize)>,
) -> usize {
    let mut fed = 0;
    while !pending_input.is_empty() {
        match session.feed(pending_input) {
            Ok(0) => break,
            Ok(accepted) => {
                pending_input.drain(..accepted);
                advance_paste_block(paste_block, accepted);
                fed += accepted;
                if debug_enabled() {
                    eprintln!("debug: pump: fed {accepted} input byte(s)");
                }
            }
            Err(_) => {
                pending_input.clear();
                *paste_block = None;
                break;
            }
        }
    }
    fed
}

/// Bounded pump thread: owns the session; ticks until terminated or stopped.
///
/// Each tick: (1) [`pump_tick`]; (2) on the FIRST tick with
/// `terminated == true`: enter N12a drain mode — tick
/// `wait_drain(Duration::from_millis(10))` + probe + snapshot (non-noop
/// only, the windowed final frame) until the session loop thread
/// finished or [`DRAIN_TIMEOUT`] elapsed — then send
/// [`AppEvent::ChildExit`], `session.close()`, break; (3) if a snapshot
/// was fetched: store it in the slot and send
/// [`AppEvent::DirtyPulse`]; (4) if the engine's live `TermMode` differs
/// from the last published one (the first tick always publishes,
/// correcting the window's initial `SHOW_CURSOR`): send
/// [`AppEvent::Modes`]; (5) drain [`PumpMsg`] commands — resize the
/// session, scroll the viewport, extend the pending input with encoded
/// keystroke bytes, queue a validated paste (N17b), or cancel the
/// in-flight paste;
/// (6) feed the pending input in order (bounded by the writer's app cap;
/// `Ok(0)` = budget full, remainder waits for the next tick;
/// `Err(Closed)` = loop dead, drop the remainder) and flush up to the app
/// cap per tick; (7) if `stop` is set: `session.close()` and break; else
/// sleep 10 ms.
pub fn pump_thread(
    mut session: PtySession,
    slot: Arc<Mutex<Option<Snapshot>>>,
    events: winit::event_loop::EventLoopProxy<AppEvent>,
    commands: mpsc::Receiver<PumpMsg>,
    stop: Arc<AtomicBool>,
) {
    let mut pending_input: Vec<u8> = Vec::new();
    // N17b: the in-flight paste block within `pending_input`, as a
    // half-open byte range `[start, end)`. `None` when no paste is in
    // flight. Kept aligned with `pending_input` by `feed_pending`.
    let mut paste_block: Option<(usize, usize)> = None;
    // N10b: the last published engine `TermMode`. `None` until the first tick,
    // so the first tick always publishes (correcting the window's initial
    // `SHOW_CURSOR` to the engine's live mode).
    let mut last_modes: Option<TermMode> = None;
    // N21b: the last published engine kitty keyboard flags. `None` until
    // the first tick, so the first tick always publishes (correcting
    // the window's initial legacy `KittyKeyboardFlags::NONE` to the
    // engine's live flags).
    let mut last_kitty: Option<KittyKeyboardFlags> = None;
    loop {
        let outcome = pump_tick(&mut session);

        if outcome.terminated {
            // N06b: surface the last title set by the child, on stderr,
            // before the loop folds `ChildExited`.
            if let Some(title) = session.title() {
                eprintln!("title: {title}");
            }

            // N12a drain mode: the session loop thread is still
            // finishing (the pinned `drain_on_exit` read). Keep ticking
            // probe + snapshot only — no input, no resize — until the
            // loop thread finished (`wait_drain` per tick) or
            // `DRAIN_TIMEOUT` elapsed. A snapshot is fetched only when
            // the damage is not the pinned no-op shape (the final
            // frame), stored in the slot and pulsed; the engine guard
            // is dropped before any channel work.
            let drain_deadline = Instant::now() + DRAIN_TIMEOUT;
            let mut finished = false;
            while !finished {
                // One bounded wait per tick. When `wait_drain` returns
                // true the loop thread is done, but the probe still
                // runs once more: the final frame is fetched after the
                // loop thread's last parse.
                finished = session.wait_drain(Duration::from_millis(10));
                let stored = {
                    let (damage, cursor) = session.engine().probe();
                    if !is_noop_damage(&damage, cursor) {
                        let snapshot = session.engine().snapshot();
                        let (rows, cols) = (snapshot.rows, snapshot.cols);
                        *slot.lock().unwrap() = Some(snapshot);
                        if debug_enabled() {
                            eprintln!("debug: pump: stored snapshot rows={rows} cols={cols}");
                        }
                        true
                    } else {
                        false
                    }
                };
                if stored {
                    let _ = events.send_event(AppEvent::DirtyPulse);
                }
                if !finished && Instant::now() >= drain_deadline {
                    eprintln!("nora: final drain timed out; proceeding to close");
                    break;
                }
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

        // N10b: publish the engine's live `TermMode` on change. `TermMode`
        // is `Copy`, so the engine guard is dropped before the channel
        // send — never held across it.
        let modes = session.engine().modes();
        if last_modes != Some(modes) {
            last_modes = Some(modes);
            let _ = events.send_event(AppEvent::Modes(modes));
        }

        // N21b: publish the engine's live kitty keyboard flags on
        // change. `KittyKeyboardFlags` is `Copy`, so the engine guard
        // is dropped before the channel send — never held across it.
        let kitty_flags = session.engine().kitty_mode();
        if last_kitty != Some(kitty_flags) {
            last_kitty = Some(kitty_flags);
            let _ = events.send_event(AppEvent::KittyModes(kitty_flags));
        }

        while let Ok(msg) = commands.try_recv() {
            match msg {
                PumpMsg::Resize(rows, cols) => {
                    if debug_enabled() {
                        eprintln!("debug: pump: resize message {rows}x{cols}");
                    }
                    session.resize(rows as u16, cols as u16);
                }
                PumpMsg::Input(bytes) => pending_input.extend(bytes),
                PumpMsg::Scroll(scroll) => {
                    if debug_enabled() {
                        eprintln!("debug: pump: scroll {scroll:?}");
                    }
                    session.engine().scroll(scroll);
                }
                PumpMsg::Select(start_row, start_col, end_row, end_col) => {
                    if debug_enabled() {
                        eprintln!(
                            "debug: pump: select ({start_row},{start_col})..({end_row},{end_col})"
                        );
                    }
                    session
                        .engine()
                        .select((start_row, start_col), (end_row, end_col));
                }
                PumpMsg::ClearSelection => {
                    if debug_enabled() {
                        eprintln!("debug: pump: clear selection");
                    }
                    session.engine().clear_selection();
                }
                PumpMsg::Copy => {
                    if let Some(text) = session.engine().copy_selection() {
                        if debug_enabled() {
                            eprintln!("debug: pump: copied {} byte(s)", text.len());
                        }
                        let _ = events.send_event(AppEvent::Copied(text));
                    }
                }
                PumpMsg::Paste { text, bracketed } => {
                    let applied =
                        apply_paste(&mut pending_input, &mut paste_block, &text, bracketed);
                    match applied {
                        PasteApply::Queued(n) => {
                            if debug_enabled() {
                                eprintln!("debug: pump: paste queued {n} byte(s)");
                            }
                        }
                        PasteApply::TooLarge { size, max } => {
                            if debug_enabled() {
                                eprintln!(
                                    "debug: paste rejected: {size} byte(s) exceeds max {max}"
                                );
                            }
                        }
                    }
                }
                PumpMsg::CancelPaste => {
                    let dropped = cancel_paste(&mut pending_input, &mut paste_block);
                    if debug_enabled() {
                        eprintln!("debug: pump: paste cancelled, dropped {dropped} byte(s)");
                    }
                }
            }
        }

        // N09b: drain pending input in order. Bounded by the writer's app
        // cap: `feed` accepts at most the cap per call, so `Ok(0)` means the
        // budget is full and the remainder waits for the next tick;
        // `Err(Closed)` means the session loop died — drop the remainder.
        // N17b: paste bytes flow through the same bounded path as
        // keystrokes (in order, capped), and the in-flight paste block is
        // advanced as bytes drain.
        feed_pending(&mut session, &mut pending_input, &mut paste_block);
        // Per-tick flush: forwards accepted bytes (including ledger replies,
        // which have no other flusher in windowed mode) — the full app cap
        // per tick; empty flushes are cheap.
        let _ = session.flush(1024 * 1024);

        if stop.load(Ordering::Relaxed) {
            // N12a: close before break — the pinned loop panics in
            // `PeekableReceiver::recv` when its sender is dropped
            // without `Msg::Shutdown` (alacritty_terminal 0.26.0
            // event_loop.rs:483-488); `close()` sends it.
            session.close();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
