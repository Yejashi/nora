//! Synthetic-payload tests for the N32a/N34 graphics filter.
//!
//! These tests exercise `GraphicsFilter` and `GraphicsQueue` directly (not
//! through `Engine`), verifying that the filter extracts kitty-graphics APCs
//! (the real protocol: `G` + comma-separated `key=value` control data, per
//! ADR-011) in parallel with the vte parser without breaking ordinary
//! terminal text.

use nora::graphics_filter::{GraphicsEvent, GraphicsFilter, GraphicsQueue, GraphicsSignal};

/// The command of a filter signal.
fn command(signal: &(usize, GraphicsSignal)) -> &GraphicsEvent {
    match &signal.1 {
        GraphicsSignal::Command(event) => event,
        other => panic!("not a command: {other:?}"),
    }
}

/// Build a well-formed kitty-graphics APC: `ESC _ G<control data>;<base64> ST`.
///
/// `payload` is the bytes between `ESC _` and `ESC \`, starting with the `G`
/// command character (e.g. `Gs=2,v=2;YWJj`).
fn apc(payload: &str) -> Vec<u8> {
    let mut out = vec![0x1B, 0x5F]; // ESC _
    out.extend_from_slice(payload.as_bytes());
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
/// GraphicsEvent with the correct command, keys, and base64-decoded data.
#[test]
fn synthetic_apc() {
    let mut filter = GraphicsFilter::new();
    // base64 of "abc" is "YWJj".
    let payload = apc("Gs=2,v=2;YWJj");
    let (forward, events) = filter.process(&payload);
    assert_eq!(forward, payload.as_slice(), "all bytes must be forwarded");
    assert_eq!(events.len(), 1, "exactly one GraphicsEvent");
    let event = command(&events[0]);
    assert_eq!(event.command, b'G');
    assert_eq!(event.get("s"), Some("2"));
    assert_eq!(event.get("v"), Some("2"));
    assert_eq!(event.data, b"abc", "base64 'YWJj' decodes to 'abc'");
}

/// Chunking: feed the same APC split across multiple process calls
/// (byte-by-byte and irregular chunks); verify it reassembles to the same
/// GraphicsEvent.
#[test]
fn chunking_reassembles() {
    let payload = apc("Gs=2,v=2;YWJj");

    // Byte-by-byte.
    let mut filter = GraphicsFilter::new();
    let mut all_events = Vec::new();
    for &byte in &payload {
        let (_, events) = filter.process(&[byte]);
        all_events.extend(events);
    }
    assert_eq!(all_events.len(), 1, "byte-by-byte reassembly");
    assert_eq!(command(&all_events[0]).command, b'G');
    assert_eq!(command(&all_events[0]).data, b"abc");

    // Irregular chunks (split in the middle of the base64).
    let mut filter = GraphicsFilter::new();
    let mut all_events = Vec::new();
    let split = payload.len() / 2;
    let (first, second) = payload.split_at(split);
    all_events.extend(filter.process(first).1);
    all_events.extend(filter.process(second).1);
    assert_eq!(all_events.len(), 1, "irregular-chunk reassembly");
    assert_eq!(command(&all_events[0]).command, b'G');
    assert_eq!(command(&all_events[0]).data, b"abc");
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
        let event = GraphicsEvent {
            command: b'G',
            keys: Vec::new(),
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
    let mut payload = vec![0x1B, 0x5F, b'G', b's', b'='];
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
    // Partial CSI (incomplete sequence), then a well-formed APC.
    let mut payload = vec![0x1B, b'[', b'1', b';', b'3']; // partial CSI
    let apc = apc("Gs=2,v=2;YWJj");
    payload.extend_from_slice(&apc);
    let mut filter = GraphicsFilter::new();
    let (forward, events) = filter.process(&payload);
    assert_eq!(forward, payload.as_slice(), "all bytes must be forwarded");
    assert_eq!(
        events.len(),
        1,
        "the APC after the partial CSI must be extracted"
    );
    assert_eq!(command(&events[0]).command, b'G');
    assert_eq!(command(&events[0]).data, b"abc");
}

/// Through `Engine::feed` (the same stream filter as the PTY path) a
/// graphics command is handled and answered, and the text around it is
/// parsed as usual.
#[test]
fn engine_feed_handles_a_command() {
    let mut engine = nora::engine::Engine::new(25, 80);
    engine.feed(b"Hello");
    // `/wAA/w==` is one red RGBA pixel.
    engine.feed(&apc("Ga=t,i=7,s=1,v=1;/wAA/w=="));
    engine.feed(b" world");
    assert!(engine.graphics().image(7).is_some(), "the image was stored");
    assert_eq!(engine.replies(), vec!["\x1b_Gi=7;OK\x1b\\".to_string()]);
    let snap = engine.snapshot();
    let text: String = snap.cells[..11].iter().map(|c| c.ch).collect();
    assert_eq!(text, "Hello world", "the APC left the text alone");
}

/// The screen clears that remove images are reported at their place in the
/// stream; a private-mode or other erase is not.
#[test]
fn clear_signals_and_positions() {
    let mut filter = GraphicsFilter::new();
    let bytes = b"ab\x1b[2Jcd\x1b[3J\x1b[?2J\x1b[Jef\x1bc";
    let (forward, signals) = filter.process(bytes);
    assert_eq!(forward, bytes.as_slice());
    assert_eq!(
        signals,
        vec![
            (6, GraphicsSignal::ClearScreen),
            (12, GraphicsSignal::ClearScrollback),
            (bytes.len(), GraphicsSignal::Reset),
        ]
    );
    let (_, signals) = filter.process(&apc("Ga=d"));
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].0, apc("Ga=d").len(), "a command ends at its ST");
}

