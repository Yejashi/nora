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
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use super::pty::{DRAIN_TIMEOUT, LaunchSpec, PasteSegment, PtySession, SessionEvent};
use crate::engine::Snapshot;
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
    pump_tick_with(session, true)
}

/// [`pump_tick`], snapshotting only when `want_snapshot`: the window has
/// consumed the previous one. Damage accumulates in the terminal until the
/// next snapshot, so skipping loses nothing; it keeps snapshot work (a
/// full-grid copy under the terminal lock, which stalls the PTY reader) at
/// or below the frame rate.
pub fn pump_tick_with(session: &mut PtySession, want_snapshot: bool) -> PumpOutcome {
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
    // While a synchronized update is pending the terminal may hold part of
    // its frame (a stream marker inside it parses the buffered bytes): no
    // frame until it ends or times out, which wakes the pump again.
    // (Read after the probe, which waits for a read in progress.)
    if !terminated && want_snapshot {
        let (damage, cursor) = session.engine().probe();
        let sync_pending = session.sync_pending();
        // An image change is a frame even without grid damage, and so is
        // a change at the cursor the term reports as cursor-only damage
        // (`Engine::cursor_changed`).
        if !sync_pending
            && (!is_noop_damage(&damage, cursor)
                || session.engine().cursor_changed()
                || session.engine().graphics_changed())
        {
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

/// `NORA_TRACE_SHELL`: log the shell integration's buffer reports.
fn trace_shell() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("NORA_TRACE_SHELL").is_some())
}

/// A pane's identity (N24b): its tab and its position within that tab's
/// layout. The pump owns one session per pane, so a pane is addressed by
/// both parts — two tabs may each have a `PaneId(0)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneKey {
    pub tab: TabId,
    pub pane: crate::layout::PaneId,
}

impl PaneKey {
    /// The first pane of the first tab (the initial session).
    pub fn first() -> Self {
        Self {
            tab: TabId(0),
            pane: crate::layout::PaneId(0),
        }
    }
}

/// Window-loop events from the pump thread (the ONLY pump→window protocol).
///
/// Every event is tagged with the [`PaneKey`] it belongs to (N24b); the
/// window routes it to that pane and ignores events for a pane that no
/// longer exists (stale).
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// An assistive technology asked for the accessibility tree (N37); the
    /// window sends it.
    Accessibility,
    /// The pump fetched a snapshot for the pane (stored in that pane's
    /// slot); non-noop damage.
    DirtyPulse { pane: PaneKey },
    /// The pane's title changed (OSC 0/2; `None` after a reset).
    Title {
        pane: PaneKey,
        title: Option<String>,
    },
    /// The pane's application rang the bell.
    Bell { pane: PaneKey },
    /// The inline suggestion for the pane's shell editor buffer (ADR-013):
    /// the text to show after the cursor, or `None` for none. Sent on every
    /// buffer change. `seq` is the last [`PumpMsg::Input`] number fully
    /// fed to the PTY when the buffer report was processed: the window
    /// keeps the suggestion only if it equals the last number it sent.
    Suggestion {
        pane: PaneKey,
        suffix: Option<String>,
        seq: u64,
    },
    /// The pane's search position: match `current` (1-based; 0 = none)
    /// of `total`.
    SearchStatus {
        pane: PaneKey,
        current: usize,
        total: usize,
    },
    /// The background font loader finished the full font system (sent by
    /// the window's own loader thread, not the pump).
    FontsReady,
    /// The clipboard text read for a paste into `pane` (sent by the
    /// window's short-lived clipboard reader thread, not the pump): `None`
    /// when the clipboard was unavailable, empty or over the paste cap.
    ClipboardText {
        pane: PaneKey,
        /// The read's number (`PasteGateAction::Read`).
        read: u64,
        text: Option<String>,
    },
    /// A termination signal arrived (SIGTERM, SIGHUP, SIGINT): close the
    /// window as if asked to (sent by the signal thread, not the pump).
    Terminate,
    /// The configuration file changed on disk (sent by the window's file
    /// watcher, not the pump).
    ConfigChanged,
    /// The session child for the pane exited (exit code, or None if the
    /// loop died without one).
    ChildExit { pane: PaneKey, status: Option<i32> },
    /// The engine's live `TermMode` for the pane changed (N10b); the window
    /// updates that pane's `encode` mode source.
    Modes { pane: PaneKey, modes: TermMode },
    /// The selected text for the pane was copied (N14b); the window thread
    /// copies it to the clipboard (or debug-prints it for now).
    Copied { pane: PaneKey, text: String },
    /// A new pane's session is ready (N24b); the window may now route input
    /// to it.
    PaneReady { pane: PaneKey },
    /// A pane's session was reaped (N24b); the window drops the pane's
    /// state.
    PaneGone { pane: PaneKey },
}

/// Window→pump message.
///
/// Every message is tagged with the [`PaneKey`] it targets (N24b); the pump
/// routes it to that pane's session, so active input never reaches another
/// pane (and never another tab).
pub enum PumpMsg {
    /// Resize the PTY session for the pane to (rows, cols) and re-pump.
    Resize {
        pane: PaneKey,
        rows: usize,
        cols: usize,
    },
    /// Encoded keystroke bytes for the pane; window→PTY, ordered after
    /// earlier messages. `seq` is the window's number for this input
    /// (increasing; ADR-013): suggestions are tagged with the last one fed
    /// to the PTY, so the window can drop one computed before its latest
    /// input.
    Input {
        pane: PaneKey,
        bytes: Vec<u8>,
        seq: u64,
    },
    /// Scroll the engine's viewport for the pane (N14a); applied on the
    /// next tick.
    Scroll { pane: PaneKey, scroll: Scroll },
    /// Select the viewport range
    /// `(start_row, start_col, end_row, end_col)` for the pane (N14b);
    /// applied on the next tick.
    Select {
        pane: PaneKey,
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    },
    /// Move the end of the pane's selection to viewport
    /// `(end_row, end_col)`, keeping its anchored start (a drag after its
    /// first [`PumpMsg::Select`]).
    ExtendSelection {
        pane: PaneKey,
        end_row: usize,
        end_col: usize,
    },
    /// Clear the selection for the pane (N14b); applied on the next tick.
    ClearSelection { pane: PaneKey },
    /// Copy the selection for the pane and emit [`AppEvent::Copied`]
    /// (N14b); applied on the next tick.
    Copy { pane: PaneKey },
    /// A paste operation for the pane (N17b): validated by the pure paste
    /// policy (size cap, bracketed paste markers, newline normalization)
    /// and queued into the pane's pending input, streamed to the PTY in
    /// order with the keystrokes, bounded by the writer's app cap.
    Paste {
        pane: PaneKey,
        /// The paste text (raw, before validation).
        text: String,
        /// Whether bracketed paste mode is active (the engine's live
        /// `TermMode::BRACKETED_PASTE`).
        bracketed: bool,
        /// The window's input number for this paste, as for
        /// [`PumpMsg::Input`] (stale suggestions are discarded by it).
        seq: u64,
    },
    /// Cancel the in-flight paste for the pane (N17b): the not-yet-fed
    /// paste bytes are dropped; keystrokes are unaffected.
    CancelPaste { pane: PaneKey },
    /// Spawn a new session for the pane (N24b); the pump uses its launch
    /// spec and the pane's slot.
    NewPane {
        pane: PaneKey,
        rows: usize,
        cols: usize,
        slot: Arc<Mutex<Option<Snapshot>>>,
    },
    /// Reap (close) the session for the pane (N24b); the pump sends
    /// [`AppEvent::PaneGone`].
    ClosePane { pane: PaneKey },
    /// New terminal options from a config reload: future panes use them,
    /// running panes take the cursor shape (scrollback is fixed per
    /// session).
    Configure {
        options: Box<super::pty::SessionOptions>,
    },
    /// Scroll the pane to its previous prompt (OSC 133).
    PreviousPrompt { pane: PaneKey },
    /// Select the pane's previous command output (OSC 133).
    SelectPreviousCommand { pane: PaneKey },
    /// Search the pane's scrollback and screen for `query` and show the
    /// newest match (empty query: clear).
    SearchQuery { pane: PaneKey, query: String },
    /// Show the next (older with `backwards`) match of the current query.
    SearchStep { pane: PaneKey, backwards: bool },
    /// End the search: clear its highlight and return to the bottom.
    SearchEnd { pane: PaneKey },
    /// The window's cell size in pixels changed (zoom, scale factor):
    /// reported to every child and used for new panes.
    CellSize { width: u16, height: u16 },
    /// Wake the pump: new PTY output, a child exit, or shutdown. Carries
    /// nothing; at most one is queued at a time (see [`pump_thread`]).
    Wake,
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

/// Length of each bracketed-paste marker (`CSI 200 ~` / `CSI 201 ~`) that
/// [`validate_paste`] wraps a bracketed paste in.
const BRACKET_MARKER_LEN: usize = 6;

/// The in-flight paste within a pane's `pending_input` (N17b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasteBlock {
    /// Start of the not-yet-fed part, as an index into `pending_input`.
    pub start: usize,
    /// End (exclusive) of the block in `pending_input`.
    pub end: usize,
    /// Bytes of the block already fed to the session.
    pub fed: usize,
    /// Whether the block is wrapped in bracketed-paste markers.
    pub bracketed: bool,
}

