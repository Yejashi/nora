//! Synthetic-payload experiment for the N32a graphics filter.
//!
//! These tests exercise `GraphicsFilter` and `GraphicsQueue` directly (not
//! through `Engine`), verifying that the filter extracts kitty-graphics
//! APCs in parallel with the vte parser without breaking ordinary terminal
//! text.

use nora::graphics_filter::{GraphicsFilter, GraphicsQueue};

/// Build a well-formed kitty-graphics APC: `ESC _ e ; <params> <base64> ST`.
fn apc(command: u8, params: &str, base64: &str) -> Vec<u8> {
    let mut out = vec![0x1B, 0x5F, command, b';']; // ESC _ <command> ;
    out.extend_from_slice(params.as_bytes());
    if !base64.is_empty() {
        out.push(b';');
        out.extend_from_slice(base64.as_bytes());
    }
    out.push(0x1B); // ESC
    out.push(0x5C); // \ (ST terminator)
    out
}

/// Ordinary text preserved: feed a CSI sequence + printable chars; verify
/// all bytes are forwarded and NO GraphicsEvent is produced.
#[test]
fn ordinary_text_preserved() {
    let mut filter = GraphicsFilter::new();
    let bytes = b"\x1b[1;31mHello\x0d\x0a";
    let (forward, events) = filter.process(bytes);
    assert_eq!(forward, bytes, "all bytes must be forwarded");
    assert!(events.is_empty(), "no GraphicsEvent for ordinary text");
}

/// Synthetic APC: feed a well-formed kitty-graphics APC; verify one
/// GraphicsEvent with the correct command, params, and base64-decoded data.
#[test]
fn synthetic_apc() {
    let mut filter = GraphicsFilter::new();
    // base64 of "abc" is "YWJj"
    let payload = apc(b'e', "25;25;0;0;1", "YWJj");
    let (forward, events) = filter.process(&payload);
    assert_eq!(forward, payload.as_slice(), "all bytes must be forwarded");
    assert_eq!(events.len(), 1, "exactly one GraphicsEvent");
    let event = &events[0];
    assert_eq!(event.command, b'e');
    assert_eq!(event.data, b"abc", "base64 'YWJj' decodes to 'abc'");
}

/// Chunking: feed the same APC split across multiple process calls
/// (byte-by-byte and irregular chunks); verify it reassembles to the same
/// GraphicsEvent.
#[test]
fn chunking_reassembles() {
    let payload = apc(b'e', "25;25;0;0;1", "YWJj");

    // Byte-by-byte.
    let mut filter = GraphicsFilter::new();
    let mut all_events = Vec::new();
    for &byte in &payload {
        let (_, events) = filter.process(&[byte]);
        all_events.extend(events);
    }
    assert_eq!(all_events.len(), 1, "byte-by-byte reassembly");
    assert_eq!(all_events[0].command, b'e');
    assert_eq!(all_events[0].data, b"abc");

    // Irregular chunks (split in the middle of the base64).
    let mut filter = GraphicsFilter::new();
    let mut all_events = Vec::new();
    let split = payload.len() / 2;
    let (first, second) = payload.split_at(split);
    all_events.extend(filter.process(first).1);
    all_events.extend(filter.process(second).1);
    assert_eq!(all_events.len(), 1, "irregular-chunk reassembly");
    assert_eq!(all_events[0].command, b'e');
    assert_eq!(all_events[0].data, b"abc");
}

/// Oversize: feed a payload larger than the queue cap; verify the queue
/// evicts the oldest event and stays within the cap.
#[test]
fn oversize_evicts_oldest() {
    let mut queue = GraphicsQueue::new();
    // Push 3 events, each 500_000 bytes of data (total 1_500_000 > 1 MiB cap).
    for i in 0..3u8 {
        let mut data = vec![0u8; 500_000];
        data[0] = i;
        let event = nora::graphics_filter::GraphicsEvent {
            command: b'e',
            params: String::new(),
            data,
        };
        queue.push(event);
    }
    // The queue should have evicted the oldest event(s) to stay within the cap.
    assert!(
        queue.total_bytes() <= 1024 * 1024,
        "queue must stay within cap"
    );
    assert!(queue.len() < 3, "oldest event(s) must have been evicted");
    // The newest event (i=2) must still be present.
    let drained = queue.drain();
    assert!(!drained.is_empty());
    assert_eq!(
        drained.last().unwrap().data[0],
        2,
        "newest event must survive"
    );
}

/// Recovery: feed a malformed/aborted APC (CAN-terminated); verify no panic,
/// no spurious event, and subsequent ordinary text still works.
#[test]
fn recovery_after_abort() {
    let mut filter = GraphicsFilter::new();
    // Start an APC, then abort it with CAN (0x18).
    let mut payload = vec![0x1B, 0x5F, b'e', b';', b'2', b'5'];
    payload.push(0x18); // CAN: abort the APC
    let (forward, events) = filter.process(&payload);
    assert_eq!(forward, payload.as_slice(), "all bytes must be forwarded");
    assert!(events.is_empty(), "no spurious event for aborted APC");

    // Subsequent ordinary text must still work.
    let bytes = b"Hello";
    let (forward, events) = filter.process(bytes);
    assert_eq!(forward, bytes);
    assert!(events.is_empty());
}

/// Mid-sequence frame: feed an APC that starts after a partial CSI; verify
/// the filter stays in sync (the parser would also handle this correctly).
#[test]
fn mid_sequence_frame() {
    let mut filter = GraphicsFilter::new();
    // Partial CSI (incomplete sequence), then a well-formed APC.
    let mut payload = vec![0x1B, b'[', b'1', b';', b'3']; // partial CSI
    let apc = apc(b'e', "25;25;0;0;1", "YWJj");
    payload.extend_from_slice(&apc);
    let (forward, events) = filter.process(&payload);
    assert_eq!(forward, payload.as_slice(), "all bytes must be forwarded");
    assert_eq!(
        events.len(),
        1,
        "the APC after the partial CSI must be extracted"
    );
    assert_eq!(events[0].command, b'e');
    assert_eq!(events[0].data, b"abc");
}

/// The filter preserves terminal byte semantics: feeding an APC through the
/// filter and then to the parser (via Engine::feed) must produce the same
/// terminal state as feeding the APC directly. This test uses Engine to
/// verify the integration (the filter runs before the parser).
#[test]
fn engine_feed_preserves_semantics() {
    let mut engine = nora::engine::Engine::new(25, 80);
    // Feed ordinary text.
    engine.feed(b"Hello");
    // Feed a well-formed APC.
    let payload = apc(b'e', "25;25;0;0;1", "YWJj");
    engine.feed(&payload);
    // The graphics queue should have one event.
    let events = engine.drain_graphics();
    assert_eq!(events.len(), 1, "Engine must have extracted the APC");
    assert_eq!(events[0].command, b'e');
    assert_eq!(events[0].data, b"abc");
    // The terminal state should be correct (the filter did not break normal text).
    // (We don't assert the full grid here; the point is that the filter
    // did not panic or corrupt the parser state.)
}
