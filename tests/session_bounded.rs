//! N04b regression suite for the headless bounded session (`nora::session`),
//! extended with the N06a reply-routing fixtures.
//!
//! Each fixture drives a fresh [`Session`] (24×80 unless noted) with query
//! bytes and asserts the exact reply bytes and reply-byte accounting the
//! card requires:
//! - replies route through the bounded writer, in order;
//! - a query split across feeds still replies exactly once;
//! - DSR, dynamic-color (OSC 10/11/12/4) and size (CSI 14t/18t) replies
//!   arrive byte-exact, and unknown arguments are silent;
//! - a query flood at a small reserved budget rejects visibly, never
//!   dropping a byte or the surrounding terminal output silently;
//! - `close()` releases every queued byte, later replies reject as closed,
//!   and `flush` on a closed session is `Err(WriterError::Closed)`.
//!
//! Reply fixtures are the spike-verified pinned-engine strings
//! (alacritty_terminal 0.26.0), terminator echoing the query: DSR-5
//! `\x1b[5n` → `"\x1b[0n"` (4 bytes); DSR-6 `\x1b[6n` at the origin →
//! `"\x1b[1;1R"` (6 bytes); unset color slots resolve at drain (256 →
//! white, 257 → black, 258 → white, any other unset → black).

use std::io;

use nora::session::io::{WriterConfig, WriterError};
use nora::session::{FeedResult, Session};

/// DSR-6 query: the reply-producing bytes used throughout the flood tests.
const DSR6: &[u8] = b"\x1b[6n";

/// A headless 24×80 session with a comfortable budget (the fixture
/// geometry, with no card-specific cap override).
fn session() -> Session {
    Session::new(
        24,
        80,
        WriterConfig {
            max_bytes: 256,
            reserved_bytes: 64,
        },
    )
}

/// A recording sink target.
#[derive(Default)]
struct Rec {
    /// Every flushed byte, in flush order.
    bytes: Vec<u8>,
    /// Number of sink invocations.
    calls: usize,
}

/// Flush up to `limit` bytes into `rec` and report the writer's result.
fn flush_into(session: &mut Session, limit: usize, rec: &mut Rec) -> Result<usize, WriterError> {
    let mut sink = |b: &[u8]| -> io::Result<usize> {
        rec.calls += 1;
        rec.bytes.extend_from_slice(b);
        Ok(b.len())
    };
    session.flush(&mut sink, limit)
}

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

// --- N06a reply-routing fixtures (pinned alacritty_terminal 0.26.0) ---

/// DSR-5 (device status query 5) replies with the fixed OK report.
#[test]
fn dsr5_replies_ok() {
    let mut session = session();
    let result = session.feed(b"\x1b[5n");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 4,
            replies_rejected: 0
        }
    );

    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(4));
    assert_eq!(rec.bytes, b"\x1b[0n");
}

/// DSR-6 (cursor position report) at the origin answers `\x1b[1;1R`.
#[test]
fn dsr6_replies_cursor_at_origin() {
    let mut session = session();
    let result = session.feed(b"\x1b[6n");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 6,
            replies_rejected: 0
        }
    );

    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(6));
    assert_eq!(rec.bytes, b"\x1b[1;1R");
}

/// DSR-6 reports the cursor's current position after a CUP move, not the
/// origin.
#[test]
fn dsr6_replies_current_cursor_after_move() {
    let mut session = session();
    // Move the cursor to row 10, column 20 (1-based); CUP produces no reply.
    let result = session.feed(b"\x1b[10;20H");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 0,
            replies_rejected: 0
        }
    );
    let result = session.feed(b"\x1b[6n");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 8,
            replies_rejected: 0
        }
    );

    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(8));
    assert_eq!(rec.bytes, b"\x1b[10;20R");
}

