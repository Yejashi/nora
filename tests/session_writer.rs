//! N04a regression suite for the bounded pending writer (`nora::session::io`).
//!
//! Each fixture drives a fresh [`BoundedWriter`] with tiny caps and fake
//! sinks (recording closures over local state) and asserts the exact byte
//! accounting and ordering the card requires:
//! - a blocked writer never exceeds its declared cap;
//! - capacity is returned on completion, cancel and close;
//! - ordering is preserved across partial writes and WouldBlock;
//! - the reply carve-out is strict: app bytes never consume the reserved
//!   budget, and replies never block on app bytes.

use std::io;

use nora::session::io::{BoundedWriter, WriterConfig, WriterError};

#[test]
fn enqueue_is_capped_at_app_budget() {
    // App budget = 10 - 4 = 6; the reserved 4 bytes stay unreachable by app.
    let mut writer = BoundedWriter::new(WriterConfig {
        max_bytes: 10,
        reserved_bytes: 4,
    });
    assert_eq!(writer.enqueue(b"aaaaaaaaaa"), Ok(6));
    // App budget is full: nothing more accepted, not even one byte.
    assert_eq!(writer.enqueue(b"b"), Ok(0));
    assert_eq!(writer.outstanding(), 6);
}

#[test]
fn reply_uses_reserved_carve_out() {
    let mut writer = BoundedWriter::new(WriterConfig {
        max_bytes: 10,
        reserved_bytes: 4,
    });
    assert_eq!(writer.enqueue(b"abcdef"), Ok(6));
    // Four reply bytes fit in the reserved carve-out...
    for _ in 0..4 {
        assert_eq!(writer.enqueue_reply(b"r"), Ok(1));
    }
    // ...but the fifth does not.
    assert_eq!(writer.enqueue_reply(b"r"), Ok(0));
    assert_eq!(writer.outstanding(), 10);
    // The writer is at its total cap: no app byte fits either.
    assert_eq!(writer.enqueue(b"x"), Ok(0));
}

#[test]
fn flush_completes_in_order() {
    let mut writer = BoundedWriter::new(WriterConfig {
        max_bytes: 100,
        reserved_bytes: 10,
    });
    assert_eq!(writer.enqueue(b"abcdef"), Ok(6));
    assert_eq!(writer.enqueue_reply(b"XY"), Ok(2));
    assert_eq!(writer.enqueue(b"ij"), Ok(2));

    let mut recorded: Vec<u8> = Vec::new();
    let mut sink = |b: &[u8]| -> io::Result<usize> {
        recorded.extend_from_slice(b);
        Ok(b.len())
    };
    // App and reply bytes share one ordered queue, oldest first.
    assert_eq!(writer.flush(&mut sink, 100), Ok(10));
    assert_eq!(recorded, b"abcdefXYij");
    // Completed bytes release their capacity.
    assert_eq!(writer.outstanding(), 0);
}

#[test]
fn flush_partial_write_keeps_head_in_order() {
    let mut writer = BoundedWriter::new(WriterConfig {
        max_bytes: 12,
        reserved_bytes: 4,
    });
    assert_eq!(writer.enqueue(b"abcdefgh"), Ok(8));

    let mut recorded: Vec<u8> = Vec::new();
    let mut partial = |b: &[u8]| -> io::Result<usize> {
        let done = 3.min(b.len());
        recorded.extend_from_slice(&b[..done]);
        Ok(done)
    };
    // The sink completes only the first 3 bytes; the rest stays queued.
    assert_eq!(writer.flush(&mut partial, 100), Ok(3));
    assert_eq!(recorded, b"abc");
    assert_eq!(writer.outstanding(), 5);

    let mut open = |b: &[u8]| -> io::Result<usize> {
        recorded.extend_from_slice(b);
        Ok(b.len())
    };
    // The remainder is drained head-first, in the original order.
    assert_eq!(writer.flush(&mut open, 100), Ok(5));
    assert_eq!(recorded, b"abcdefgh");
    assert_eq!(writer.outstanding(), 0);
}