impl PasteBlock {
    /// The block's total length as validated (fed plus pending bytes).
    fn total(&self) -> usize {
        self.fed + (self.end - self.start)
    }
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
    paste_block: &mut Option<PasteBlock>,
    text: &str,
    bracketed: bool,
) -> PasteApply {
    match validate_paste(text, bracketed) {
        PasteValidation::Ok { bytes, bracketed } => {
            let start = pending_input.len();
            pending_input.extend_from_slice(&bytes);
            *paste_block = Some(PasteBlock {
                start,
                end: pending_input.len(),
                fed: 0,
                bracketed,
            });
            PasteApply::Queued(bytes.len())
        }
        PasteValidation::TooLarge { size, max } => PasteApply::TooLarge { size, max },
    }
}

/// Cancel the in-flight paste (N17b): remove its not-yet-fed text from
/// `pending_input` and clear the block. Keystrokes are unaffected.
/// Returns the count of bytes dropped.
///
/// A block nothing of which was fed is dropped whole. Once part of it
/// reached the child, the bytes needed to leave the child consistent
/// stay queued: the rest of a started start marker, the rest of a cut
/// UTF-8 character, and the end marker of a bracketed paste (dropping it
/// would leave the application in paste mode).
pub fn cancel_paste(pending_input: &mut Vec<u8>, paste_block: &mut Option<PasteBlock>) -> usize {
    let Some(block) = paste_block.take() else {
        return 0;
    };
    if block.fed == 0 {
        pending_input.drain(block.start..block.end);
        return block.end - block.start;
    }
    // Offsets below are within the block as validated.
    let total = block.total();
    let (text_start, text_end) = if block.bracketed {
        (BRACKET_MARKER_LEN, total.saturating_sub(BRACKET_MARKER_LEN))
    } else {
        (0, total)
    };
    let at = |offset: usize| block.start + offset - block.fed;
    let mut cut = block.fed.max(text_start);
    // Complete a character the fed part cut (continuation bytes).
    while cut < text_end && (pending_input[at(cut)] & 0xC0) == 0x80 {
        cut += 1;
    }
    if cut >= text_end {
        return 0;
    }
    pending_input.drain(at(cut)..at(text_end));
    text_end - cut
}

/// [`cancel_paste`], but the bytes it keeps queued (the rest of a cut
/// character or marker, a bracketed paste's end marker) stay tracked as
/// the in-flight block, so replies still wait for the paste's end
/// ([`paste_segment`]). Returns the count of bytes dropped.
pub fn cancel_paste_keeping_tail(
    pending_input: &mut Vec<u8>,
    paste_block: &mut Option<PasteBlock>,
) -> usize {
    let before = *paste_block;
    let dropped = cancel_paste(pending_input, paste_block);
    if let Some(block) = before
        && block.fed > 0
        && block.end - block.start > dropped
    {
        *paste_block = Some(PasteBlock {
            end: block.end - dropped,
            ..block
        });
    }
    dropped
}

/// Where the bytes fed so far leave the in-flight paste, for the
/// session's protocol replies: [`PasteSegment::Open`] between two payload
/// characters of a bracketed paste (or right after its start marker, or
/// right before its end marker), [`PasteSegment::Hold`] inside a marker
/// or a UTF-8 character, [`PasteSegment::Closed`] otherwise.
pub fn paste_segment(pending_input: &[u8], paste_block: Option<PasteBlock>) -> PasteSegment {
    let Some(block) = paste_block else {
        return PasteSegment::Closed;
    };
    if !block.bracketed || block.fed == 0 {
        return PasteSegment::Closed;
    }
    let total = block.total();
    if block.fed < BRACKET_MARKER_LEN || block.fed > total.saturating_sub(BRACKET_MARKER_LEN) {
        return PasteSegment::Hold;
    }
    if pending_input
        .get(block.start)
        .is_some_and(|&byte| byte & 0xC0 == 0x80)
    {
        return PasteSegment::Hold;
    }
    PasteSegment::Open
}

/// A reply closed the in-flight bracketed paste for good (the application
/// turned bracketed paste off): drop the block's end marker, so the rest
/// of the paste is sent as ordinary text.
pub fn unbracket_paste(pending_input: &mut Vec<u8>, paste_block: &mut Option<PasteBlock>) {
    let Some(block) = *paste_block else {
        return;
    };
    if !block.bracketed || block.end - block.start < BRACKET_MARKER_LEN {
        return;
    }
    pending_input.drain(block.end - BRACKET_MARKER_LEN..block.end);
    let end = block.end - BRACKET_MARKER_LEN;
    *paste_block = (end > block.start).then_some(PasteBlock {
        end,
        bracketed: false,
        ..block
    });
}

