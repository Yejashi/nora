//! Real-PTY session driver (N05a).
//!
//! [`PtySession::spawn`] launches a child on a real PTY and runs the pinned
//! `alacritty_terminal` event loop on a dedicated thread against the
//! engine's shared term (the deliberately encapsulated synchronization
//! boundary). Child output updates the same [`Engine`] the caller reads;
//! application bytes go through the shared [`BoundedWriter`] and are
//! forwarded to the child as ordered `Msg::Input`; parser replies are
//! accepted atomically through the shared [`ReplyLedger`]; loop end
//! surfaces as [`SessionEvent::Terminated`] on every poll after the loop
//! thread has ended.
//!
//! N06a: every drained record is resolved through the shared
//! [`super::resolve_record`] (`PtyWrite` bytes, color, size and
//! clipboard-load query events) and the resulting replies are accepted
//! atomically through the shared [`ReplyLedger`], against this
//! session's own window geometry.
//!
//! N12a: the bounded child-exit lifecycle — the pinned `Lifecycle`
//! state chain (`Starting → Running → Exiting → Exited → Closed`),
//! [`PtySession::wait_drain`] (the bounded wait for the session loop
//! thread) and [`DRAIN_TIMEOUT`] (5 s). Finiteness is nora-side: the
//! pinned `drain_on_exit` read has no in-crate timeout
//! (alacritty_terminal 0.26.0 `event_loop.rs`).

use std::borrow::Cow;
use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use alacritty_terminal::event::WindowSize;
use alacritty_terminal::event_loop::{EventLoop, EventLoopSendError, EventLoopSender, Msg};
use alacritty_terminal::tty::{self, Options, Shell};

use crate::engine::{Engine, EventRecord};

use super::io::{BoundedWriter, WriterConfig, WriterError};
use super::{NOMINAL_CELL_HEIGHT, NOMINAL_CELL_WIDTH, ReplyLedger, resolve_record};

/// N32b/N35a: byte-level pre-filter for the PTY `EventLoop`.
///
/// Runs the N32a [`GraphicsFilter`] and the N35a [`PromptMarkerFilter`] in
/// sequence on the pinned PTY reader thread (via the vendored `Filter`
/// hook), pushing extracted events into the session's shared
/// [`GraphicsQueue`] and [`MarkerQueue`] handles. The headless
/// `Engine::feed` path and this PTY path feed the same queues.
///
/// The vendored `EventLoop::with_filter` installs a single filter (it
/// replaces, not chains), so both filters run inside this one hook: the
/// graphics filter sees the raw bytes, and the marker filter sees the
/// graphics filter's forwarded bytes.
struct PtyFilter {
    graphics: crate::graphics_filter::GraphicsFilter,
    graphics_queue: Arc<Mutex<crate::graphics_filter::GraphicsQueue>>,
    markers: crate::prompt_marker::PromptMarkerFilter,
    marker_queue: Arc<Mutex<crate::prompt_marker::MarkerQueue>>,
    /// The child's current working directory (N35d): the most recent OSC 7
    /// `file://` path, shared with the headless `Engine::feed` path.
    cwd: Arc<Mutex<Option<String>>>,
    /// Byte-bounded queue of hyperlink events, shared with the headless
    /// `Engine::feed` path (N35d).
    hyperlink_queue: Arc<Mutex<crate::prompt_marker::HyperlinkQueue>>,
    /// Shell events from the last `process` call, in order (N35b/N35d).
    /// The event loop calls `on_marker` once per reported position, in
    /// order, so `pending_index` tracks which event the next callback is
    /// for.
    pending_events: Vec<crate::prompt_marker::ShellEvent>,
    pending_index: usize,
}

impl alacritty_terminal::event_loop::Filter for PtyFilter {
    fn process(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<usize>) {
        let (forward, events) = self.graphics.process(bytes);
        if !events.is_empty() {
            let mut queue = self
                .graphics_queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for event in events {
                queue.push(event);
            }
        }
        let (forward, events) = self.markers.process(forward);
        self.pending_events = events.iter().map(|(_, e)| e.clone()).collect();
        self.pending_index = 0;
        let positions: Vec<usize> = events.iter().map(|(pos, _)| *pos).collect();
        (forward, positions)
    }