#[test]
fn flush_wouldblock_keeps_bytes_queued() {
    let mut writer = BoundedWriter::new(WriterConfig {
        max_bytes: 12,
        reserved_bytes: 4,
    });
    assert_eq!(writer.enqueue(b"abcdefgh"), Ok(8));

    // A WouldBlock sink completes nothing: nothing is dropped, nothing is lost.
    let mut blocked = |_b: &[u8]| -> io::Result<usize> { Ok(0) };
    assert_eq!(writer.flush(&mut blocked, 100), Ok(0));
    assert_eq!(writer.outstanding(), 8);

    let mut recorded: Vec<u8> = Vec::new();
    let mut open = |b: &[u8]| -> io::Result<usize> {
        recorded.extend_from_slice(b);
        Ok(b.len())
    };
    assert_eq!(writer.flush(&mut open, 100), Ok(8));
    assert_eq!(recorded, b"abcdefgh");
    assert_eq!(writer.outstanding(), 0);
}

#[test]
fn sink_io_error_keeps_bytes_queued() {
    let mut writer = BoundedWriter::new(WriterConfig {
        max_bytes: 12,
        reserved_bytes: 4,
    });
    assert_eq!(writer.enqueue(b"abcdefgh"), Ok(8));

    // A failing sink must keep every attempted byte queued and propagate.
    let mut failing = |_b: &[u8]| -> io::Result<usize> { Err(io::Error::other("boom")) };
    let err = writer
        .flush(&mut failing, 100)
        .expect_err("sink error must propagate as WriterError::Io");
    match &err {
        WriterError::Io(e) => assert_eq!(e.kind(), io::ErrorKind::Other),
        other => panic!("expected WriterError::Io, got {other:?}"),
    }
    assert_eq!(writer.outstanding(), 8);

    let mut recorded: Vec<u8> = Vec::new();
    let mut open = |b: &[u8]| -> io::Result<usize> {
        recorded.extend_from_slice(b);
        Ok(b.len())
    };
    assert_eq!(writer.flush(&mut open, 100), Ok(8));
    assert_eq!(recorded, b"abcdefgh");
    assert_eq!(writer.outstanding(), 0);
}

#[test]
fn blocked_writer_never_exceeds_cap() {
    // App budget = 32 - 8 = 24; the sink never completes a byte, so every
    // accepted byte stays queued forever and caps total acceptance.
    let mut writer = BoundedWriter::new(WriterConfig {
        max_bytes: 32,
        reserved_bytes: 8,
    });
    let mut blocked = |_b: &[u8]| -> io::Result<usize> { Ok(0) };

    let mut accepted = 0usize;
    for _ in 0..1000 {
        accepted += writer
            .enqueue(&[b'a'; 4])
            .expect("enqueue must not fail while open");
        assert!(writer.outstanding() <= 32);
        writer
            .flush(&mut blocked, 32)
            .expect("flush must not fail while blocked");
    }
    assert_eq!(accepted, 24);
    assert_eq!(writer.outstanding(), 24);
}

#[test]
fn cancel_releases_all_and_closes() {
    let mut writer = BoundedWriter::new(WriterConfig {
        max_bytes: 10,
        reserved_bytes: 4,
    });
    assert_eq!(writer.enqueue(b"abc"), Ok(3));
    assert_eq!(writer.enqueue_reply(b"xy"), Ok(2));

    assert_eq!(writer.cancel(), 5);
    assert_eq!(writer.outstanding(), 0);
    // Idempotent: the second cancel releases nothing.
    assert_eq!(writer.cancel(), 0);
    // And the writer is closed: every byte path rejects.
    assert_eq!(writer.enqueue(b"x"), Err(WriterError::Closed));
    assert_eq!(writer.enqueue_reply(b"x"), Err(WriterError::Closed));
    let mut sink = |_b: &[u8]| -> io::Result<usize> { Ok(0) };
    assert_eq!(writer.flush(&mut sink, 10), Err(WriterError::Closed));
}

#[test]
fn empty_enqueue_is_noop() {
    let mut writer = BoundedWriter::new(WriterConfig {
        max_bytes: 10,
        reserved_bytes: 4,
    });
    assert_eq!(writer.enqueue(b""), Ok(0));

    let mut sink_called = false;
    let mut sink = |b: &[u8]| -> io::Result<usize> {
        sink_called = true;
        Ok(b.len())
    };
    // An empty queue completes zero bytes without touching the sink.
    assert_eq!(writer.flush(&mut sink, 100), Ok(0));
    assert!(!sink_called, "sink must not be called for an empty queue");
    assert_eq!(writer.outstanding(), 0);
}