/// Whether `bytes` (one encoded key) cancel an in-flight paste: Ctrl+C
/// (`0x03`, or kitty `CSI 99;5u`) or Escape (`0x1b`, or kitty `CSI 27u`),
/// pressed (not repeated or released). Caps Lock and Num Lock are ignored
/// in the kitty modifiers.
pub fn is_paste_cancel_key(bytes: &[u8]) -> bool {
    if bytes == [0x03] || bytes == [0x1b] {
        return true;
    }
    let Some(params) = bytes
        .strip_prefix(b"\x1b[")
        .and_then(|rest| rest.strip_suffix(b"u"))
        .and_then(|params| std::str::from_utf8(params).ok())
    else {
        return false;
    };
    let mut fields = params.split(';');
    let code = fields.next();
    let (mods, event) = match fields.next() {
        None => ("1", "1"),
        Some(field) => field.split_once(':').unwrap_or((field, "1")),
    };
    if fields.next().is_some() || event != "1" {
        return false;
    }
    // Kitty modifiers are 1 + a bit set; 64 and 128 are the lock keys.
    let Some(mods) = mods.parse::<u32>().ok().and_then(|m| m.checked_sub(1)) else {
        return false;
    };
    let mods = mods & !(64 | 128);
    match code {
        Some("99") => mods == 4,
        Some("27") => mods == 0,
        _ => false,
    }
}

/// Advance the in-flight paste block after `drained` bytes were fed from
/// the front of `pending_input` (N17b). The block shifts left by
/// `drained`; when it is fully drained it becomes `None`.
pub fn advance_paste_block(paste_block: &mut Option<PasteBlock>, drained: usize) {
    if let Some(block) = *paste_block {
        let start = block.start.saturating_sub(drained);
        let end = block.end.saturating_sub(drained);
        let fed = block.fed + (drained.min(block.end) - drained.min(block.start));
        *paste_block = if start == end {
            None
        } else {
            Some(PasteBlock {
                start,
                end,
                fed,
                ..block
            })
        };
    }
}

/// Show `hit` (scroll + selection) and report the search position.
fn show_search(
    pane: PaneKey,
    pane_pump: &mut PanePump,
    hits: &[crate::engine::SearchHit],
    hit: Option<crate::engine::SearchHit>,
    events: &winit::event_loop::EventLoopProxy<AppEvent>,
) {
    match hit {
        Some(hit) => pane_pump.session.engine().show_search_hit(hit),
        None => pane_pump.session.engine().clear_selection(),
    }
    pane_pump.force_snapshot = true;
    let current = hit
        .and_then(|h| hits.iter().position(|x| *x == h))
        .map_or(0, |i| i + 1);
    let _ = events.send_event(AppEvent::SearchStatus {
        pane,
        current,
        total: hits.len(),
    });
}

/// Close a session on its own short-lived thread (it may wait for the
/// child, see [`PtySession::close`]). `None` if the thread could not be
/// spawned, in which case the session is closed inline.
fn close_in_background(mut session: PtySession) -> Option<std::thread::JoinHandle<()>> {
    let (tx, rx) = mpsc::channel::<PtySession>();
    let spawned = std::thread::Builder::new()
        .name("nora-close".to_string())
        .spawn(move || {
            if let Ok(mut session) = rx.recv() {
                session.close();
            }
        });
    match spawned {
        Ok(handle) => match tx.send(session) {
            Ok(()) => Some(handle),
            Err(mpsc::SendError(mut session)) => {
                session.close();
                None
            }
        },
        Err(_) => {
            session.close();
            None
        }
    }
}

/// Most paste bytes a pane queues in the pump (2 MiB); a paste that would
/// pass it is refused. Keyboard input may use [`INPUT_HEADROOM`] more, so
/// keys such as Ctrl+C typed behind a large paste to a child that stopped
/// reading are still accepted. With the writer budget and
/// [`super::pty::MAX_IN_FLIGHT`] this bounds a pane's input memory end to
/// end.
pub const MAX_PENDING_INPUT: usize = 2 * 1024 * 1024;

/// Extra queue bytes reserved for [`PumpMsg::Input`] (keys, mouse
/// reports, accepted suggestions) beyond [`MAX_PENDING_INPUT`]: pastes
/// can never use it.
pub const INPUT_HEADROOM: usize = 16 * 1024;

/// Queue `bytes` from a [`PumpMsg::Input`] in `pending_input`, unless that
/// would pass `MAX_PENDING_INPUT + INPUT_HEADROOM`. Returns whether the
/// bytes were accepted (refused input is never partly queued).
pub fn accept_input(pending_input: &mut Vec<u8>, bytes: &[u8]) -> bool {
    if pending_input.len() + bytes.len() <= MAX_PENDING_INPUT + INPUT_HEADROOM {
        pending_input.extend_from_slice(bytes);
        true
    } else {
        false
    }
}

/// Queue one [`PumpMsg::Input`]'s bytes (see [`accept_input`]). Ctrl+C or
/// Escape while a paste is still being fed first cancels the rest of the
/// paste ([`cancel_paste`]: the end marker stays), so the key follows the
/// end of the paste instead of waiting behind a paste the child reads
/// slowly or not at all.
pub fn queue_key_input(
    pending_input: &mut Vec<u8>,
    paste_block: &mut Option<PasteBlock>,
    bytes: &[u8],
) -> bool {
    if paste_block.is_some() && is_paste_cancel_key(bytes) {
        let dropped = cancel_paste_keeping_tail(pending_input, paste_block);
        if debug_enabled() {
            eprintln!("debug: pump: paste cancelled, dropped {dropped} byte(s)");
        }
    }
    accept_input(pending_input, bytes)
}

/// Most paste bytes a pane hands its session at a time (writer queue plus
/// bytes in flight to the PTY loop): the rest waits in the pump, so a
/// cancel drops it (see [`is_paste_cancel_key`]). At the pump's 10 ms busy
/// tick this still passes at least 6.4 MiB/s to a child that keeps up.
pub const PASTE_WINDOW: usize = 64 * 1024;

/// Feed `pending_input` to the session through the bounded writer, in
/// order, until the budget is full, the input is empty, or the loop is
/// dead (N09b, extended for N17b paste tracking). Advances
/// `paste_block` as bytes drain, so an in-flight paste's block stays
/// aligned with `pending_input`; paste bytes are held to
/// [`PASTE_WINDOW`]. Returns the total bytes fed this call.
pub fn feed_pending(
    session: &mut PtySession,
    pending_input: &mut Vec<u8>,
    paste_block: &mut Option<PasteBlock>,
) -> usize {
    let mut fed = 0;
    loop {
        // A reply closed the paste for good: the rest goes unbracketed.
        if session.take_paste_closed() {
            unbracket_paste(pending_input, paste_block);
        }
        // Replies queued from here on go around the paste's markers.
        session.set_paste_segment(paste_segment(pending_input, *paste_block));
        if pending_input.is_empty() {
            break;
        }
        // Paste bytes enter the session only while it holds less than
        // `PASTE_WINDOW` (writer plus in flight), so the rest of a paste a
        // slow child is still reading stays here, where Ctrl+C or Escape
        // can cancel it. Bytes before the paste (keys) are not held.
        let limit = match *paste_block {
            Some(block) if block.start == 0 => {
                let held = session.outstanding() + session.in_flight();
                PASTE_WINDOW.saturating_sub(held).min(block.end)
            }
            Some(block) => block.start,
            None => pending_input.len(),
        };
        if limit == 0 {
            break;
        }
        match session.feed(&pending_input[..limit]) {
            // Refused while a closed paste waits (handled above).
            Ok(0) if session.paste_closed_pending() => {}
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
                session.set_paste_segment(PasteSegment::Closed);
                break;
            }
        }
    }
    fed
}

