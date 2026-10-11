//! N04b regression suite for the headless bounded session (`nora::session`).
//!
//! Each fixture drives a fresh [`Session`] with DSR query bytes and asserts
//! the exact reply-byte accounting the card requires:
//! - replies route through the bounded writer, in order;
//! - a query split across feeds still replies exactly once;
//! - a DSR flood at a small reserved budget rejects visibly, never dropping
//!   a byte silently;
//! - `close()` releases every queued byte, later replies reject as closed,
//!   and `flush` on a closed session is `Err(WriterError::Closed)`.
//!
//! Reply fixtures are the spike-verified pinned-engine strings
//! (alacritty_terminal 0.26.0): DSR-5 `\x1b[5n` → `"\x1b[0n"` (4 bytes);
//! DSR-6 `\x1b[6n` at the origin → `"\x1b[1;1R"` (6 bytes).

use std::io;

use nora::session::io::{WriterConfig, WriterError};
use nora::session::{FeedResult, Session};

/// DSR-6 query: the reply-producing bytes used throughout the flood tests.
const DSR6: &[u8] = b"\x1b[6n";

#[test]
fn replies_are_delivered_in_order() {
    let mut session = Session::new(
        24,
        80,
        WriterConfig {
            max_bytes: 256,
            reserved_bytes: 64,
        },
    );
    // Both queries fit the reply budget: 4 + 6 = 10 enqueued, none rejected.
    let result = session.feed(b"\x1b[5n\x1b[6n");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 10,
            replies_rejected: 0
        }
    );

    let mut recorded: Vec<u8> = Vec::new();
    let mut sink = |b: &[u8]| -> io::Result<usize> {
        recorded.extend_from_slice(b);
        Ok(b.len())
    };
    assert_eq!(session.flush(&mut sink, 256), Ok(10));
    // DSR-5's 4-byte reply precedes DSR-6's 6-byte reply: order preserved.
    assert_eq!(recorded, b"\x1b[0n\x1b[1;1R");
}

#[test]
fn split_query_across_turns_still_replies() {
    let mut session = Session::new(
        24,
        80,
        WriterConfig {
            max_bytes: 256,
            reserved_bytes: 64,
        },
    );
    // Incomplete escape: the parser holds its state, no reply yet.
    let result = session.feed(b"\x1b[6");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 0,
            replies_rejected: 0
        }
    );
    // The final byte completes the DSR-6; the reply arrives exactly once.
    let result = session.feed(b"n");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 6,
            replies_rejected: 0
        }
    );

    let mut recorded: Vec<u8> = Vec::new();
    let mut sink = |b: &[u8]| -> io::Result<usize> {
        recorded.extend_from_slice(b);
        Ok(b.len())
    };
    assert_eq!(session.flush(&mut sink, 256), Ok(6));
    assert_eq!(recorded, b"\x1b[1;1R");
}

#[test]
fn query_flood_is_rejected_visibly() {
    // Reply budget = 16 < 10 x 6: the flood must reject visibly, never
    // drop a byte silently.
    let mut session = Session::new(
        24,
        80,
        WriterConfig {
            max_bytes: 32,
            reserved_bytes: 16,
        },
    );

    let mut flood = Vec::new();
    for _ in 0..10 {
        flood.extend_from_slice(DSR6);
    }
    assert_eq!(flood.len(), 40);
    // Exactly two 6-byte replies fit the 16-byte budget: 12 enqueued, the
    // remaining eight replies (48 bytes) rejected.
    let result = session.feed(&flood);
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 12,
            replies_rejected: 48
        }
    );

    // The budget is full (12 queued, 4 bytes free < 6): one more DSR-6
    // rejects in full instead of dropping silently.
    let result = session.feed(DSR6);
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 0,
            replies_rejected: 6
        }
    );

    let mut recorded: Vec<u8> = Vec::new();
    let mut sink = |b: &[u8]| -> io::Result<usize> {
        recorded.extend_from_slice(b);
        Ok(b.len())
    };
    // Exactly the two accepted replies, in order.
    assert_eq!(session.flush(&mut sink, 32), Ok(12));
    assert_eq!(recorded, b"\x1b[1;1R\x1b[1;1R");
}

#[test]
fn close_releases_all_and_rejects_later_replies() {
    let mut session = Session::new(
        24,
        80,
        WriterConfig {
            max_bytes: 64,
            reserved_bytes: 16,
        },
    );
    let result = session.feed(b"\x1b[5n");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 4,
            replies_rejected: 0
        }
    );

    // close() releases every queued byte and closes the writer.
    assert_eq!(session.close(), 4);

    // The engine still answers DSR-6, but the closed writer rejects the
    // reply instead of dropping it silently.
    let result = session.feed(DSR6);
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 0,
            replies_rejected: 6
        }
    );
}

#[test]
fn flush_after_close_is_closed_error() {
    let mut session = Session::new(
        24,
        80,
        WriterConfig {
            max_bytes: 64,
            reserved_bytes: 16,
        },
    );
    assert_eq!(session.close(), 0);

    let mut sink = |_b: &[u8]| -> io::Result<usize> { Ok(0) };
    assert_eq!(session.flush(&mut sink, 16), Err(WriterError::Closed));
}

#[test]
fn feed_empty_is_noop() {
    let mut session = Session::new(
        24,
        80,
        WriterConfig {
            max_bytes: 64,
            reserved_bytes: 16,
        },
    );
    let result = session.feed(b"");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 0,
            replies_rejected: 0
        }
    );

    let mut sink_called = false;
    let mut sink = |b: &[u8]| -> io::Result<usize> {
        sink_called = true;
        Ok(b.len())
    };
    // An empty queue completes zero bytes without touching the sink.
    assert_eq!(session.flush(&mut sink, 16), Ok(0));
    assert!(!sink_called, "sink must not be called for an empty queue");
}