    fn on_marker(
        &mut self,
        _position: usize,
        cursor_line: i32,
        cursor_col: usize,
        total_scrolled: u64,
        total_pulled: u64,
    ) {
        if let Some(event) = self.pending_events.get(self.pending_index).cloned() {
            self.pending_index += 1;
            match event {
                crate::prompt_marker::ShellEvent::Marker(m) => {
                    let entry = crate::prompt_marker::MarkerAtLine {
                        line: cursor_line,
                        scrolled: total_scrolled,
                        pulled: total_pulled,
                        event: m,
                    };
                    let mut queue = self
                        .marker_queue
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    queue.push(entry);
                }
                crate::prompt_marker::ShellEvent::Cwd(path) => {
                    let mut cwd = self.cwd.lock().unwrap_or_else(PoisonError::into_inner);
                    *cwd = Some(path);
                }
                crate::prompt_marker::ShellEvent::Hyperlink(uri) => {
                    let entry = crate::prompt_marker::HyperlinkAtLine {
                        line: cursor_line,
                        col: cursor_col,
                        scrolled: total_scrolled,
                        pulled: total_pulled,
                        uri,
                    };
                    let mut queue = self
                        .hyperlink_queue
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    queue.push(entry);
                }
            }
        }
    }
}

/// Default per-session write budgets (policy values, 03:86-89): 1 MiB
/// total, 64 KiB reserved for parser replies.
pub const DEFAULT_CAP: WriterConfig = WriterConfig {
    max_bytes: 1024 * 1024,
    reserved_bytes: 64 * 1024,
};

/// Terminal options for a new session (from the configuration).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionOptions {
    /// Scrollback lines.
    pub history: usize,
    /// The cursor shape used until the application sets one.
    pub cursor_shape: alacritty_terminal::vte::ansi::CursorShape,
}

impl Default for SessionOptions {
    /// No scrollback and a block cursor (the library default; the window
    /// passes the configured values).
    fn default() -> Self {
        Self {
            history: 0,
            cursor_shape: alacritty_terminal::vte::ansi::CursorShape::Block,
        }
    }
}

/// Launch parameters for a PTY child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSpec {
    /// The executable to run (no shell interpolation).
    pub program: String,
    /// Arguments passed verbatim as argv.
    pub args: Vec<String>,
    /// Pre-validated by nora (the pinned crate silently ignores a bad cwd).
    pub cwd: Option<PathBuf>,
    /// Extra env vars, added to the inherited environment.
    pub env: Vec<(String, String)>,
}

/// Error launching or validating a PTY session.
#[derive(Debug)]
pub enum PtyError {
    /// cwd does not exist or is not a directory.
    BadCwd(PathBuf),
    /// The session could not be started (child spawn or event-loop registration failed).
    Spawn(io::Error),
}

// `io::Error` is not `PartialEq`, so the derive cannot express equality.
// Compare by variant; two spawn errors are equal without inspecting the
// (uncomparable) inner error.
impl PartialEq for PtyError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PtyError::BadCwd(a), PtyError::BadCwd(b)) => a == b,
            (PtyError::Spawn(_), PtyError::Spawn(_)) => true,
            _ => false,
        }
    }
}

/// Per-session generation identity, minted once at spawn and attached to
/// every event nora emits for that session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl SessionId {
    /// Mint the next generation identity (also the PTY window id).
    fn mint() -> Self {
        Self(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// The session's bounded child-exit lifecycle (N12a, 03:107):
/// `Starting → Running → Exiting → Exited → Closed`.
///
/// Writers (pinned): the main thread sets `Running` at spawn (after
/// the loop handle is assigned — the loop thread never sets `Running`)
/// and folds `Exiting` on the first child-exit fold; the session loop
/// thread folds `Exiting` on its return path (every exit path of the
/// pinned PTY reader funnels through it — covers a loop death without
/// a child-exit event) and signals its finish; `close()` (main thread
/// only) folds `Exited` after the bounded wait and `Closed` after
/// joining the loop thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    /// Spawn, before the loop handle is assigned (transient).
    Starting,
    /// The session is live (loop handle assigned).
    Running,
    /// A child exit was folded, or the loop thread is exiting.
    Exiting,
    /// `close()`'s bounded wait for the loop thread returned.
    Exited,
    /// The loop thread is joined; `close()` completed.
    Closed,
}

/// Bound for every drain wait of the session loop thread (N12a): the
/// pinned `drain_on_exit` read has no in-crate timeout, so finiteness
/// is nora-side (03:107).
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Most input bytes handed to the PTY loop but not yet written to the PTY
/// (1 MiB). Past it, `flush` holds bytes back in the session writer, so a
/// child that stops reading stdin bounds memory instead of growing the
/// loop's queue (audit P4).
pub const MAX_IN_FLIGHT: usize = 1024 * 1024;

/// How long `close` waits for the child to exit after SIGHUP before it
/// kills the child's process group.
pub const CLOSE_GRACE: Duration = Duration::from_millis(1500);

/// `status` is `Some` on a clean child exit (`Event::ChildExit`), `None`
/// when the loop died from an I/O or registration error. Not
/// `PartialEq`-able (`ExitStatus`) — assert with `matches!` +
/// `status.code()`.
#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// The loop thread has ended.
    Terminated {
        session: SessionId,
        status: Option<ExitStatus>,
    },
}

