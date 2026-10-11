//! Bounded, ordered pending-write queue (N04a).
//!
//! The pinned `alacritty_terminal` 0.26.0 worker keeps accepted writes in a
//! private unbounded `State::write_list` with no byte caps and no drain
//! acknowledgements (verified in the N04 readiness spike), so nora drives
//! its own writer: bytes are reserved on acceptance, sent oldest first in
//! a single sink call, and released as the sink reports completion.
//!
//! Application and parser-reply bytes share one ordered queue. A strict
//! carve-out of the total budget is reserved for replies so a large paste
//! can never starve protocol replies or shutdown.

use std::io;

/// Per-session outbound budget. `max_bytes` caps all queued bytes
/// (application + reply). `reserved_bytes` is a strict carve-out of
/// `max_bytes` that only reply bytes may occupy, so a large paste can
/// never starve replies. Effective reply budget =
/// min(reserved_bytes, max_bytes); the app budget is the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriterConfig {
    pub max_bytes: usize,
    pub reserved_bytes: usize,
}

#[derive(Debug)]
pub enum WriterError {
    /// The writer has been closed (cancel/shutdown).
    Closed,
    /// Terminal sink error.
    Io(io::Error),
}

// `io::Error` is not `PartialEq`, so the derive cannot express equality.
// Compare by variant; two sink errors are equal without inspecting the
// (uncomparable) inner error.
impl PartialEq for WriterError {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (WriterError::Closed, WriterError::Closed) | (WriterError::Io(_), WriterError::Io(_))
        )
    }
}

/// One queued run of bytes. Chunks of both kinds share the single ordered
/// queue; `reply` only decides which budget the bytes count against.
#[derive(Debug)]
struct Chunk {
    data: Vec<u8>,
    reply: bool,
}

/// Bounded, ordered pending-write queue with a strict reply carve-out.
/// Bytes are accepted in order and never dropped while accepted.
pub struct BoundedWriter {
    config: WriterConfig,
    reply_budget: usize,
    app_budget: usize,
    /// Single ordered queue, oldest first, shared by app and reply bytes.
    queue: std::collections::VecDeque<Chunk>,
    /// Bytes of each kind currently queued (sum = outstanding).
    queued_app: usize,
    queued_reply: usize,
    /// Reused scratch so `flush` can present a slice spanning chunks
    /// without per-call allocation. Capacity is bounded by `max_bytes`.
    assembled: Vec<u8>,
    closed: bool,
}

impl BoundedWriter {
    pub fn new(cap: WriterConfig) -> Self {
        let reply_budget = cap.reserved_bytes.min(cap.max_bytes);
        let app_budget = cap.max_bytes - reply_budget;
        Self {
            config: cap,
            reply_budget,
            app_budget,
            queue: std::collections::VecDeque::new(),
            queued_app: 0,
            queued_reply: 0,
            assembled: Vec::new(),
            closed: false,
        }
    }

    /// Accept ordered application bytes. Returns bytes accepted
    /// (0..=bytes.len(), possibly partial; 0 when the app budget is
    /// full). Accepted bytes are never dropped. Err(Closed) if closed.
    pub fn enqueue(&mut self, bytes: &[u8]) -> Result<usize, WriterError> {
        if self.closed {
            return Err(WriterError::Closed);
        }
        let take = bytes.len().min(self.app_budget - self.queued_app);
        if take > 0 {
            self.push_chunk(&bytes[..take], false);
        }
        Ok(take)
    }

    /// Accept ordered parser/control reply bytes from the reserved
    /// budget. Same return semantics as `enqueue`.
    pub fn enqueue_reply(&mut self, bytes: &[u8]) -> Result<usize, WriterError> {
        if self.closed {
            return Err(WriterError::Closed);
        }
        let take = bytes.len().min(self.reply_budget - self.queued_reply);
        if take > 0 {
            self.push_chunk(&bytes[..take], true);
        }
        Ok(take)
    }

    /// Send up to `limit` queued bytes, oldest first, to `sink` in one
    /// call. `sink` returns the bytes it completed; the un-completed
    /// remainder stays queued at the head, in order. Returns bytes
    /// completed. A sink error keeps all attempted bytes queued and
    /// propagates as `Err(Io)`. Err(Closed) if closed.
    pub fn flush<F: FnMut(&[u8]) -> io::Result<usize>>(
        &mut self,
        sink: &mut F,
        limit: usize,
    ) -> Result<usize, WriterError> {
        if self.closed {
            return Err(WriterError::Closed);
        }
        let n = limit.min(self.outstanding());
        if n == 0 {
            return Ok(0);
        }
        self.assembled.clear();
        self.assembled
            .reserve_exact(n.max(self.assembled.capacity()));
        let mut remaining = n;
        for chunk in self.queue.iter() {
            if remaining == 0 {
                break;
            }
            let take = chunk.data.len().min(remaining);
            self.assembled.extend_from_slice(&chunk.data[..take]);
            remaining -= take;
        }
        let completed = sink(&self.assembled[..n]).map_err(WriterError::Io)?;
        // A sink reports at most the bytes it was given; clamp defensively.
        let completed = completed.min(n);
        self.advance(completed);
        Ok(completed)
    }

    /// Bytes still reserved (queued app + reply). Always ≤ `max_bytes`.
    pub fn outstanding(&self) -> usize {
        self.queued_app + self.queued_reply
    }

    /// Discard all queued bytes, release all capacity, close the writer.
    /// Idempotent: returns bytes released (0 on second call).
    pub fn cancel(&mut self) -> usize {
        if self.closed {
            return 0;
        }
        self.closed = true;
        let released = self.outstanding();
        self.queue.clear();
        self.queued_app = 0;
        self.queued_reply = 0;
        released
    }

    fn push_chunk(&mut self, bytes: &[u8], reply: bool) {
        // Merge into the tail when the kind matches, so repeated enqueues of
        // the same kind stay one chunk (keeps flush assembly cheap).
        if let Some(tail) = self.queue.back_mut()
            && tail.reply == reply
        {
            tail.data.extend_from_slice(bytes);
            if reply {
                self.queued_reply += bytes.len();
            } else {
                self.queued_app += bytes.len();
            }
            return;
        }
        self.queue.push_back(Chunk {
            data: bytes.to_vec(),
            reply,
        });
        if reply {
            self.queued_reply += bytes.len();
        } else {
            self.queued_app += bytes.len();
        }
    }

    /// Drop `n` bytes from the head of the queue, releasing capacity from
    /// the kind they belong to.
    fn advance(&mut self, mut n: usize) {
        while n > 0 {
            let head = self
                .queue
                .front_mut()
                .expect("outstanding bytes are queued");
            let take = head.data.len().min(n);
            if head.reply {
                self.queued_reply -= take;
            } else {
                self.queued_app -= take;
            }
            head.data.drain(..take);
            n -= take;
            if head.data.is_empty() {
                self.queue.pop_front();
            }
        }
    }
}

impl std::fmt::Debug for BoundedWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("BoundedWriter")
            .field("config", &self.config)
            .field("outstanding", &self.outstanding())
            .field("closed", &self.closed)
            .finish()
    }
}