/// vte ends an APC at any ESC: an interrupted transfer (no ST) does not
/// swallow the sequences after it, at any chunk boundary.
#[test]
fn an_esc_ends_an_unterminated_apc() {
    let mut stream = b"\x1b_Ga=t,i=5,m=1;AAAA".to_vec();
    let clear = stream.len() + 4;
    stream.extend_from_slice(b"\x1b[2J");
    let reset = stream.len() + 2;
    stream.extend_from_slice(b"\x1bc");
    stream.extend_from_slice(b"\x1b_Gx\x1b\x1b_Gy");
    let query = apc("Ga=q,i=1;AAAA");
    stream.extend_from_slice(&query);
    let expected = vec![
        (clear, GraphicsSignal::ClearScreen),
        (reset, GraphicsSignal::Reset),
        (
            stream.len(),
            GraphicsSignal::Command(GraphicsEvent {
                command: b'G',
                keys: vec![("a".into(), "q".into()), ("i".into(), "1".into())],
                data: vec![0, 0, 0],
            }),
        ),
    ];
    // Every split into two chunks, then one byte at a time.
    let mut splits: Vec<Vec<&[u8]>> = (0..=stream.len())
        .map(|at| vec![&stream[..at], &stream[at..]])
        .collect();
    splits.push(stream.chunks(1).collect());
    for chunks in splits {
        let mut filter = GraphicsFilter::new();
        let mut signals = Vec::new();
        let mut start = 0;
        for chunk in &chunks {
            let (forward, found) = filter.process(chunk);
            assert_eq!(forward, *chunk);
            signals.extend(found.into_iter().map(|(at, s)| (start + at, s)));
            start += chunk.len();
        }
        assert_eq!(
            signals,
            expected,
            "chunks {:?}",
            chunks.iter().map(|c| c.len()).collect::<Vec<_>>()
        );
    }
}

/// vte 0.15 ends an APC only at CAN, SUB or ESC: an 8-bit ST (0x9C) is
/// data, so it neither completes a command nor ends the frame.
#[test]
fn an_8bit_st_does_not_end_an_apc() {
    let mut filter = GraphicsFilter::new();
    let (_, signals) = filter.process(b"\x1b_Ga=q,i=1;AAAA\x9c\x1b\\");
    assert!(signals.is_empty(), "the frame holds 0x9C: {signals:?}");
    // Text after a 0x9C is still inside the frame (vte drops it).
    let (_, signals) = filter.process(b"\x1b_Gx\x9cGa=q,i=1;AAAA\x1b\\");
    assert!(signals.is_empty(), "{signals:?}");
    let (_, signals) = filter.process(&apc("Ga=q,i=1;AAAA"));
    assert_eq!(signals.len(), 1, "the next frame is read again");
}
