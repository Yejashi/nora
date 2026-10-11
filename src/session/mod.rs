//! Session layer: bounded, ordered outbound I/O per terminal session.
//!
//! N04a introduced [`io::BoundedWriter`], a byte-budgeted pending-write
//! queue that caps queued application and parser-reply bytes and returns
//! capacity as writes complete. N04b adds [`Session`]: a headless session
//! pairing the N03 [`crate::engine::Engine`] with that writer.
//!
//! The session is headless: parser replies are produced synchronously by
//! the parser inside [`Session::feed`] and enqueued in the same call, so
//! replies never block under the engine lock — by construction.

use std::collections::VecDeque;

use crate::engine::Engine;

use io::{BoundedWriter, WriterConfig, WriterError};

pub mod io;

/// Reply-byte accounting for one parser turn ([`Session::feed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedResult {
    /// Reply bytes accepted into the writer queue this turn.
    pub replies_enqueued: usize,
    /// Reply bytes rejected this turn: the reply budget was full or the
    /// writer was already closed (visible, never silent).
    pub replies_rejected: usize,
}

/// One enqueued run tracked by [`Session`], in writer-queue order.
struct Run {
    /// Whether the run occupies the reply carve-out.
    reply: bool,
    /// Bytes still queued in this run.
    len: usize,
}

/// A headless terminal session: N03 [`Engine`] + N04a [`BoundedWriter`].
///
/// [`Session::feed`] runs one parser turn and then drains every event the
/// turn produced: parser replies are enqueued in order and non-reply
/// events are consumed and discarded (UI routing is a later card).
///
/// Replies are accepted atomically: a reply is enqueued only if it fits
/// wholly in the remaining reply headroom, otherwise it is rejected in
/// full and shown in [`FeedResult`]. The session mirrors the writer's
/// reply capacity (the writer does not expose per-kind headroom) so that
/// capacity completed by [`Session::flush`] becomes available to later
/// replies in the same turn order.
pub struct Session {
    engine: Engine,
    writer: BoundedWriter,
    /// `min(reserved_bytes, max_bytes)`, the strict reply carve-out.
    reply_budget: usize,
    /// Reply bytes currently queued in the writer.
    queued_replies: usize,
    /// Kind-tagged mirror of the writer queue (oldest first), used to
    /// account completed bytes per kind.
    runs: VecDeque<Run>,
}

impl Session {
    /// Create a headless session with a `rows` × `cols` engine and an
    /// outbound writer budgeted by `cap`.
    pub fn new(rows: usize, cols: usize, cap: WriterConfig) -> Self {
        Self {
            engine: Engine::new(rows, cols),
            writer: BoundedWriter::new(cap),
            reply_budget: cap.reserved_bytes.min(cap.max_bytes),
            queued_replies: 0,
            runs: VecDeque::new(),
        }
    }

    /// Feed raw PTY bytes: one parser turn, then drain all events that
    /// turn produced, in order. Returns this turn's reply accounting.
    pub fn feed(&mut self, bytes: &[u8]) -> FeedResult {
        self.engine.feed(bytes);
        let mut enqueued = 0;
        let mut rejected = 0;
        // `Engine::replies` accumulates (does not consume), so the drain
        // is: process everything captured since the last turn, clear the
        // recorder, and re-check until nothing is pending.
        loop {
            let pending = self.engine.replies();
            if pending.is_empty() {
                break;
            }
            for reply in pending {
                let accepted = if reply.len() <= self.reply_headroom() {
                    // Fits wholly; the strict carve-out guarantees the
                    // writer accepts all of it (0 only when closed).
                    match self.writer.enqueue_reply(reply.as_bytes()) {
                        Ok(n) => n,
                        Err(WriterError::Closed) => 0,
                        // Sink errors surface in `flush` only.
                        Err(WriterError::Io(_)) => 0,
                    }
                } else {
                    // Atomic: reject the whole reply rather than enqueue
                    // a fragment the protocol stream could not complete.
                    0
                };
                if accepted > 0 {
                    self.queued_replies += accepted;
                    self.runs.push_back(Run {
                        reply: true,
                        len: accepted,
                    });
                }
                enqueued += accepted;
                rejected += reply.len() - accepted;
            }
            self.engine.clear_events();
        }
        FeedResult {
            replies_enqueued: enqueued,
            replies_rejected: rejected,
        }
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
        self.complete(completed);
        Ok(completed)
    }

    /// Release every queued byte and close the writer. Idempotent:
    /// returns the bytes released (0 on subsequent calls).
    pub fn close(&mut self) -> usize {
        let released = self.writer.cancel();
        self.runs.clear();
        self.queued_replies = 0;
        released
    }

    /// Reply bytes still available to new replies.
    fn reply_headroom(&self) -> usize {
        self.reply_budget - self.queued_replies
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
                self.queued_replies -= take;
            }
            run.len -= take;
            completed -= take;
            if run.len == 0 {
                self.runs.pop_front();
            }
        }
    }
}