/// Per-pane pump state (N24b): the session and its in-flight input/paste
/// state, so each pane's input is routed to its own session and never
/// reaches another pane (and never another tab).
struct PanePump {
    session: PtySession,
    /// Publish a snapshot on the next tick even without terminal damage:
    /// selection changes are not damage, yet must reach the screen.
    force_snapshot: bool,
    /// The last published title.
    last_title: Option<String>,
    /// The active search: its query and the match currently shown.
    search: Option<(String, Option<crate::engine::SearchHit>)>,
    /// Pending input bytes for this pane (N09b/N17b).
    pending_input: Vec<u8>,
    /// The in-flight paste block within `pending_input`, as a half-open
    /// byte range `[start, end)` (N17b); `None` when no paste is in flight.
    paste_block: Option<PasteBlock>,
    /// The last published engine `TermMode` (N10b); `None` until the first
    /// tick, so the first tick always publishes.
    last_modes: Option<TermMode>,
    /// Whether this pane's session has terminated and is draining (N12a).
    draining: bool,
    /// The drain deadline (N12a); `None` when not draining.
    drain_deadline: Option<Instant>,
    /// The child's exit status, captured when first terminating (N12a).
    exit_status: Option<ExitStatus>,
    /// The shell's last reported editor buffer (ADR-013), for change
    /// detection.
    shell_buffer: Option<crate::shell_input::LineBuffer>,
    /// Whether the terminal was in password input at the last buffer
    /// check (ADR-013), for change detection.
    password_input: bool,
    /// The number of the last [`PumpMsg::Input`] received for this pane.
    input_seq: u64,
    /// The number of the last input whose bytes were all fed to the
    /// session (every earlier input's too); tags suggestions.
    fed_seq: u64,
}

impl PanePump {
    fn new(session: PtySession) -> Self {
        Self {
            session,
            force_snapshot: false,
            last_title: None,
            search: None,
            pending_input: Vec::new(),
            paste_block: None,
            last_modes: None,
            draining: false,
            drain_deadline: None,
            exit_status: None,
            shell_buffer: None,
            password_input: false,
            input_seq: 0,
            fed_seq: 0,
        }
    }
}

/// Tick one pane's session (N24b): run its N12a drain state machine, store +
/// pulse a snapshot on non-noop damage, and publish its modes on
/// change. Returns `false` when the pane's session has been fully drained
/// and closed (the caller removes it).
/// Seconds since the Unix epoch (0 if the clock is before it).
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// How often the PATH executables are listed again.
const EXECUTABLES_REFRESH: Duration = Duration::from_secs(300);

/// The suggestion history and where it is saved, and the executables on
/// PATH for first-word completion (ADR-013).
struct Suggestions {
    history: crate::suggest::History,
    /// Where the history is saved; `None` when it is not saved this
    /// session (no state directory, or a file nora cannot read).
    path: Option<std::path::PathBuf>,
    last_save: Instant,
    /// The save running on a background thread (a slow `fsync` must not
    /// stall the pump), at most one at a time.
    saving: Option<std::thread::JoinHandle<std::io::Result<()>>>,
    /// Listed on a background thread (a cold scan of PATH takes hundreds
    /// of milliseconds); empty until the first listing finishes.
    executables: Arc<Mutex<Arc<Vec<String>>>>,
    executables_at: Option<Instant>,
    /// Path completions, looked up on a background thread (a dead
    /// network mount must not stall the pump), keyed by pane and input
    /// sequence number.
    paths: crate::suggest::PathCompleter<(PaneKey, u64)>,
}

impl Suggestions {
    /// Load the user's history. A file that cannot be read starts an
    /// empty history that is not saved this session (one message), so
    /// the file is never overwritten. `wake` wakes the pump when a path
    /// completion is ready.
    fn load(wake: crate::engine::Waker) -> Self {
        let mut path = crate::suggest::History::default_path();
        let loaded = path.as_deref().map(crate::suggest::History::load);
        let history = match loaded {
            Some(Ok(history)) => history,
            Some(Err(error)) => {
                if let Some(file) = path.take() {
                    eprintln!(
                        "nora: history {}: {error}; not saving history this session",
                        file.display()
                    );
                }
                crate::suggest::History::new()
            }
            None => crate::suggest::History::new(),
        };
        let mut suggestions = Self {
            history,
            path,
            last_save: Instant::now(),
            saving: None,
            executables: Arc::new(Mutex::new(Arc::new(Vec::new()))),
            executables_at: None,
            paths: crate::suggest::PathCompleter::new(wake),
        };
        suggestions.refresh_executables();
        suggestions
    }

    /// List PATH's executables again in the background when the last
    /// listing started more than [`EXECUTABLES_REFRESH`] ago.
    fn refresh_executables(&mut self) {
        if self
            .executables_at
            .is_some_and(|at| at.elapsed() < EXECUTABLES_REFRESH)
        {
            return;
        }
        self.executables_at = Some(Instant::now());
        let Some(path_var) = std::env::var_os("PATH") else {
            return;
        };
        let out = self.executables.clone();
        let _ = std::thread::Builder::new()
            .name("nora-path-scan".to_string())
            .spawn(move || {
                let names = Arc::new(crate::suggest::path_executables(&path_var));
                *out.lock().unwrap_or_else(PoisonError::into_inner) = names;
            });
    }

    /// The best suggestion for `buffer`: history first, else an
    /// unambiguous executable completion of the last word. A path
    /// completion is requested in the background under `key` instead
    /// (`None` now; [`Suggestions::paths`] returns it later).
    fn suggest(
        &mut self,
        key: (PaneKey, u64),
        buffer: &str,
        cwd: Option<&str>,
        now: u64,
    ) -> Option<String> {
        if let Some(full) = self.history.suggest(buffer, cwd, now) {
            return Some(full[buffer.len()..].to_string());
        }
        self.refresh_executables();
        let executables = self
            .executables
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let home = std::env::var("HOME").ok();
        match crate::suggest::plan_word(buffer, cwd, home.as_deref(), &executables) {
            crate::suggest::WordCompletion::Done(completion) => completion,
            crate::suggest::WordCompletion::Path(query) => {
                self.paths.request(key, query);
                None
            }
        }
    }

    /// Save when changed, at most every `interval`, on a background
    /// thread with an owned snapshot. While a save is still running this
    /// one is deferred to a later call.
    fn save_if_due(&mut self, interval: Duration) {
        if self.saving.as_ref().is_some_and(|s| s.is_finished()) {
            self.finish_save();
        }
        if self.saving.is_some() || !self.history.is_dirty() || self.last_save.elapsed() < interval
        {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        self.last_save = Instant::now();
        let mut snapshot = self.history.take_snapshot();
        match std::thread::Builder::new()
            .name("nora-history-save".to_string())
            .spawn(move || snapshot.save(&path))
        {
            Ok(handle) => self.saving = Some(handle),
            Err(error) => {
                eprintln!("nora: saving history: {error}");
                self.history.mark_dirty();
            }
        }
    }

    /// Save now on this thread, after any background save finished: the
    /// final save, which must be on disk before the process exits.
    fn save_now(&mut self) {
        self.finish_save();
        let Some(path) = self.path.clone() else {
            return;
        };
        if self.history.is_dirty()
            && let Err(error) = self.history.save(&path)
        {
            self.save_failed(&error);
        }
    }

    /// Wait for the background save, if any, and report its result.
    fn finish_save(&mut self) {
        let Some(handle) = self.saving.take() else {
            return;
        };
        let result = handle
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("the history save thread panicked")));
        if let Err(error) = result {
            self.save_failed(&error);
        }
    }

    /// Report a failed save. The history is saved again later, unless
    /// the file has become one nora cannot read (it is left alone).
    fn save_failed(&mut self, error: &std::io::Error) {
        let Some(path) = &self.path else {
            return;
        };
        if error.kind() == std::io::ErrorKind::InvalidData {
            eprintln!(
                "nora: saving history {}: {error}; not saving history this session",
                path.display()
            );
            self.path = None;
        } else {
            eprintln!("nora: saving history {}: {error}", path.display());
            self.history.mark_dirty();
        }
    }
}