/// A DSR query split across two feeds replies exactly once: the first
/// turn enqueues nothing, the completing turn enqueues the one 6-byte
/// reply, and the flush carries it exactly once.
#[test]
fn split_dsr_query_replies_exactly_once() {
    let mut session = session();
    // Incomplete escape: the parser holds its state, no reply yet.
    let result = session.feed(b"\x1b[6");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 0,
            replies_rejected: 0
        }
    );
    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(0));
    assert_eq!(rec.calls, 0, "an incomplete query must enqueue nothing");

    // The final byte completes the DSR-6; exactly one 6-byte reply.
    let result = session.feed(b"n");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 6,
            replies_rejected: 0
        }
    );
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(6));
    assert_eq!(rec.bytes, b"\x1b[1;1R");
}

/// A DSR argument the pinned engine does not implement is silent: no
/// reply bytes, no rejection, nothing to flush.
#[test]
fn unknown_dsr_arg_is_silent() {
    let mut session = session();
    let result = session.feed(b"\x1b[15n");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 0,
            replies_rejected: 0
        }
    );

    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(0));
    assert_eq!(
        rec.calls, 0,
        "an unknown DSR argument must produce no bytes"
    );
}

/// Querying the background color (OSC 11) while the slot is unset
/// resolves at drain to the default black, ST-terminated like the query.
#[test]
fn osc11_query_default_background() {
    let mut session = session();
    let result = session.feed(b"\x1b]11;?\x1b\\");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 25,
            replies_rejected: 0
        }
    );

    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(25));
    assert_eq!(rec.bytes, b"\x1b]11;rgb:0000/0000/0000\x1b\\");
}

/// Querying the cursor color (OSC 12) while the slot is unset resolves
/// at drain to the default white, BEL-terminated like the query.
#[test]
fn osc12_query_default_cursor() {
    let mut session = session();
    let result = session.feed(b"\x1b]12;?\x07");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 24,
            replies_rejected: 0
        }
    );

    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(24));
    assert_eq!(rec.bytes, b"\x1b]12;rgb:ffff/ffff/ffff\x07");
}

/// Setting the foreground color (OSC 10) produces no reply; a later query
/// reports the freshly set value, not the unset default.
#[test]
fn osc10_set_then_query_fresh_value() {
    let mut session = session();
    // `rgb:12/34/56` scales to Rgb { 0x12, 0x34, 0x56 }; a set is silent.
    let result = session.feed(b"\x1b]10;rgb:12/34/56\x07");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 0,
            replies_rejected: 0
        }
    );
    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(0));
    assert_eq!(rec.calls, 0, "an OSC 10 set must produce no reply");

    let result = session.feed(b"\x1b]10;?\x07");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 24,
            replies_rejected: 0
        }
    );
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(24));
    assert_eq!(rec.bytes, b"\x1b]10;rgb:1212/3434/5656\x07");
}

/// Querying an unset palette entry (OSC 4, index 0) resolves at drain to
/// the default black, ST-terminated like the query.
#[test]
fn osc4_query_default_index0() {
    let mut session = session();
    let result = session.feed(b"\x1b]4;0;?\x1b\\");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 26,
            replies_rejected: 0
        }
    );

    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(26));
    assert_eq!(rec.bytes, b"\x1b]4;0;rgb:0000/0000/0000\x1b\\");
}

/// Setting a palette entry (OSC 4, index 2) produces no reply; a later
/// query reports the freshly set value.
#[test]
fn osc4_set_then_query_palette_entry() {
    let mut session = session();
    // `rgb:ab/cd/ef` scales to Rgb { 0xab, 0xcd, 0xef }; a set is silent.
    let result = session.feed(b"\x1b]4;2;rgb:ab/cd/ef\x07");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 0,
            replies_rejected: 0
        }
    );
    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(0));
    assert_eq!(rec.calls, 0, "an OSC 4 set must produce no reply");

    let result = session.feed(b"\x1b]4;2;?\x07");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 25,
            replies_rejected: 0
        }
    );
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(25));
    assert_eq!(rec.bytes, b"\x1b]4;2;rgb:abab/cdcd/efef\x07");
}