/// A session bound to a real PTY child: N03 [`Engine`] + N04a
/// [`BoundedWriter`] + the pinned event loop driven against the engine's
/// shared term.
///
/// The loop thread owns all PTY I/O. The main thread owns the bounded
/// outbound writer and the shared [`ReplyLedger`]: `feed`/`flush` keep
/// app bytes and parser replies in one ordered, byte-budgeted stream, and
/// `poll_events` reports loop end as `Terminated` on every poll after the
/// loop thread has ended.
pub struct PtySession {
    id: SessionId,
    engine: Engine,
    writer: BoundedWriter,
    ledger: ReplyLedger,
    /// The session's own window geometry, as passed to `tty::new`;
    /// size replies resolve against it, so they never cross sessions.
    size: WindowSize,
    sender: EventLoopSender,
    /// The child's pid, which is also its process group id.
    #[cfg(unix)]
    child_pid: libc::pid_t,
    /// Bytes sent as `Msg::Input` and not yet written (decremented by the
    /// vendored loop's `pty_write`).
    in_flight: Arc<std::sync::atomic::AtomicUsize>,
    /// The session loop thread (N12a): a wrapper around the pinned PTY
    /// reader that folds [`Lifecycle::Exiting`] on the wrapper's return
    /// (every exit path of the pinned loop funnels through it) and
    /// signals `done` before returning. Present while the thread is
    /// unjoined; `terminated()` uses the handle while present.
    loop_handle: Option<JoinHandle<()>>,
    /// Current lifecycle state (N12a), shared main ↔ loop thread.
    lifecycle: Arc<Mutex<Lifecycle>>,
    /// The loop thread finished (N12a): set by the loop thread on
    /// every exit path, then it notifies `done_notify`; `wait_drain`
    /// waits on it (bounded).
    done: Arc<Mutex<bool>>,
    done_notify: Arc<Condvar>,
    /// Exit status recorded from the loop's `ChildExit` event, if any.
    child_status: Option<ExitStatus>,
    /// Whether `Msg::Shutdown` has been sent.
    shutdown_sent: bool,
    /// A terminated loop with no child-exit event was reported once
    /// (N12a): `observe_loop_end` keeps the stderr line singular.
    loop_end_reported: bool,
    /// Last title set by the terminal (latest wins), `None` when the
    /// title was reset or never set. Folded on the main thread from
    /// drained [`EventRecord::Title`]/[`EventRecord::ResetTitle`]
    /// records; no cross-thread mutation of shared state.
    title: Option<String>,
    /// A bell rang since the last [`PtySession::take_bell`].
    bell: bool,
}

impl PtySession {
    /// Mint the id; pre-validate cwd (`PtyError::BadCwd`); spawn the child
    /// (pinned `tty::new` + `EventLoop::new` with `drain_on_exit: true`
    /// and `ref_test: false`) on the engine's shared term handle; run the
    /// loop on a dedicated thread; route every drained record through
    /// the shared [`resolve_record`] → atomic reply headroom (shared
    /// [`ReplyLedger`]), `ChildExit` → `Terminated(Some(status))`, other
    /// events discarded.
    pub fn spawn(
        spec: LaunchSpec,
        rows: usize,
        cols: usize,
        cap: WriterConfig,
    ) -> Result<Self, PtyError> {
        Self::spawn_configured(spec, rows, cols, cap, &SessionOptions::default())
    }

