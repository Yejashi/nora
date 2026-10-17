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

use super::pty::{DRAIN_TIMEOUT, LaunchSpec, PtySession, SessionEvent};
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
    if !terminated && want_snapshot {
        let (damage, cursor) = session.engine().probe();
        // An image change is a frame even without grid damage.
        if !is_noop_damage(&damage, cursor) || session.engine().graphics_changed() {
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
    /// buffer change.
    Suggestion {
        pane: PaneKey,
        suffix: Option<String>,
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
    /// earlier messages.
    Input { pane: PaneKey, bytes: Vec<u8> },
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

/// Most input bytes a pane queues in the pump (2 MiB). With the writer
/// budget and [`super::pty::MAX_IN_FLIGHT`] this bounds a pane's input
/// memory end to end; past it, keys and pastes are refused.
pub const MAX_PENDING_INPUT: usize = 2 * 1024 * 1024;

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
    paste_block: Option<(usize, usize)>,
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
    path: Option<std::path::PathBuf>,
    last_save: Instant,
    /// Listed on a background thread (a cold scan of PATH takes hundreds
    /// of milliseconds); empty until the first listing finishes.
    executables: Arc<Mutex<Arc<Vec<String>>>>,
    executables_at: Option<Instant>,
}

impl Suggestions {
    /// Load the user's history (an unreadable file starts empty, with a
    /// message; it is not overwritten until something is recorded).
    fn load() -> Self {
        let path = crate::suggest::History::default_path();
        let history = match &path {
            Some(path) => crate::suggest::History::load(path).unwrap_or_else(|error| {
                eprintln!("nora: history {}: {error}", path.display());
                crate::suggest::History::new()
            }),
            None => crate::suggest::History::new(),
        };
        let mut suggestions = Self {
            history,
            path,
            last_save: Instant::now(),
            executables: Arc::new(Mutex::new(Arc::new(Vec::new()))),
            executables_at: None,
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
    /// unambiguous path or executable completion of the last word.
    fn suggest(&mut self, buffer: &str, cwd: Option<&str>, now: u64) -> Option<String> {
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
        crate::suggest::complete_word(buffer, cwd, home.as_deref(), &executables)
    }

    /// Save when changed, at most every `interval`.
    fn save_if_due(&mut self, interval: Duration) {
        if !self.history.is_dirty() || self.last_save.elapsed() < interval {
            return;
        }
        self.last_save = Instant::now();
        if let Some(path) = &self.path
            && let Err(error) = self.history.save(path)
        {
            eprintln!("nora: saving history {}: {error}", path.display());
        }
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
    let engine = pane_pump.session.engine();
    let suffix = match (&buffer, suggestions.as_mut()) {
        (Some(b), Some(s))
            if b.cursor_at_end()
                && !password_input
                && !engine.modes().contains(TermMode::ALT_SCREEN)
                && pane_pump.paste_block.is_none() =>
        {
            s.suggest(&b.text, engine.cwd().as_deref(), now)
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
    let _ = events.send_event(AppEvent::Suggestion { pane, suffix });
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
            if !is_noop_damage(&damage, cursor) {
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

    let mut suggestions = options.autocomplete.then(Suggestions::load);
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
        for pane in removed {
            sessions.remove(&pane);
            slots.remove(&pane);
            let _ = events.send_event(AppEvent::PaneGone { pane });
        }

        // (2) Drain commands, routing each to its pane's session.
        let pending = first
            .into_iter()
            .chain(std::iter::from_fn(|| commands.try_recv().ok()));
        let mut commanded = false;
        for msg in pending {
            commanded |= !matches!(msg, PumpMsg::Wake);
            match msg {
                // The wake only ended the wait; the tick above already
                // picked up the new output.
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
                            s.save_if_due(Duration::ZERO);
                        }
                        suggestions = new.autocomplete.then(Suggestions::load);
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
                PumpMsg::Input { pane, bytes } => {
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
                        // Refused (never accepted, then dropped) when the
                        // pane's queue is full: the child stopped reading.
                        if pane_pump.pending_input.len() + bytes.len() <= MAX_PENDING_INPUT {
                            pane_pump.pending_input.extend(bytes);
                        } else if debug_enabled() {
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
                } => {
                    if let Some(pane_pump) = sessions.get_mut(&pane) {
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
                        let dropped =
                            cancel_paste(&mut pane_pump.pending_input, &mut pane_pump.paste_block);
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
            let _ = pane_pump.session.flush(1024 * 1024);
        }

        if let Some(s) = suggestions.as_mut() {
            s.save_if_due(Duration::from_secs(30));
        }
        if stop.load(Ordering::Relaxed) {
            if let Some(s) = suggestions.as_mut() {
                s.save_if_due(Duration::ZERO);
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