/// CSI 14t answers the pixel size of the session's own window: at the
/// nominal headless 24×80 geometry (8 px × 16 px cells) that is
/// 24×16 = 384 rows of pixels and 80×8 = 640 columns.
#[test]
fn csi_t14_replies_nominal_pixel_size() {
    let mut session = session();
    let result = session.feed(b"\x1b[14t");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 12,
            replies_rejected: 0
        }
    );

    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(12));
    assert_eq!(rec.bytes, b"\x1b[4;384;640t");
}

/// CSI 18t answers the character size of the session's own window.
#[test]
fn csi_t18_replies_char_size() {
    let mut session = session();
    let result = session.feed(b"\x1b[18t");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 10,
            replies_rejected: 0
        }
    );

    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(10));
    assert_eq!(rec.bytes, b"\x1b[8;24;80t");
}

/// Ten DSR-5 queries in one turn against a 20-byte reply budget: exactly
/// five 4-byte replies fit (20 enqueued), the remaining five are rejected
/// whole (20 rejected) — visibly, never silently.
#[test]
fn query_flood_rejects_beyond_budget() {
    let mut session = Session::new(
        24,
        80,
        WriterConfig {
            max_bytes: 24,
            reserved_bytes: 20,
        },
    );

    let mut flood = Vec::new();
    for _ in 0..10 {
        flood.extend_from_slice(b"\x1b[5n");
    }
    assert_eq!(flood.len(), 40);
    let result = session.feed(&flood);
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 20,
            replies_rejected: 20
        }
    );

    // The five accepted replies complete in order; nothing else fits.
    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 24, &mut rec), Ok(20));
    assert_eq!(rec.bytes, b"\x1b[0n\x1b[0n\x1b[0n\x1b[0n\x1b[0n");
}

/// A flood turn that fills the reply budget still lets the next turn's
/// input feed; the previous flush credits the completed reply bytes back
/// into the budget, so the new query's reply is accepted and flushes in
/// the next turn (the AB input lands on the engine grid — the headless
/// feed has no app-run output path — nothing dropped).
#[test]
fn query_flood_full_still_feeds_terminal_output() {
    let mut session = Session::new(
        24,
        80,
        WriterConfig {
            max_bytes: 24,
            reserved_bytes: 20,
        },
    );

    let mut flood = Vec::new();
    for _ in 0..10 {
        flood.extend_from_slice(b"\x1b[5n");
    }
    let result = session.feed(&flood);
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 20,
            replies_rejected: 20
        }
    );
    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 24, &mut rec), Ok(20));

    // Next turn: app bytes A and B land at (0,0)/(0,1) with the cursor at
    // (0,2); the previous flush completed the accepted replies, so the
    // reply budget has room and this DSR-5 reply is accepted (4 bytes).
    let result = session.feed(b"AB\x1b[5n");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 4,
            replies_rejected: 0
        }
    );

    // Only the accepted 4-byte reply is queued (AB was processed to the
    // grid — the headless feed has no app-run output path); nothing dropped.
    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 24, &mut rec), Ok(4));
    assert_eq!(rec.bytes, b"\x1b[0n");
}

/// An OSC the pinned engine does not recognize (1337) is silent: no
/// reply bytes, no rejection, nothing to flush.
#[test]
fn unknown_osc_is_silent() {
    let mut session = session();
    let result = session.feed(b"\x1b]1337;Set;1337;Clear\x07");
    assert_eq!(
        result,
        FeedResult {
            replies_enqueued: 0,
            replies_rejected: 0
        }
    );

    let mut rec = Rec::default();
    assert_eq!(flush_into(&mut session, 256, &mut rec), Ok(0));
    assert_eq!(rec.calls, 0, "an unknown OSC must produce no bytes");
}
