//! One live-loop tick against a real PTY session (N08b2a).
//!
//! [`pump_tick`] drains the session's event reports and, while the
//! session lives, probes the engine for pending damage: it fetches a
//! snapshot only when the damage is not the pinned cursor-cell no-op
//! shape. The tick does no I/O of its own (the session loop thread owns
//! PTY I/O), no clocks, no global state, no new dependencies; borrows
//! are sequential `&mut` calls, so the term mutex is never held across
//! anything but the pinned engine calls.

use std::collections::HashMap;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use super::pty::{DRAIN_TIMEOUT, LaunchSpec, PtySession, SessionEvent};
use crate::engine::{KittyKeyboardFlags, Snapshot};
use crate::input::paste::{PasteValidation, validate_paste};
use crate::render::term::is_noop_damage;
use crate::workspace::TabId;
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
///
/// Every event is tagged with the [`TabId`] it belongs to (N23b); the window
/// routes it to that tab and ignores events for a tab that no longer exists
/// (stale).
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// The pump fetched a snapshot for `tab` (stored in that tab's slot);
    /// non-noop damage.
    DirtyPulse { tab: TabId },
    /// The session child for `tab` exited (exit code, or None if the loop
    /// died without one).
    ChildExit { tab: TabId, status: Option<i32> },
    /// The engine's live `TermMode` for `tab` changed (N10b); the window
    /// updates that tab's `encode` mode source.
    Modes { tab: TabId, modes: TermMode },
    /// The engine's live kitty keyboard flags for `tab` changed (N21b); the
    /// window updates that tab's `encode_kitty` flag source.
    KittyModes {
        tab: TabId,
        flags: KittyKeyboardFlags,
    },
    /// The selected text for `tab` was copied (N14b); the window thread
    /// copies it to the clipboard (or debug-prints it for now).
    Copied { tab: TabId, text: String },
    /// A new tab's session is ready (N23b); the window may now route input
    /// to it.
    TabReady { tab: TabId },
    /// A tab's session was reaped (N23b); the window drops the tab's state.
    TabGone { tab: TabId },
}

