//! Session layer: bounded, ordered outbound I/O per terminal session.
//!
//! N04a introduced [`io::BoundedWriter`], a byte-budgeted pending-write
//! queue that caps queued application and parser-reply bytes and returns
//! capacity as writes complete. N04b adds [`Session`]: a headless session
//! pairing the N03 [`crate::engine::Engine`] with that writer. N05a adds
//! [`pty::PtySession`]: the same bounded outbound I/O over a real PTY
//! child whose output drives the engine through the pinned event loop.
//!
//! The headless session is synchronous: parser replies are produced
//! inside [`Session::feed`] and enqueued in the same call, so replies
//! never block under the engine lock — by construction.
//!
//! N06a routes every drained engine event through the shared
//! [`resolve_record`]: `PtyWrite` bytes and the formatter-based query
//! events (color, text-area size, clipboard load) become reply strings
//! that both session types accept atomically through the shared
//! [`ReplyLedger`].

use std::collections::VecDeque;

use alacritty_terminal::event::WindowSize;

use crate::engine::{Engine, EventRecord};

use io::{BoundedWriter, WriterConfig, WriterError};

pub mod io;
pub mod pty;
pub mod pump;

pub use pty::{
    DRAIN_TIMEOUT, LaunchSpec, Lifecycle, PtyError, PtySession, SessionEvent, SessionId,
    SessionOptions,
};

/// Nominal headless cell width in pixels (no UI window exists yet, so
/// pixel-size replies report columns times this value).
pub const NOMINAL_CELL_WIDTH: u16 = 8;

/// Nominal headless cell height in pixels (no UI window exists yet, so
/// pixel-size replies report rows times this value).
pub const NOMINAL_CELL_HEIGHT: u16 = 16;

/// Resolve one drained event record into a reply string.
///
/// `PtyWrite` is already engine-formatted. `ColorRequest` applies the
/// engine's formatter to the raw color table, where unset slots resolve
/// to their defaults (no seeding). `TextAreaSizeRequest` applies the
/// engine's formatter to `size` — the session's own window geometry, so
/// replies never cross sessions. `ClipboardLoad` formats an empty
/// clipboard (no clipboard state yet — unreachable under
/// `Osc52::Disabled` until N06b). `ClipboardStore` and every other
/// variant produces no reply (UI routing is a later card).
pub(crate) fn resolve_record(
    engine: &Engine,
    size: WindowSize,
    palette: &crate::render::palette::Palette,
    record: EventRecord,
) -> Option<String> {
    match record {
        EventRecord::PtyWrite(reply) => Some(reply),
        // A color the application set itself, else the palette the window
        // draws with (OSC 10/11 replies let programs pick light or dark
        // themes, so they must match the screen).
        EventRecord::ColorRequest(index, format) => Some(format(
            engine
                .color(index)
                .unwrap_or_else(|| palette.get(index.min(palette_last()))),
        )),
        EventRecord::TextAreaSizeRequest(format) => Some(format(size)),
        EventRecord::ClipboardLoad(_, format) => Some(format("")),
        _ => None,
    }
}

/// The last valid palette index.
fn palette_last() -> usize {
    alacritty_terminal::term::color::COUNT - 1
}

/// Reply-byte accounting for one parser turn ([`Session::feed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedResult {
    /// Reply bytes accepted into the writer queue this turn.
    pub replies_enqueued: usize,
    /// Reply bytes rejected this turn: the reply budget was full or the
    /// writer was already closed (visible, never silent).
    pub replies_rejected: usize,
}

/// One enqueued run tracked by a session, in writer-queue order.
struct Run {
    /// Whether the run occupies the reply carve-out.
    reply: bool,
    /// Bytes still queued in this run.
    len: usize,
}

/// Shared reply-acceptance ledger for [`Session`] and [`pty::PtySession`].
///
/// [`BoundedWriter`] is byte-granular and exposes no per-kind headroom, so
/// the ledger mirrors the strict reply carve-out: a reply is accepted only
/// if it fits wholly in the remaining headroom (otherwise rejected in
/// full — never split mid-stream), accepted bytes are tracked per run, and
/// bytes completed by `flush` are credited back per kind. Rejections are
/// counted (visible, never silent).
struct ReplyLedger {
    /// `min(reserved_bytes, max_bytes)`, the strict reply carve-out.
    budget: usize,
    /// Reply bytes currently queued in the writer.
    queued: usize,
    /// Kind-tagged mirror of the writer queue (oldest first), used to
    /// account completed bytes per kind.
    runs: VecDeque<Run>,
    /// Total reply bytes rejected (budget full or writer closed).
    rejected: usize,
}

impl ReplyLedger {
    fn new(budget: usize) -> Self {
        Self {
            budget,
            queued: 0,
            runs: VecDeque::new(),
            rejected: 0,
        }
    }

    /// Reply bytes still available to new replies.
    fn headroom(&self) -> usize {
        self.budget - self.queued
    }