impl Drop for Suggestions {
    /// A save still running finishes before the history goes away.
    fn drop(&mut self) {
        self.finish_save();
    }
}

/// ADR-013: fold the pane's executed commands into the history and, when
/// its editor buffer changed, send the window the suggestion for it. No
/// suggestion with the cursor before the end of the buffer, on the
/// alternate screen, while a paste is being delivered, or while the
/// terminal is set up for password input.
fn update_suggestion(
    pane: PaneKey,
    pane_pump: &mut PanePump,
    suggestions: &mut Option<Suggestions>,
    events: &winit::event_loop::EventLoopProxy<AppEvent>,
) {
    let now = unix_now();
    let executed = pane_pump.session.engine().take_executed();
    if let Some(s) = suggestions.as_mut() {
        for command in &executed {
            s.history
                .record(&command.cmdline, command.cwd.as_deref(), command.exit, now);
        }
    }
    let buffer = pane_pump.session.engine().shell_buffer();
    // The mode is read only while a buffer could be suggested for (one
    // `tcgetattr`); a change in it alone recomputes the suggestion.
    let password_input = buffer.is_some() && pane_pump.session.password_input();
    if buffer == pane_pump.shell_buffer && password_input == pane_pump.password_input {
        return;
    }
    let seq = pane_pump.fed_seq;
    if let Some(s) = suggestions.as_mut() {
        // A path completion still looked up for this pane is stale now.
        s.paths.discard(|&(p, _)| p == pane);
    }
    let engine = pane_pump.session.engine();
    let suffix = match (&buffer, suggestions.as_mut()) {
        (Some(b), Some(s))
            if b.cursor_at_end()
                && !password_input
                && !engine.modes().contains(TermMode::ALT_SCREEN)
                && pane_pump.paste_block.is_none() =>
        {
            s.suggest((pane, seq), &b.text, engine.cwd().as_deref(), now)
        }
        _ => None,
    };
    if trace_shell() {
        match &buffer {
            Some(b) => eprintln!("shell: buffer {:?} cursor {}", b.text, b.cursor),
            None => eprintln!("shell: buffer none"),
        }
        if password_input {
            eprintln!("shell: password input");
        }
        eprintln!("shell: suggestion {suffix:?}");
    }
    pane_pump.shell_buffer = buffer;
    pane_pump.password_input = password_input;
    // The tag says which input the buffer reflects. Residual race: a
    // report the shell wrote before it read the latest key, but that the
    // engine parsed only after the key was fed, is tagged with the key's
    // number although it shows the older buffer; the next report (the
    // key's echo) replaces it.
    let _ = events.send_event(AppEvent::Suggestion { pane, suffix, seq });
}

fn tick_pane(
    pane: PaneKey,
    pane_pump: &mut PanePump,
    slot: &Arc<Mutex<Option<Snapshot>>>,
    events: &winit::event_loop::EventLoopProxy<AppEvent>,
) -> bool {
    if pane_pump.draining {
        // N12a drain mode: the session loop thread is still finishing (the
        // pinned `drain_on_exit` read). Wait, then probe + snapshot only —
        // no input, no resize — until the loop thread finished or
        // `DRAIN_TIMEOUT` elapsed. A snapshot is fetched only when the
        // damage is not the pinned no-op shape (the final frame).
        let finished = pane_pump.session.wait_drain(Duration::from_millis(10));
        let stored = {
            let (damage, cursor) = pane_pump.session.engine().probe();
            if !is_noop_damage(&damage, cursor) || pane_pump.session.engine().cursor_changed() {
                let snapshot = pane_pump.session.engine().snapshot();
                let (rows, cols) = (snapshot.rows, snapshot.cols);
                *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(snapshot);
                if debug_enabled() {
                    eprintln!("debug: pump: stored snapshot rows={rows} cols={cols}");
                }
                true
            } else {
                false
            }
        };
        if stored {
            let _ = events.send_event(AppEvent::DirtyPulse { pane });
        }
        let timed_out = pane_pump
            .drain_deadline
            .map(|deadline| Instant::now() >= deadline)
            .unwrap_or(false);
        if finished || timed_out {
            let status = pane_pump.exit_status.and_then(|status| status.code());
            let _ = events.send_event(AppEvent::ChildExit { pane, status });
            pane_pump.session.close();
            return false;
        }
        return true;
    }

    // Backpressure: while the window still holds an undrawn snapshot of
    // this pane, do not build another (the window wakes the pump when it
    // takes one).
    let slot_free = slot
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_none();
    let outcome = pump_tick_with(&mut pane_pump.session, slot_free);

    if outcome.terminated {
        // N06b: surface the last title set by the child, on stderr, before
        // the loop folds `ChildExited`. Enter drain mode (N12a); the drain
        // frame is probed on the next tick.
        if debug_enabled()
            && let Some(title) = pane_pump.session.title()
        {
            eprintln!("title: {title}");
        }
        pane_pump.draining = true;
        pane_pump.drain_deadline = Some(Instant::now() + DRAIN_TIMEOUT);
        pane_pump.exit_status = outcome.status;
        return true;
    }

    let forced = slot_free && std::mem::take(&mut pane_pump.force_snapshot);
    let snapshot = match outcome.snapshot {
        Some(snapshot) => Some(snapshot),
        None if forced => Some(pane_pump.session.engine().snapshot()),
        None => None,
    };
    if let Some(snapshot) = snapshot {
        let (rows, cols) = (snapshot.rows, snapshot.cols);
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(snapshot);
        if debug_enabled() {
            eprintln!("debug: pump: stored snapshot rows={rows} cols={cols}");
        }
        let _ = events.send_event(AppEvent::DirtyPulse { pane });
    }

    // Publish title changes and bells for the window.
    let title = pane_pump.session.title();
    if title != pane_pump.last_title {
        pane_pump.last_title = title.clone();
        let _ = events.send_event(AppEvent::Title { pane, title });
    }
    if pane_pump.session.take_bell() {
        let _ = events.send_event(AppEvent::Bell { pane });
    }

    // N10b: publish the engine's live `TermMode` on change. `TermMode` is
    // `Copy`, so the engine guard is dropped before the channel send —
    // never held across it.
    let modes = pane_pump.session.engine().modes();
    if pane_pump.last_modes != Some(modes) {
        pane_pump.last_modes = Some(modes);
        let _ = events.send_event(AppEvent::Modes { pane, modes });
    }

    true
}