    /// [`PtySession::spawn`] with terminal options from the configuration.
    pub fn spawn_configured(
        spec: LaunchSpec,
        rows: usize,
        cols: usize,
        cap: WriterConfig,
        options: &SessionOptions,
    ) -> Result<Self, PtyError> {
        let id = SessionId::mint();
        let cwd = spec.cwd;
        if let Some(dir) = &cwd
            && !dir.is_dir()
        {
            return Err(PtyError::BadCwd(dir.clone()));
        }

        let mut engine = Engine::with_history(rows, cols, options.history);
        engine.set_default_cursor_shape(options.cursor_shape);
        let writer = BoundedWriter::new(cap);
        let ledger = ReplyLedger::new(cap.reserved_bytes.min(cap.max_bytes));

        let options = Options {
            shell: Some(Shell::new(spec.program, spec.args)),
            working_directory: cwd,
            env: spec.env.into_iter().collect(),
            drain_on_exit: true,
            // Quote argv for CreateProcess like a normal Windows program.
            #[cfg(target_os = "windows")]
            escape_args: true,
        };
        // Nominal headless cell geometry: pixel-size replies report
        // rows/cols times the nominal cell size.
        let size = WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: NOMINAL_CELL_WIDTH,
            cell_height: NOMINAL_CELL_HEIGHT,
        };
        let pty = tty::new(&options, size, id.0).map_err(PtyError::Spawn)?;
        // The child is a session leader (setsid), so this is also its
        // process group id; kept for kill escalation in `close`.
        #[cfg(unix)]
        let child_pid = pty.child().id() as libc::pid_t;

        // The slave keeps the kernel's cooked line discipline (ICANON,
        // ECHO, ISIG, ICRNL, OPOST|ONLCR); the pinned `tty::new` adds only
        // IUTF8, exactly like upstream alacritty. Applications that need
        // raw input (shells' line editors, full-screen programs) switch
        // the mode themselves. Forcing raw here broke echo, Ctrl+C and
        // newline translation for every ordinary command (audit P2).

        // `drain_on_exit: true`: on child exit the pinned loop can process
        // the exit token before reading master bytes that arrived in the
        // same poll batch; the flag makes the exit path do one final
        // `pty_read` so trailing output (argv/pwd lines) reaches the term.
        // N32b/N35a: install the pre-filter on the pinned PTY reader so
        // kitty-graphics APCs (N32a) and OSC 133 prompt markers (N35a) are
        // extracted on the PTY path (the headless `Engine::feed` path
        // already runs its own N32a/N35a filters). Both push into the
        // session's shared queues.
        let pty_filter = PtyFilter {
            graphics: crate::graphics_filter::GraphicsFilter::new(),
            graphics_queue: engine.graphics_handle(),
            markers: crate::prompt_marker::PromptMarkerFilter::new(),
            marker_queue: engine.markers_handle(),
            cwd: engine.cwd_handle(),
            hyperlink_queue: engine.hyperlink_handle(),
            pending_events: Vec::new(),
            pending_index: 0,
        };
        let in_flight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loop_ = EventLoop::new(engine.term_handle(), engine.recorder(), pty, true, false)
            .map(|loop_| {
                loop_
                    .with_filter(Box::new(pty_filter))
                    .with_in_flight(in_flight.clone())
            })
            .map_err(PtyError::Spawn)?;
        let sender = loop_.channel();

        // N12a: the session loop thread wraps the pinned PTY reader.
        // Every exit path of the pinned loop (child exit, I/O error,
        // registration error, shutdown) funnels through the wrapper's
        // return: fold `Exiting` there (covers a loop death without a
        // child-exit event), then signal `done` before returning. The
        // main thread sets `Running` after the loop handle is
        // assigned — the loop thread never sets `Running`.
        let lifecycle = Arc::new(Mutex::new(Lifecycle::Starting));
        let done = Arc::new(Mutex::new(false));
        let done_notify = Arc::new(Condvar::new());
        let lifecycle_loop = lifecycle.clone();
        let done_loop = done.clone();
        let done_notify_loop = done_notify.clone();
        let loop_handle = std::thread::Builder::new()
            .name(format!("nora session {}", id.0))
            .spawn(move || {
                let pinned = loop_.spawn();
                let _ = pinned.join();
                *lifecycle_loop
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Lifecycle::Exiting;
                *done_loop.lock().unwrap_or_else(PoisonError::into_inner) = true;
                done_notify_loop.notify_all();
            })
            .map_err(PtyError::Spawn)?;

