//! Real-PTY session driver (N05a).
//!
//! [`PtySession::spawn`] launches a child on a real PTY and runs the pinned
//! `alacritty_terminal` event loop on a dedicated thread against the
//! engine's shared term (the deliberately encapsulated synchronization
//! boundary). Child output updates the same [`Engine`] the caller reads;
//! application bytes go through the shared [`BoundedWriter`] and are
//! forwarded to the child as ordered `Msg::Input`; parser replies are
//! accepted atomically through the shared [`ReplyLedger`]; loop end
//! surfaces as exactly one [`SessionEvent::Terminated`].

use std::borrow::Cow;
use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;

use alacritty_terminal::event::WindowSize;
use alacritty_terminal::event_loop::{EventLoop, EventLoopSendError, EventLoopSender, Msg, State};
use alacritty_terminal::tty::{self, Options, Pty, Shell};

use crate::engine::{Engine, EventRecord, Recorder};

use super::ReplyLedger;
use super::io::{BoundedWriter, WriterConfig, WriterError};

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
/// `poll_events` reports loop end as exactly one `Terminated`.
pub struct PtySession {
    id: SessionId,
    engine: Engine,
    writer: BoundedWriter,
    ledger: ReplyLedger,
    sender: EventLoopSender,
    loop_handle: Option<JoinHandle<(EventLoop<Pty, Recorder>, State)>>,
    /// Exit status recorded from the loop's `ChildExit` event, if any.
    child_status: Option<ExitStatus>,
    /// Whether the `Terminated` event has been emitted (exactly once).
    terminated_emitted: bool,
    /// Whether `Msg::Shutdown` has been sent.
    shutdown_sent: bool,
}

impl PtySession {
    /// Mint the id; pre-validate cwd (`PtyError::BadCwd`); spawn the child
    /// (pinned `tty::new` + `EventLoop::new` with `drain_on_exit: true`
    /// and `ref_test: false`) on the engine's shared term handle; run the
    /// loop on a dedicated thread; wire `Event::PtyWrite` → atomic reply
    /// headroom (shared [`ReplyLedger`]), `ChildExit` →
    /// `Terminated(Some(status))`, other events discarded.
    pub fn spawn(
        spec: LaunchSpec,
        rows: usize,
        cols: usize,
        cap: WriterConfig,
    ) -> Result<Self, PtyError> {
        let id = SessionId::mint();
        let cwd = spec.cwd;
        if let Some(dir) = &cwd
            && !dir.is_dir()
        {
            return Err(PtyError::BadCwd(dir.clone()));
        }

        let engine = Engine::new(rows, cols);
        let writer = BoundedWriter::new(cap);
        let ledger = ReplyLedger::new(cap.reserved_bytes.min(cap.max_bytes));

        let options = Options {
            shell: Some(Shell::new(spec.program, spec.args)),
            working_directory: cwd,
            env: spec.env.into_iter().collect(),
            drain_on_exit: true,
        };
        let size = WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            // Zero cell sizes are safe: the pinned loop forwards rows and
            // columns to the child; pixel sizes stay 0.
            cell_width: 0,
            cell_height: 0,
        };
        let pty = tty::new(&options, size, id.0).map_err(PtyError::Spawn)?;

        // `drain_on_exit: true`: on child exit the pinned loop can process
        // the exit token before reading master bytes that arrived in the
        // same poll batch; the flag makes the exit path do one final
        // `pty_read` so trailing output (argv/pwd lines) reaches the term.
        let loop_ = EventLoop::new(engine.term_handle(), engine.recorder(), pty, true, false)
            .map_err(PtyError::Spawn)?;
        let sender = loop_.channel();
        let loop_handle = loop_.spawn();

        Ok(Self {
            id,
            engine,
            writer,
            ledger,
            sender,
            loop_handle: Some(loop_handle),
            child_status: None,
            terminated_emitted: false,
            shutdown_sent: false,
        })
    }

    /// This session's generation identity.
    pub fn id(&self) -> SessionId {
        self.id
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
        let mut loop_dead = false;
        let result = self.writer.flush(
            &mut |bytes: &[u8]| {
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

    /// Events emitted so far, in order (non-blocking drain).
    pub fn poll_events(&mut self) -> Vec<SessionEvent> {
        self.process_events();
        let mut events = Vec::new();
        if !self.terminated_emitted && self.terminated() {
            // The loop thread has returned: its final events (notably
            // `ChildExit`) are now visible in the shared stream, so drain
            // once more before reporting.
            self.process_events();
            let status = self.child_status.take();
            self.terminated_emitted = true;
            events.push(SessionEvent::Terminated {
                session: self.id,
                status,
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

    /// Block until the loop thread ends; exit status if a clean child exit.
    ///
    /// Note: the exit status is one-shot across surfaces — if `poll_events`
    /// has already emitted `Terminated`, `join` returns `None` for a clean
    /// exit.
    pub fn join(&mut self) -> Option<ExitStatus> {
        if let Some(handle) = self.loop_handle.take() {
            let _ = handle.join();
        }
        self.process_events();
        self.child_status
    }

    /// Release writer bytes and send `Msg::Shutdown`. Idempotent.
    pub fn close(&mut self) -> usize {
        self.process_events();
        let released = self.writer.cancel();
        self.ledger.release();
        self.send_shutdown();
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
    /// loop); this is the main-thread side of that wiring: `PtyWrite`
    /// replies are accepted atomically through the shared
    /// [`ReplyLedger`] (rejections counted there, never dropped),
    /// `ChildExit` records the status, other events are discarded.
    fn process_events(&mut self) {
        for record in self.engine.drain_events() {
            match record {
                EventRecord::PtyWrite(reply) => {
                    self.ledger.accept(&mut self.writer, &reply);
                }
                EventRecord::ChildExit(status) if self.child_status.is_none() => {
                    self.child_status = Some(status);
                }
                _ => {}
            }
        }
    }
}