/// Bounded pump thread: owns one session per pane (N24b); ticks until
/// stopped.
///
/// Each tick: (1) [`tick_pane`] for every pane — drain its events, store +
/// pulse a snapshot on non-noop damage, publish its modes, and
/// run its N12a drain state machine; (2) remove fully-drained panes and
/// send [`AppEvent::PaneGone`]; (3) drain [`PumpMsg`] commands, routing each
/// to its pane's session (spawn a new session on [`PumpMsg::NewPane`], reap
/// one on [`PumpMsg::ClosePane`]); (4) feed each pane's pending input in
/// order (bounded by the writer's app cap) and flush up to the app cap per
/// tick; (5) if `stop` is set: close every session and break; else sleep
/// 10 ms.
///
/// Each pane's input is routed to its own session, so active input never
/// reaches another pane (and never another tab); closing a pane reaps its
/// session.
// The thread's fixed wiring: first pane (key, session, slot), the spawn
// spec for new panes, the window proxy, the command channel's two ends,
// and the stop flag.
#[allow(clippy::too_many_arguments)]
pub fn pump_thread(
    first_pane: PaneKey,
    mut first_session: PtySession,
    first_slot: Arc<Mutex<Option<Snapshot>>>,
    spec: LaunchSpec,
    events: winit::event_loop::EventLoopProxy<AppEvent>,
    commands: mpsc::Receiver<PumpMsg>,
    wake_tx: mpsc::Sender<PumpMsg>,
    stop: Arc<AtomicBool>,
    options: super::pty::SessionOptions,
) {
    let mut options = options;
    // Event-driven wakeup: each session's engine calls `waker` when the PTY
    // loop records output or a child exit; it queues one `PumpMsg::Wake`
    // unless one is already pending, so the queue never grows from wakes.
    let wake_queued = Arc::new(AtomicBool::new(false));
    let waker: crate::engine::Waker = {
        let wake_queued = wake_queued.clone();
        Arc::new(move || {
            if !wake_queued.swap(true, Ordering::AcqRel) {
                let _ = wake_tx.send(PumpMsg::Wake);
            }
        })
    };
    let mut sessions: HashMap<PaneKey, PanePump> = HashMap::new();
    let mut slots: HashMap<PaneKey, Arc<Mutex<Option<Snapshot>>>> = HashMap::new();
    first_session.engine().set_waker(waker.clone());
    sessions.insert(first_pane, PanePump::new(first_session));
    slots.insert(first_pane, first_slot);

    let mut suggestions = options
        .autocomplete
        .then(|| Suggestions::load(waker.clone()));
    let mut timeout = Duration::ZERO;
    loop {
        // (0) Block until a command, a wake (PTY output, child exit), or
        // the timeout: no fixed sleep on the echo path (audit P5).
        let first = commands.recv_timeout(timeout).ok();
        // Cleared before the tick: output that arrives while this pass
        // runs queues a fresh wake.
        wake_queued.store(false, Ordering::Release);

        // (1) Tick every pane; collect the ones that finished draining.
        let mut removed = Vec::new();
        {
            for (pane, pane_pump) in sessions.iter_mut() {
                if !tick_pane(*pane, pane_pump, &slots[pane], &events) {
                    removed.push(*pane);
                } else {
                    update_suggestion(*pane, pane_pump, &mut suggestions, &events);
                }
            }
        }
        // A path completion finished in the background (it woke the
        // pump); results made stale by a later buffer were discarded.
        if let Some(s) = suggestions.as_mut()
            && let Some(((pane, seq), suffix)) = s.paths.poll()
            && sessions.contains_key(&pane)
        {
            if trace_shell() {
                eprintln!("shell: path suggestion {suffix:?}");
            }
            let _ = events.send_event(AppEvent::Suggestion { pane, suffix, seq });
        }
        for pane in removed {
            sessions.remove(&pane);
            slots.remove(&pane);
            let _ = events.send_event(AppEvent::PaneGone { pane });
        }

        // (2) Drain commands, routing each to its pane's session.
        let pending = first
            .into_iter()
            .map(|msg| (true, msg))
            .chain(std::iter::from_fn(|| {
                commands.try_recv().ok().map(|msg| (false, msg))
            }));
        let mut commanded = false;
        for (before_tick, msg) in pending {
            // A wake that arrived after this pass's tick announces output
            // the tick may not have seen (a shell writes its buffer report
            // and then the echo, microseconds apart): tick again at once.
            // Draining it silently left that output undrawn until the next
            // key or the 500 ms heartbeat.
            commanded |= !before_tick || !matches!(msg, PumpMsg::Wake);
            match msg {
                // The wake that ended the wait: the tick above ran after it.
                PumpMsg::Wake => {}
                // The PTY filter queues prompt markers; fold them into the
                // engine's store before navigating by them.
                PumpMsg::PreviousPrompt { pane } => {
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
                        let engine = pane_pump.session.engine();
                        engine.drain_markers();
                        let _ = engine.jump_to_previous_prompt();
                        pane_pump.force_snapshot = true;
                    }
                }
                PumpMsg::SelectPreviousCommand { pane } => {
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
                        let engine = pane_pump.session.engine();
                        engine.drain_markers();
                        let _ = engine.select_previous_command();
                        pane_pump.force_snapshot = true;
                    }
                }
                PumpMsg::SearchQuery { pane, query } => {
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
                        let hits = pane_pump.session.engine().search_text(&query);
                        // Start from the newest match, as a reader scrolling
                        // up from the prompt would.
                        let shown = hits.last().copied();
                        show_search(pane, pane_pump, &hits, shown, &events);
                        pane_pump.search = Some((query, shown));
                    }
                }
                PumpMsg::SearchStep { pane, backwards } => {
                    if let Some(pane_pump) = sessions.get_mut(&pane)
                        && let Some((query, current)) = pane_pump.search.clone()
                    {
                        let hits = pane_pump.session.engine().search_text(&query);
                        let key = |h: &crate::engine::SearchHit| (h.line_id, h.start);
                        let next = match current {
                            Some(cur) if backwards => hits
                                .iter()
                                .rev()
                                .find(|h| key(h) < key(&cur))
                                .or(hits.last()),
                            Some(cur) => hits.iter().find(|h| key(h) > key(&cur)).or(hits.first()),
                            None => hits.last(),
                        }
                        .copied();
                        show_search(pane, pane_pump, &hits, next, &events);
                        pane_pump.search = Some((query, next));
                    }
                }
                PumpMsg::SearchEnd { pane } => {
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
                        pane_pump.search = None;
                        pane_pump.session.engine().clear_selection();
                        pane_pump.session.engine().scroll(Scroll::Bottom);
                        pane_pump.force_snapshot = true;
                    }
                }
                PumpMsg::CellSize { width, height } => {
                    for pane_pump in sessions.values_mut() {
                        pane_pump.session.set_cell_size(width, height);
                    }
                    options.cell_width = width;
                    options.cell_height = height;
                }
                PumpMsg::Configure { options: new } => {
                    let new = *new;
                    // Every pane rebuilds its snapshot: the window keeps
                    // idle panes' last snapshots with the old colors
                    // already applied (audit RV5).
                    for pane_pump in sessions.values_mut() {
                        pane_pump.session.set_palette(new.palette);
                        pane_pump.force_snapshot = true;
                    }
                    let new = super::pty::SessionOptions {
                        // The window's current cell size stays: only
                        // CellSize changes it.
                        cell_width: options.cell_width,
                        cell_height: options.cell_height,
                        ..new
                    };
                    if new.autocomplete != options.autocomplete {
                        if let Some(s) = suggestions.as_mut() {
                            s.save_now();
                        }
                        suggestions = new.autocomplete.then(|| Suggestions::load(waker.clone()));
                    }
                    if new.cursor_shape != options.cursor_shape {
                        for pane_pump in sessions.values_mut() {
                            pane_pump
                                .session
                                .engine()
                                .set_default_cursor_shape(new.cursor_shape);
                            pane_pump.force_snapshot = true;
                        }
                    }
                    options = new;
                }
                PumpMsg::NewPane {
                    pane,
                    rows,
                    cols,
                    slot,
                } => {
                    if debug_enabled() {
                        eprintln!("debug: pump: new pane {pane:?} {rows}x{cols}");
                    }
                    match PtySession::spawn_configured(
                        spec.clone(),
                        rows,
                        cols,
                        super::pty::DEFAULT_CAP,
                        &options,
                    ) {
                        Ok(mut session) => {
                            session.engine().set_waker(waker.clone());
                            sessions.insert(pane, PanePump::new(session));
                            slots.insert(pane, slot);
                            let _ = events.send_event(AppEvent::PaneReady { pane });
                        }
                        Err(err) => {
                            eprintln!("nora: failed to spawn pane {pane:?}: {err:?}");
                            let _ = events.send_event(AppEvent::PaneGone { pane });
                        }
                    }
                }
                PumpMsg::ClosePane { pane } => {
                    if debug_enabled() {
                        eprintln!("debug: pump: close pane {pane:?}");
                    }
                    if let Some(pane_pump) = sessions.remove(&pane) {
                        // Closing can wait for a slow child (up to the
                        // kill escalation in `close`); do it off the pump
                        // thread so other panes keep running (audit P6).
                        close_in_background(pane_pump.session);
                        slots.remove(&pane);
                        let _ = events.send_event(AppEvent::PaneGone { pane });
                    }
                }
                PumpMsg::Resize { pane, rows, cols } => {
                    if debug_enabled() {
                        eprintln!("debug: pump: resize {pane:?} {rows}x{cols}");
                    }
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
                        pane_pump.session.resize(rows as u16, cols as u16);
                    }
                }
                PumpMsg::Input { pane, bytes, seq } => {
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
                        // A refused input counts as fed once the bytes
                        // before it are: it never changes the buffer.
                        pane_pump.input_seq = seq;
                        // Refused (never accepted, then dropped) when the
                        // pane's queue is full: the child stopped reading.
                        // Keys keep a headroom pastes cannot fill.
                        let accepted = queue_key_input(
                            &mut pane_pump.pending_input,
                            &mut pane_pump.paste_block,
                            &bytes,
                        );
                        if !accepted && debug_enabled() {
                            eprintln!("debug: pump: input refused, {pane:?} queue full");
                        }
                    }
                }
                PumpMsg::Scroll { pane, scroll } => {
                    if debug_enabled() {
                        eprintln!("debug: pump: scroll {pane:?} {scroll:?}");
                    }
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
                        pane_pump.session.engine().scroll(scroll);
                    }
                }
                PumpMsg::Select {
                    pane,
                    start_row,
                    start_col,
                    end_row,
                    end_col,
                } => {
                    if debug_enabled() {
                        eprintln!(
                            "debug: pump: select {pane:?} ({start_row},{start_col})..({end_row},{end_col})"
                        );
                    }
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
                        pane_pump
                            .session
                            .engine()
                            .select((start_row, start_col), (end_row, end_col));
                        pane_pump.force_snapshot = true;
                    }
                }
                PumpMsg::ExtendSelection {
                    pane,
                    end_row,
                    end_col,
                } => {
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
                        pane_pump
                            .session
                            .engine()
                            .extend_selection((end_row, end_col));
                        pane_pump.force_snapshot = true;
                    }
                }
                PumpMsg::ClearSelection { pane } => {
                    if debug_enabled() {
                        eprintln!("debug: pump: clear selection {pane:?}");
                    }
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
                        pane_pump.session.engine().clear_selection();
                        pane_pump.force_snapshot = true;
                    }
                }
                PumpMsg::Copy { pane } => {
                    if let Some(pane_pump) = sessions.get_mut(&pane)
                        && let Some(text) = pane_pump.session.engine().copy_selection()
                    {
                        if debug_enabled() {
                            eprintln!("debug: pump: copied {} byte(s)", text.len());
                        }
                        let _ = events.send_event(AppEvent::Copied { pane, text });
                    }
                }
                PumpMsg::Paste {
                    pane,
                    text,
                    bracketed,
                    seq,
                } => {
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
                        pane_pump.input_seq = seq;
                        // Bracket markers add 12 bytes; newline
                        // normalization never grows the text.
                        if pane_pump.pending_input.len() + text.len() + 12 > MAX_PENDING_INPUT {
                            if debug_enabled() {
                                eprintln!("debug: pump: paste refused, {pane:?} queue full");
                            }
                            continue;
                        }
                        let applied = apply_paste(
                            &mut pane_pump.pending_input,
                            &mut pane_pump.paste_block,
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
                PumpMsg::CancelPaste { pane } => {
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
                        let dropped = cancel_paste_keeping_tail(
                            &mut pane_pump.pending_input,
                            &mut pane_pump.paste_block,
                        );
                        if debug_enabled() {
                            eprintln!("debug: pump: paste cancelled, dropped {dropped} byte(s)");
                        }
                    }
                }
            }
        }

        // (3) Feed each pane's pending input in order (bounded by the
        // writer's app cap) and flush up to the app cap per tick. Draining
        // panes get no new input (the child is exiting), but are still
        // flushed to forward any final ledger replies.
        for pane_pump in sessions.values_mut() {
            if !pane_pump.draining {
                feed_pending(
                    &mut pane_pump.session,
                    &mut pane_pump.pending_input,
                    &mut pane_pump.paste_block,
                );
            }
            // Every queued input reached the session: the shell's next
            // buffer report can reflect the latest one.
            if pane_pump.pending_input.is_empty() {
                pane_pump.fed_seq = pane_pump.input_seq;
            }
            let _ = pane_pump.session.flush(1024 * 1024);
        }

        if let Some(s) = suggestions.as_mut() {
            s.save_if_due(Duration::from_secs(30));
        }
        if stop.load(Ordering::Relaxed) {
            if let Some(s) = suggestions.as_mut() {
                s.save_now();
            }
            // N12a: close before break — the pinned loop panics in
            // `PeekableReceiver::recv` when its sender is dropped
            // without `Msg::Shutdown` (alacritty_terminal 0.26.0
            // event_loop.rs:483-488); `close()` sends it.
            // Close every pane concurrently, then wait for all of them.
            let closers: Vec<_> = sessions
                .drain()
                .map(|(_, pane_pump)| close_in_background(pane_pump.session))
                .collect();
            for closer in closers.into_iter().flatten() {
                let _ = closer.join();
            }
            break;
        }
        // Poll again soon only while some pane needs it: a draining pane
        // (final output after child exit) or input still queued behind
        // the writer's cap. Otherwise wait for a wake; the heartbeat only
        // bounds how stale an unexpected missed wake can get.
        let busy = sessions
            .values()
            .any(|p| p.draining || !p.pending_input.is_empty() || p.session.outstanding() > 0);
        // A command in (2) may have changed the view (scroll, search,
        // selection, config) after this pass's tick: tick again now
        // rather than at the next wake (audit RV2: up to 500 ms late on
        // an idle pane). The extra tick builds a snapshot only on damage
        // or a forced view, and only into a free slot.
        timeout = if commanded {
            Duration::ZERO
        } else if busy {
            Duration::from_millis(10)
        } else {
            Duration::from_millis(500)
        };
    }
}