/// Window→pump message.
///
/// Every message is tagged with the [`TabId`] it targets (N23b); the pump
/// routes it to that tab's session, so active input never reaches another
/// tab.
pub enum PumpMsg {
    /// Resize the PTY session for `tab` to (rows, cols) and re-pump.
    Resize {
        tab: TabId,
        rows: usize,
        cols: usize,
    },
    /// Encoded keystroke bytes for `tab`; window→PTY, ordered after earlier
    /// messages.
    Input { tab: TabId, bytes: Vec<u8> },
    /// Scroll the engine's viewport for `tab` (N14a); applied on the next
    /// tick.
    Scroll { tab: TabId, scroll: Scroll },
    /// Select the viewport range
    /// `(start_row, start_col, end_row, end_col)` for `tab` (N14b); applied
    /// on the next tick.
    Select {
        tab: TabId,
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    },
    /// Clear the selection for `tab` (N14b); applied on the next tick.
    ClearSelection { tab: TabId },
    /// Copy the selection for `tab` and emit [`AppEvent::Copied`] (N14b);
    /// applied on the next tick.
    Copy { tab: TabId },
    /// A paste operation for `tab` (N17b): validated by the pure paste
    /// policy (size cap, bracketed paste markers, newline normalization)
    /// and queued into the tab's pending input, streamed to the PTY in
    /// order with the keystrokes, bounded by the writer's app cap.
    Paste {
        tab: TabId,
        /// The paste text (raw, before validation).
        text: String,
        /// Whether bracketed paste mode is active (the engine's live
        /// `TermMode::BRACKETED_PASTE`).
        bracketed: bool,
    },
    /// Cancel the in-flight paste for `tab` (N17b): the not-yet-fed paste
    /// bytes are dropped; keystrokes are unaffected.
    CancelPaste { tab: TabId },
    /// Spawn a new session for `tab` (N23b); the pump uses its launch spec
    /// and the tab's slot.
    NewTab {
        tab: TabId,
        rows: usize,
        cols: usize,
        slot: Arc<Mutex<Option<Snapshot>>>,
    },
    /// Reap (close) the session for `tab` (N23b); the pump sends
    /// [`AppEvent::TabGone`].
    CloseTab { tab: TabId },
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

/// Per-tab pump state (N23b): the session and its in-flight input/paste
/// state, so each tab's input is routed to its own session and never
/// reaches another tab.
struct TabPump {
    session: PtySession,
    /// Pending input bytes for this tab (N09b/N17b).
    pending_input: Vec<u8>,
    /// The in-flight paste block within `pending_input`, as a half-open
    /// byte range `[start, end)` (N17b); `None` when no paste is in flight.
    paste_block: Option<(usize, usize)>,
    /// The last published engine `TermMode` (N10b); `None` until the first
    /// tick, so the first tick always publishes.
    last_modes: Option<TermMode>,
    /// The last published engine kitty keyboard flags (N21b); `None` until
    /// the first tick, so the first tick always publishes.
    last_kitty: Option<KittyKeyboardFlags>,
    /// Whether this tab's session has terminated and is draining (N12a).
    draining: bool,
    /// The drain deadline (N12a); `None` when not draining.
    drain_deadline: Option<Instant>,
    /// The child's exit status, captured when first terminating (N12a).
    exit_status: Option<ExitStatus>,
}

impl TabPump {
    fn new(session: PtySession) -> Self {
        Self {
            session,
            pending_input: Vec::new(),
            paste_block: None,
            last_modes: None,
            last_kitty: None,
            draining: false,
            drain_deadline: None,
            exit_status: None,
        }
    }
}

/// Tick one tab's session (N23b): run its N12a drain state machine, store +
/// pulse a snapshot on non-noop damage, and publish its modes/kitty flags on
/// change. Returns `false` when the tab's session has been fully drained
/// and closed (the caller removes it).
fn tick_tab(
    tab: TabId,
    tab_pump: &mut TabPump,
    slot: &Arc<Mutex<Option<Snapshot>>>,
    events: &winit::event_loop::EventLoopProxy<AppEvent>,
) -> bool {
    if tab_pump.draining {
        // N12a drain mode: the session loop thread is still finishing (the
        // pinned `drain_on_exit` read). Wait, then probe + snapshot only —
        // no input, no resize — until the loop thread finished or
        // `DRAIN_TIMEOUT` elapsed. A snapshot is fetched only when the
        // damage is not the pinned no-op shape (the final frame).
        let finished = tab_pump.session.wait_drain(Duration::from_millis(10));
        let stored = {
            let (damage, cursor) = tab_pump.session.engine().probe();
            if !is_noop_damage(&damage, cursor) {
                let snapshot = tab_pump.session.engine().snapshot();
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
            let _ = events.send_event(AppEvent::DirtyPulse { tab });
        }
        let timed_out = tab_pump
            .drain_deadline
            .map(|deadline| Instant::now() >= deadline)
            .unwrap_or(false);
        if finished || timed_out {
            let status = tab_pump.exit_status.and_then(|status| status.code());
            let _ = events.send_event(AppEvent::ChildExit { tab, status });
            tab_pump.session.close();
            return false;
        }
        return true;
    }

    let outcome = pump_tick(&mut tab_pump.session);

    if outcome.terminated {
        // N06b: surface the last title set by the child, on stderr, before
        // the loop folds `ChildExited`. Enter drain mode (N12a); the drain
        // frame is probed on the next tick.
        if let Some(title) = tab_pump.session.title() {
            eprintln!("title: {title}");
        }
        tab_pump.draining = true;
        tab_pump.drain_deadline = Some(Instant::now() + DRAIN_TIMEOUT);
        tab_pump.exit_status = outcome.status;
        return true;
    }

    if let Some(snapshot) = outcome.snapshot {
        let (rows, cols) = (snapshot.rows, snapshot.cols);
        *slot.lock().unwrap() = Some(snapshot);
        if debug_enabled() {
            eprintln!("debug: pump: stored snapshot rows={rows} cols={cols}");
        }
        let _ = events.send_event(AppEvent::DirtyPulse { tab });
    }

    // N10b: publish the engine's live `TermMode` on change. `TermMode` is
    // `Copy`, so the engine guard is dropped before the channel send —
    // never held across it.
    let modes = tab_pump.session.engine().modes();
    if tab_pump.last_modes != Some(modes) {
        tab_pump.last_modes = Some(modes);
        let _ = events.send_event(AppEvent::Modes { tab, modes });
    }

    // N21b: publish the engine's live kitty keyboard flags on change.
    // `KittyKeyboardFlags` is `Copy`, so the engine guard is dropped
    // before the channel send — never held across it.
    let kitty_flags = tab_pump.session.engine().kitty_mode();
    if tab_pump.last_kitty != Some(kitty_flags) {
        tab_pump.last_kitty = Some(kitty_flags);
        let _ = events.send_event(AppEvent::KittyModes {
            tab,
            flags: kitty_flags,
        });
    }

    true
}

/// Bounded pump thread: owns one session per tab (N23b); ticks until stopped.
///
/// Each tick: (1) [`tick_tab`] for every tab — drain its events, store +
/// pulse a snapshot on non-noop damage, publish its modes/kitty flags, and
/// run its N12a drain state machine; (2) remove fully-drained tabs and send
/// [`AppEvent::TabGone`]; (3) drain [`PumpMsg`] commands, routing each to
/// its tab's session (spawn a new session on [`PumpMsg::NewTab`], reap one on
/// [`PumpMsg::CloseTab`]); (4) feed each tab's pending input in order
/// (bounded by the writer's app cap) and flush up to the app cap per tick;
/// (5) if `stop` is set: close every session and break; else sleep 10 ms.
///
/// Each tab's input is routed to its own session, so active input never
/// reaches another tab; closing a tab reaps its session.
pub fn pump_thread(
    first_tab: TabId,
    first_session: PtySession,
    first_slot: Arc<Mutex<Option<Snapshot>>>,
    spec: LaunchSpec,
    events: winit::event_loop::EventLoopProxy<AppEvent>,
    commands: mpsc::Receiver<PumpMsg>,
    stop: Arc<AtomicBool>,
) {
    let mut sessions: HashMap<TabId, TabPump> = HashMap::new();
    let mut slots: HashMap<TabId, Arc<Mutex<Option<Snapshot>>>> = HashMap::new();
    sessions.insert(first_tab, TabPump::new(first_session));
    slots.insert(first_tab, first_slot);

    loop {
        // (1) Tick every tab; collect the ones that finished draining.
        let mut removed = Vec::new();
        {
            for (tab, tab_pump) in sessions.iter_mut() {
                if !tick_tab(*tab, tab_pump, &slots[tab], &events) {
                    removed.push(*tab);
                }
            }
        }
        for tab in removed {
            sessions.remove(&tab);
            slots.remove(&tab);
            let _ = events.send_event(AppEvent::TabGone { tab });
        }

        // (2) Drain commands, routing each to its tab's session.
        while let Ok(msg) = commands.try_recv() {
            match msg {
                PumpMsg::NewTab {
                    tab,
                    rows,
                    cols,
                    slot,
                } => {
                    if debug_enabled() {
                        eprintln!("debug: pump: new tab {tab:?} {rows}x{cols}");
                    }
                    match PtySession::spawn(spec.clone(), rows, cols, super::pty::DEFAULT_CAP) {
                        Ok(session) => {
                            sessions.insert(tab, TabPump::new(session));
                            slots.insert(tab, slot);
                            let _ = events.send_event(AppEvent::TabReady { tab });
                        }
                        Err(err) => {
                            eprintln!("nora: failed to spawn tab {tab:?}: {err:?}");
                            let _ = events.send_event(AppEvent::TabGone { tab });
                        }
                    }
                }
                PumpMsg::CloseTab { tab } => {
                    if debug_enabled() {
                        eprintln!("debug: pump: close tab {tab:?}");
                    }
                    if let Some(mut tab_pump) = sessions.remove(&tab) {
                        tab_pump.session.close();
                        slots.remove(&tab);
                        let _ = events.send_event(AppEvent::TabGone { tab });
                    }
                }
                PumpMsg::Resize { tab, rows, cols } => {
                    if debug_enabled() {
                        eprintln!("debug: pump: resize {tab:?} {rows}x{cols}");
                    }
                    if let Some(tab_pump) = sessions.get_mut(&tab) {
                        tab_pump.session.resize(rows as u16, cols as u16);
                    }
                }
                PumpMsg::Input { tab, bytes } => {
                    if let Some(tab_pump) = sessions.get_mut(&tab) {
                        tab_pump.pending_input.extend(bytes);
                    }
                }
                PumpMsg::Scroll { tab, scroll } => {
                    if debug_enabled() {
                        eprintln!("debug: pump: scroll {tab:?} {scroll:?}");
                    }
                    if let Some(tab_pump) = sessions.get_mut(&tab) {
                        tab_pump.session.engine().scroll(scroll);
                    }
                }
                PumpMsg::Select {
                    tab,
                    start_row,
                    start_col,
                    end_row,
                    end_col,
                } => {
                    if debug_enabled() {
                        eprintln!(
                            "debug: pump: select {tab:?} ({start_row},{start_col})..({end_row},{end_col})"
                        );
                    }
                    if let Some(tab_pump) = sessions.get_mut(&tab) {
                        tab_pump
                            .session
                            .engine()
                            .select((start_row, start_col), (end_row, end_col));
                    }
                }
                PumpMsg::ClearSelection { tab } => {
                    if debug_enabled() {
                        eprintln!("debug: pump: clear selection {tab:?}");
                    }
                    if let Some(tab_pump) = sessions.get_mut(&tab) {
                        tab_pump.session.engine().clear_selection();
                    }
                }
                PumpMsg::Copy { tab } => {
                    if let Some(tab_pump) = sessions.get_mut(&tab)
                        && let Some(text) = tab_pump.session.engine().copy_selection()
                    {
                        if debug_enabled() {
                            eprintln!("debug: pump: copied {} byte(s)", text.len());
                        }
                        let _ = events.send_event(AppEvent::Copied { tab, text });
                    }
                }
                PumpMsg::Paste {
                    tab,
                    text,
                    bracketed,
                } => {
                    if let Some(tab_pump) = sessions.get_mut(&tab) {
                        let applied = apply_paste(
                            &mut tab_pump.pending_input,
                            &mut tab_pump.paste_block,
                            &text,
                            bracketed,
                        );
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
                }
                PumpMsg::CancelPaste { tab } => {
                    if let Some(tab_pump) = sessions.get_mut(&tab) {
                        let dropped =
                            cancel_paste(&mut tab_pump.pending_input, &mut tab_pump.paste_block);
                        if debug_enabled() {
                            eprintln!("debug: pump: paste cancelled, dropped {dropped} byte(s)");
                        }
                    }
                }
            }
        }

        // (3) Feed each tab's pending input in order (bounded by the
        // writer's app cap) and flush up to the app cap per tick. Draining
        // tabs get no new input (the child is exiting), but are still
        // flushed to forward any final ledger replies.
        for (_tab, tab_pump) in sessions.iter_mut() {
            if !tab_pump.draining {
                feed_pending(
                    &mut tab_pump.session,
                    &mut tab_pump.pending_input,
                    &mut tab_pump.paste_block,
                );
            }
            let _ = tab_pump.session.flush(1024 * 1024);
        }

        if stop.load(Ordering::Relaxed) {
            // N12a: close before break — the pinned loop panics in
            // `PeekableReceiver::recv` when its sender is dropped
            // without `Msg::Shutdown` (alacritty_terminal 0.26.0
            // event_loop.rs:483-488); `close()` sends it.
            for tab_pump in sessions.values_mut() {
                tab_pump.session.close();
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