    /// Accept one reply atomically against `writer`; returns the accepted
    /// byte count (rejections accumulate in the ledger).
    fn accept(&mut self, writer: &mut BoundedWriter, reply: &str) -> usize {
        let accepted = if reply.len() <= self.headroom() {
            // Fits wholly; the strict carve-out guarantees the writer
            // accepts all of it (0 only when closed).
            match writer.enqueue_reply(reply.as_bytes()) {
                Ok(n) => n,
                Err(WriterError::Closed) => 0,
                // Sink errors surface in `flush` only.
                Err(WriterError::Io(_)) => 0,
            }
        } else {
            // Atomic: reject the whole reply rather than enqueue a
            // fragment the protocol stream could not complete.
            0
        };
        if accepted > 0 {
            self.queued += accepted;
            self.runs.push_back(Run {
                reply: true,
                len: accepted,
            });
        }
        self.rejected += reply.len() - accepted;
        accepted
    }

    /// Record an accepted application run (it does not occupy the
    /// carve-out, but it occupies the writer queue).
    fn track_app(&mut self, len: usize) {
        self.runs.push_back(Run { reply: false, len });
    }

    /// Release `completed` bytes from the head of the tracked runs,
    /// crediting the reply headroom for any reply bytes completed.
    fn complete(&mut self, mut completed: usize) {
        while completed > 0 {
            let run = self
                .runs
                .front_mut()
                .expect("completed bytes were enqueued first");
            let take = run.len.min(completed);
            if run.reply {
                self.queued -= take;
            }
            run.len -= take;
            completed -= take;
            if run.len == 0 {
                self.runs.pop_front();
            }
        }
    }

    /// Drop all tracked state (the writer was cancelled).
    fn release(&mut self) {
        self.runs.clear();
        self.queued = 0;
    }
}

/// A headless terminal session: N03 [`Engine`] + N04a [`BoundedWriter`].
///
/// [`Session::feed`] runs one parser turn and then drains every event the
/// turn produced: each record is resolved through the shared
/// [`resolve_record`] and the resulting replies are accepted atomically
/// through the shared [`ReplyLedger`]; non-reply events are consumed and
/// discarded (UI routing is a later card).
pub struct Session {
    engine: Engine,
    /// The session's own window geometry (rows/cols + nominal cell
    /// sizes); size replies resolve against it.
    size: WindowSize,
    writer: BoundedWriter,
    ledger: ReplyLedger,
    /// Last title set by the terminal (latest wins), `None` when the
    /// title was reset or never set. Folded on the main thread from
    /// drained [`EventRecord::Title`]/[`EventRecord::ResetTitle`]
    /// records; no cross-thread mutation of shared state.
    title: Option<String>,
}

impl Session {
    /// Create a headless session with a `rows` × `cols` engine and an
    /// outbound writer budgeted by `cap`.
    pub fn new(rows: usize, cols: usize, cap: WriterConfig) -> Self {
        Self {
            engine: Engine::new(rows, cols),
            // Nominal headless geometry: pixel-size replies report
            // rows/cols times the nominal cell size.
            size: WindowSize {
                num_lines: rows as u16,
                num_cols: cols as u16,
                cell_width: NOMINAL_CELL_WIDTH,
                cell_height: NOMINAL_CELL_HEIGHT,
            },
            writer: BoundedWriter::new(cap),
            ledger: ReplyLedger::new(cap.reserved_bytes.min(cap.max_bytes)),
            title: None,
        }
    }

    /// Feed raw PTY bytes: one parser turn, then drain all events that
    /// turn produced, in order. Returns this turn's reply accounting.
    pub fn feed(&mut self, bytes: &[u8]) -> FeedResult {
        self.engine.feed(bytes);
        let rejected_before = self.ledger.rejected;
        let mut enqueued = 0;
        // One atomic pass: `Engine::drain_events` takes the whole
        // pending stream in arrival order; every record is resolved to
        // a reply (or discarded for non-reply variants) in that order.
        for record in self.engine.drain_events() {
            // Fold presentation state on the main thread: latest title
            // wins; a reset clears it.
            match &record {
                EventRecord::Title(t) => self.title = Some(t.clone()),
                EventRecord::ResetTitle => self.title = None,
                _ => {}
            }
            if let Some(reply) = resolve_record(
                &self.engine,
                self.size,
                &crate::render::palette::Palette::default(),
                record,
            ) {
                enqueued += self.ledger.accept(&mut self.writer, &reply);
            }
        }
        FeedResult {
            replies_enqueued: enqueued,
            replies_rejected: self.ledger.rejected - rejected_before,
        }
    }

    /// The last title set by the terminal, if any (folded on the main
    /// thread; see the struct docs).
    pub fn title(&self) -> Option<String> {
        self.title.clone()
    }

    /// Bytes still reserved by the session writer.
    pub fn outstanding(&self) -> usize {
        self.writer.outstanding()
    }

    /// Send queued bytes, oldest first, to `sink` through the bounded
    /// writer.
    pub fn flush<F: FnMut(&[u8]) -> std::io::Result<usize>>(
        &mut self,
        sink: &mut F,
        limit: usize,
    ) -> Result<usize, WriterError> {
        let completed = self.writer.flush(sink, limit)?;
        self.ledger.complete(completed);
        Ok(completed)
    }

    /// Release every queued byte and close the writer. Idempotent:
    /// returns the bytes released (0 on subsequent calls).
    pub fn close(&mut self) -> usize {
        let released = self.writer.cancel();
        self.ledger.release();
        released
    }
}