/// How long the window waits for a clipboard read before input typed
/// behind it goes out without it (the read's text is then dropped).
pub const PASTE_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Most paste requests queued behind a clipboard read in flight.
pub const MAX_QUEUED_PASTES: usize = 4;

/// What the window does next for [`PasteGate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteGateAction {
    /// Send these input bytes to the pane now ([`PumpMsg::Input`]).
    Input { pane: PaneKey, bytes: Vec<u8> },
    /// Paste this clipboard text into the pane now ([`PumpMsg::Paste`]).
    Paste { pane: PaneKey, text: String },
    /// Start clipboard read `read` for a paste into the pane; report its
    /// result with [`PasteGate::read_done`].
    Read { pane: PaneKey, read: u64 },
}

/// One entry held behind a clipboard read.
#[derive(Debug)]
enum Held {
    Input(PaneKey, Vec<u8>),
    Paste(PaneKey),
}

/// The read in flight.
#[derive(Debug)]
struct PasteRead {
    pane: PaneKey,
    id: u64,
    started: Instant,
}

/// The window's paste order: the clipboard is read on a thread, so input
/// to a pane typed after a paste request is held until the clipboard text
/// was sent, then released in order (Ctrl+Shift+V then Enter pastes, then
/// runs). A paste requested while a read is in flight queues behind it.
/// Held input is bounded by [`INPUT_HEADROOM`] (more is refused, as the
/// pump refuses keys over its queue), queued pastes by
/// [`MAX_QUEUED_PASTES`]; a read that takes longer than
/// [`PASTE_READ_TIMEOUT`] is abandoned at the next input. Pure: the
/// caller runs the returned actions in order.
#[derive(Debug, Default)]
pub struct PasteGate {
    reading: Option<PasteRead>,
    held: std::collections::VecDeque<Held>,
    held_bytes: usize,
    next_id: u64,
}