        *lifecycle.lock().unwrap_or_else(PoisonError::into_inner) = Lifecycle::Running;

        Ok(Self {
            id,
            engine,
            writer,
            ledger,
            size,
            sender,
            in_flight,
            #[cfg(unix)]
            child_pid,
            loop_handle: Some(loop_handle),
            lifecycle,
            done,
            done_notify,
            child_status: None,
            shutdown_sent: false,
            loop_end_reported: false,
            title: None,
            bell: false,
        })
    }

    /// Resize this session's window: update the shared size, apply the
    /// same dimensions to the engine grid (the pinned `Term::resize` marks
    /// the terminal fully damaged, so the next [`Engine::snapshot`]
    /// reports `Damage::Full`), and send `Msg::Resize` to the PTY loop,
    /// which sets the master's window size (`TIOCSWINSZ`) so the child
    /// receives `SIGWINCH`. `resolve_record` answers later CSI 14 t
    /// queries against the updated `size`. Nothing is written to the
    /// child's input: an unsolicited size report would arrive as typed
    /// bytes (audit P1).
    ///
    /// N11a zero retention (lib-level invariant): a zero on either axis
    /// is retained per axis against the last valid value before
    /// anything is applied — if the retained pair equals the current
    /// geometry, the resize is a no-op; the engine and the PTY never see
    /// a zero.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = if rows == 0 { self.size.num_lines } else { rows };
        let cols = if cols == 0 { self.size.num_cols } else { cols };
        if (rows, cols) == (self.size.num_lines, self.size.num_cols) {
            return;
        }
        self.size = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: NOMINAL_CELL_WIDTH,
            cell_height: NOMINAL_CELL_HEIGHT,
        };
        self.engine().resize(rows as usize, cols as usize);
        // Tell the child: the loop applies TIOCSWINSZ on the master, and
        // the kernel delivers SIGWINCH to the foreground process group.
        // A send failure means the loop already ended (child gone).
        let _ = self.sender.send(Msg::Resize(self.size));
    }

    /// This session's generation identity.
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// The last title set by the terminal, if any (folded on the main
    /// thread; see the struct docs).
    pub fn title(&self) -> Option<String> {
        self.title.clone()
    }

    /// The session's current lifecycle state (N12a, 03:107).
    pub fn lifecycle(&self) -> Lifecycle {
        *self
            .lifecycle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Bounded wait for the session loop thread to finish (N12a).
    ///
    /// Returns `true` if the loop finished within `timeout`.
    /// Finiteness is nora-side: the pinned `drain_on_exit` read has no
    /// in-crate timeout, so every join/close path goes through this
    /// bound (03:107).
    pub fn wait_drain(&self, timeout: Duration) -> bool {
        let mut done = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        if !*done {
            let (guard, _timeout_result) = self
                .done_notify
                .wait_timeout(done, timeout)
                .unwrap_or_else(PoisonError::into_inner);
            done = guard;
        }
        *done
    }

    /// App bytes into the writer's app budget (partial acceptance,
    /// `BoundedWriter::enqueue` semantics). `Err(Closed)` when closed.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<usize, WriterError> {
        self.process_events();
        let accepted = self.writer.enqueue(bytes)?;
        if accepted > 0 {
            self.ledger.track_app(accepted);
        }
        Ok(accepted)
    }

    /// Input bytes handed to the PTY loop and not yet written to the PTY
    /// (at most [`MAX_IN_FLIGHT`] plus one flush chunk).
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Bytes still reserved by the session writer.
    pub fn outstanding(&self) -> usize {
        self.writer.outstanding()
    }

    /// Delivery to the child is internal: completed bytes are forwarded
    /// as ordered `Msg::Input` via the loop channel (no caller sink —
    /// unlike headless `Session`, the caller does not own the terminal).
    /// Sender disconnected (loop dead) → close + `Err(WriterError::Closed)`.
    pub fn flush(&mut self, limit: usize) -> Result<usize, WriterError> {
        self.process_events();
        let room = MAX_IN_FLIGHT.saturating_sub(self.in_flight());
        let limit = limit.min(room);
        if limit == 0 {
            return Ok(0);
        }
        let mut loop_dead = false;
        let in_flight = self.in_flight.clone();
        let result = self.writer.flush(
            &mut |bytes: &[u8]| {
                in_flight.fetch_add(bytes.len(), std::sync::atomic::Ordering::AcqRel);
                match self.sender.send(Msg::Input(Cow::Owned(bytes.to_vec()))) {
                    // One chunk in, one ordered `Msg::Input` out: the whole
                    // chunk is delivered atomically (or not at all).
                    Ok(()) => Ok(bytes.len()),
                    Err(err) => {
                        if matches!(err, EventLoopSendError::Send(_)) {
                            loop_dead = true;
                        }
                        Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            format!("pty loop: {err}"),
                        ))
                    }
                }
            },
            limit,
        );
        match result {
            Ok(completed) => {
                self.ledger.complete(completed);
                Ok(completed)
            }
            Err(_) if loop_dead => {
                // The loop can no longer deliver anything: close the
                // session writer and report it as closed.
                self.writer.cancel();
                self.ledger.release();
                self.send_shutdown();
                Err(WriterError::Closed)
            }
            Err(err) => Err(err),
        }
    }

    /// Non-blocking poll: drains pending records and, once the loop thread
    /// has ended, reports the terminal state as `Terminated` on every call —
    /// the state is observable at any later tick (a caller may first notice
    /// the loop's end on a poll that does not inspect events). `status` is
    /// `Some` once the loop's `ChildExit` record has been drained (it always
    /// precedes loop end for a clean exit), else `None` (the loop died from
    /// an I/O or registration error).
    pub fn poll_events(&mut self) -> Vec<SessionEvent> {
        self.process_events();
        let mut events = Vec::new();
        if self.terminated() {
            self.observe_loop_end();
            events.push(SessionEvent::Terminated {
                session: self.id,
                status: self.child_status,
            });
        }
        events
    }

    /// True once the loop thread has ended.
    pub fn terminated(&self) -> bool {
        match &self.loop_handle {
            Some(handle) => handle.is_finished(),
            None => true,
        }
    }

    /// Block until the loop thread has ended — bounded by
    /// [`DRAIN_TIMEOUT`] (N12a: the pinned `drain_on_exit` read has no
    /// in-crate timeout) — the child's exit status if a clean child
    /// exit was observed. The status is state, not a one-shot: it is
    /// also carried by every `Terminated` after loop end.
    pub fn join(&mut self) -> Option<ExitStatus> {
        let finished = self.wait_drain(DRAIN_TIMEOUT);
        if !finished {
            Self::warn_drain_timeout();
        }
        if let Some(handle) = self.loop_handle.take()
            && finished
        {
            let _ = handle.join();
        }
        self.process_events();
        self.observe_loop_end();
        self.child_status
    }

    /// Release writer bytes and send `Msg::Shutdown`, then wait for the
    /// loop thread to finish (bounded by [`DRAIN_TIMEOUT`], N12a), fold
    /// `Exited`, and join the loop thread (folding `Closed`).
    /// Idempotent: a second call returns 0 without touching the writer.
    pub fn close(&mut self) -> usize {
        if self.lifecycle() == Lifecycle::Closed {
            return 0;
        }
        self.process_events();
        self.observe_loop_end();
        let released = self.writer.cancel();
        self.ledger.release();
        self.send_shutdown();
        // Shutdown makes the loop drop the PTY, which SIGHUPs the child and
        // waits for it. A child that ignores SIGHUP would block that wait
        // forever (leaking the loop thread, the child and the PTY fds), so
        // after a grace period its process group is killed (audit P6).
        let mut finished = self.wait_drain(CLOSE_GRACE);
        if !finished {
            self.kill_child_group();
            finished = self.wait_drain(DRAIN_TIMEOUT);
        }
        if !finished {
            Self::warn_drain_timeout();
        }
        *self
            .lifecycle
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Lifecycle::Exited;
        if let Some(handle) = self.loop_handle.take()
            && finished
        {
            let _ = handle.join();
        }
        *self
            .lifecycle
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Lifecycle::Closed;
        released
    }

    /// Same-crate accessor for engine state (tests now, UI later).
    ///
    /// While the loop is alive, the loop thread owns the engine's parser;
    /// do not call `Engine::feed` directly (it would double-drive the
    /// parser). Read state via `snapshot()`.
    pub fn engine(&mut self) -> &mut Engine {
        &mut self.engine
    }

    /// Whether a bell rang since the last call (and clear it).
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.bell)
    }

    /// The child's pid (also its process group id).
    #[cfg(unix)]
    pub fn child_pid(&self) -> libc::pid_t {
        self.child_pid
    }

    /// SIGKILL the child's process group (and the child itself, in case it
    /// left the group). Used only after `CLOSE_GRACE` when SIGHUP did not
    /// end it.
    #[cfg(unix)]
    fn kill_child_group(&self) {
        if self.child_pid <= 1 {
            return;
        }
        // SAFETY: `kill` takes plain integers and has no memory effects.
        // The pid is our own child's: it is not reaped until the loop's
        // `Pty::drop` wait returns, which has not happened (the loop has
        // not finished), so it cannot have been reused; the group id is
        // that pid, held by the group while any member lives.
        unsafe {
            libc::kill(-self.child_pid, libc::SIGKILL);
            libc::kill(self.child_pid, libc::SIGKILL);
        }
    }

    /// Windows: closing the pseudo-console (ConPTY) terminates the
    /// processes attached to it, so there is no group to signal.
    #[cfg(not(unix))]
    fn kill_child_group(&self) {}

    /// Send `Msg::Shutdown` once. A send failure means the loop is already
    /// gone — nothing left to notify.
    fn send_shutdown(&mut self) {
        if !self.shutdown_sent {
            let _ = self.sender.send(Msg::Shutdown);
            self.shutdown_sent = true;
        }
    }

    /// Atomically drain all engine events recorded so far, in order.
    ///
    /// The loop thread records its events through the engine's shared
    /// recorder (`PtyWrite` from the shared term, `ChildExit` from the
    /// loop); this is the main-thread side of that wiring: every record
    /// is resolved through the shared [`resolve_record`] (reply strings
    /// are accepted atomically through the shared [`ReplyLedger`] —
    /// rejections counted there, never dropped), `ChildExit` records
    /// the status, and other events are discarded.
    fn process_events(&mut self) {
        for record in self.engine.drain_events() {
            // Fold presentation state on the main thread: latest title
            // wins; a reset clears it.
            match &record {
                EventRecord::Title(t) => self.title = Some(t.clone()),
                EventRecord::ResetTitle => self.title = None,
                EventRecord::Bell => self.bell = true,
                _ => {}
            }
            match record {
                EventRecord::ChildExit(status) if self.child_status.is_none() => {
                    self.child_status = Some(status);
                    // N12a: the first child-exit fold also folds
                    // `Exiting` (idempotent value — the loop thread
                    // folds the same value on its return path).
                    *self
                        .lifecycle
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = Lifecycle::Exiting;
                }
                record => {
                    if let Some(reply) = resolve_record(&self.engine, self.size, record) {
                        self.ledger.accept(&mut self.writer, &reply);
                    }
                }
            }
        }
    }

    /// Report a loop end without a child-exit event exactly once
    /// (N12a): the pinned loop sends `ChildExit` before returning on
    /// every clean exit, so a terminated loop with no folded status is
    /// a launch/worker failure — visible, never silent (03:107).
    /// Called from the main-thread observation points
    /// ([`Self::poll_events`] and [`Self::join`]); on a live windowed
    /// session the pump thread is that driver.
    fn observe_loop_end(&mut self) {
        if self.terminated() && !self.loop_end_reported {
            self.loop_end_reported = true;
            if self.child_status.is_none() {
                eprintln!("nora: error: session loop exited without a child-exit event");
            }
        }
    }

    /// One stderr line when the bounded drain does not finish in time
    /// (N12a): the loop thread is then detached (process-lifetime
    /// bounded) and the join proceeds.
    fn warn_drain_timeout() {
        eprintln!("nora: session drain timed out; detaching loop thread");
    }
}

// The session's writer side is `Arc`-based and `PtyReader` is a raw
// master handle, so `PtySession` is `Send` — the type of state a window
// loop must own across its thread.
const _: () = {
    const fn _assert<T: Send>() {}
    _assert::<PtySession>()
};