impl PasteGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a clipboard read is in flight.
    pub fn reading(&self) -> bool {
        self.reading.is_some()
    }

    /// The user asked to paste into `pane`.
    pub fn request(&mut self, pane: PaneKey, now: Instant) -> Vec<PasteGateAction> {
        if self.reading.is_none() {
            return vec![self.start(pane, now)];
        }
        let queued = self
            .held
            .iter()
            .filter(|h| matches!(h, Held::Paste(_)))
            .count();
        if queued < MAX_QUEUED_PASTES {
            self.held.push_back(Held::Paste(pane));
        }
        Vec::new()
    }

    /// Input for `pane`: sent now, or held when a paste into `pane` is
    /// waiting for its clipboard read.
    pub fn input(&mut self, pane: PaneKey, bytes: Vec<u8>, now: Instant) -> Vec<PasteGateAction> {
        let mut actions = Vec::new();
        if let Some(read) = &self.reading
            && now.duration_since(read.started) >= PASTE_READ_TIMEOUT
        {
            self.reading = None;
            actions = self.release(now);
        }
        if !self.waits(pane) {
            actions.push(PasteGateAction::Input { pane, bytes });
        } else if self.held_bytes + bytes.len() <= INPUT_HEADROOM {
            self.held_bytes += bytes.len();
            self.held.push_back(Held::Input(pane, bytes));
        }
        actions
    }

    /// Clipboard read `read` finished with `text` (`None`: unavailable,
    /// empty or too large): paste it, then release what was held behind
    /// it. The result of an abandoned read is dropped.
    pub fn read_done(
        &mut self,
        read: u64,
        text: Option<String>,
        now: Instant,
    ) -> Vec<PasteGateAction> {
        let Some(current) = self.reading.take_if(|r| r.id == read) else {
            return Vec::new();
        };
        let mut actions: Vec<_> = text
            .map(|text| PasteGateAction::Paste {
                pane: current.pane,
                text,
            })
            .into_iter()
            .collect();
        actions.extend(self.release(now));
        actions
    }

    /// Whether input for `pane` must wait: a paste into it is pending, or
    /// earlier input for it is still held.
    fn waits(&self, pane: PaneKey) -> bool {
        self.reading.as_ref().is_some_and(|r| r.pane == pane)
            || self.held.iter().any(|h| match h {
                Held::Input(p, _) | Held::Paste(p) => *p == pane,
            })
    }

    fn start(&mut self, pane: PaneKey, now: Instant) -> PasteGateAction {
        self.next_id += 1;
        let read = self.next_id;
        self.reading = Some(PasteRead {
            pane,
            id: read,
            started: now,
        });
        PasteGateAction::Read { pane, read }
    }

    /// Release held entries in order up to the next queued paste, whose
    /// read then starts.
    fn release(&mut self, now: Instant) -> Vec<PasteGateAction> {
        let mut actions = Vec::new();
        while let Some(held) = self.held.pop_front() {
            match held {
                Held::Input(pane, bytes) => {
                    self.held_bytes -= bytes.len();
                    actions.push(PasteGateAction::Input { pane, bytes });
                }
                Held::Paste(pane) => {
                    actions.push(self.start(pane, now));
                    break;
                }
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suggest::History;

    fn suggestions_at(path: std::path::PathBuf) -> Suggestions {
        Suggestions {
            history: History::new(),
            path: Some(path),
            last_save: Instant::now(),
            saving: None,
            executables: Arc::new(Mutex::new(Arc::new(Vec::new()))),
            executables_at: None,
            paths: crate::suggest::PathCompleter::new(Arc::new(|| {})),
        }
    }

    /// The periodic save runs on a background thread (one at a time) so
    /// a slow `fsync` cannot stall the pump; the final save waits for it
    /// and writes what is left in place.
    #[test]
    fn history_saves_in_the_background_then_finally_in_place() {
        let dir = std::env::temp_dir().join(format!("nora-pump-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("history");
        let mut s = suggestions_at(path.clone());
        assert!(s.history.record("make", Some("/p"), Some(0), 1));
        s.save_if_due(Duration::ZERO);
        assert!(s.saving.is_some(), "saving off the pump thread");
        assert!(!s.history.is_dirty(), "the snapshot is being saved");
        assert!(s.history.record("cargo test", Some("/p"), Some(0), 2));
        s.save_if_due(Duration::ZERO);
        s.save_now();
        assert!(s.saving.is_none());
        assert!(!s.history.is_dirty());
        let disk = History::load(&path).expect("saved");
        assert!(disk.get("make").is_some() && disk.get("cargo test").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A history file that has become unreadable is left alone and not
    /// saved again this session.
    #[test]
    fn unreadable_history_stops_saving() {
        let dir = std::env::temp_dir().join(format!("nora-pump-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history");
        std::fs::write(&path, "nora-history 2\n").unwrap();
        let mut s = suggestions_at(path.clone());
        assert!(s.history.record("make", None, Some(0), 1));
        s.save_if_due(Duration::ZERO);
        s.save_now();
        assert!(s.path.is_none(), "saving stopped");
        assert_eq!(std::fs::read(&path).unwrap(), b"nora-history 2\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
