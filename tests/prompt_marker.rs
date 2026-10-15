//! Synthetic OSC 133 prompt-marker tests (N35a).
//!
//! These tests exercise [`PromptMarkerFilter`], [`MarkerQueue`], and
//! [`MarkerStore`] directly (not through `Engine`), verifying that the
//! filter detects OSC 133 prompt markers (`OSC 133 ; <kind> ST`) in
//! parallel with the vte parser, suppresses well-formed marker frames,
//! forwards non-133 and malformed OSC frames unchanged, and that the
//! store is 1-based, bounded, and byte-accounted.

use nora::prompt_marker::{
    MarkerAtLine, MarkerKind, MarkerQueue, MarkerStore, PromptMarkerEvent, PromptMarkerFilter,
};

/// Wrap an OSC data string in a full OSC frame: `ESC ] <data> ESC \`.
fn osc(data: &str) -> Vec<u8> {
    let mut out = vec![0x1B, 0x5D]; // ESC ]
    out.extend_from_slice(data.as_bytes());
    out.push(0x1B); // ESC
    out.push(0x5C); // \ (ST terminator)
    out
}

/// A single prompt-marker event.
fn ev(kind: MarkerKind) -> PromptMarkerEvent {
    PromptMarkerEvent { kind }
}

/// A marker paired with a grid line (for queue/store tests).
fn entry(kind: MarkerKind) -> MarkerAtLine {
    MarkerAtLine {
        line: 0,
        scrolled: 0,
        pulled: 0,
        event: ev(kind),
    }
}

// ---------------------------------------------------------------------------
// Filter: well-formed OSC 133 markers (A/B/C/D)
// ---------------------------------------------------------------------------

/// Feed `OSC 133 ; A ST`; assert one `A` event and the frame is
/// suppressed from the forwarded bytes.
#[test]
fn filter_marker_a() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("133 ; A");
    let (forward, events) = filter.process(&bytes);
    assert!(forward.is_empty(), "well-formed marker frame is suppressed");
    assert_eq!(events, vec![(0, ev(MarkerKind::A))]);
}

/// Feed `OSC 133 ; B ST`; assert one `B` event and the frame is
/// suppressed.
#[test]
fn filter_marker_b() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("133 ; B");
    let (forward, events) = filter.process(&bytes);
    assert!(forward.is_empty());
    assert_eq!(events, vec![(0, ev(MarkerKind::B))]);
}

/// Feed `OSC 133 ; C ST`; assert one `C` event and the frame is
/// suppressed.
#[test]
fn filter_marker_c() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("133 ; C");
    let (forward, events) = filter.process(&bytes);
    assert!(forward.is_empty());
    assert_eq!(events, vec![(0, ev(MarkerKind::C))]);
}

/// Feed `OSC 133 ; D ST`; assert one `D` event and the frame is
/// suppressed.
#[test]
fn filter_marker_d() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("133 ; D");
    let (forward, events) = filter.process(&bytes);
    assert!(forward.is_empty());
    assert_eq!(events, vec![(0, ev(MarkerKind::D))]);
}

/// Feed a marker with no spaces around the `;` (`133;A`); assert it is
/// still detected (a single optional space is tolerated).
#[test]
fn filter_marker_no_spaces() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("133;A");
    let (forward, events) = filter.process(&bytes);
    assert!(forward.is_empty());
    assert_eq!(events, vec![(0, ev(MarkerKind::A))]);
}

/// Feed a marker terminated by the single-byte ST (`0x9C`); assert it is
/// detected and suppressed.
#[test]
fn filter_marker_0x9c_terminator() {
    let mut filter = PromptMarkerFilter::new();
    let mut bytes = vec![0x1B, 0x5D]; // ESC ]
    bytes.extend_from_slice(b"133 ; A");
    bytes.push(0x9C); // ST (single-byte form)
    let (forward, events) = filter.process(&bytes);
    assert!(forward.is_empty());
    assert_eq!(events, vec![(0, ev(MarkerKind::A))]);
}

/// Feed a marker terminated by BEL (`0x07`); assert it is detected and
/// suppressed.
#[test]
fn filter_marker_bel_terminator() {
    let mut filter = PromptMarkerFilter::new();
    let mut bytes = vec![0x1B, 0x5D]; // ESC ]
    bytes.extend_from_slice(b"133 ; A");
    bytes.push(0x07); // BEL terminator
    let (forward, events) = filter.process(&bytes);
    assert!(forward.is_empty());
    assert_eq!(events, vec![(0, ev(MarkerKind::A))]);
}

/// Feed several markers interleaved with ordinary text; assert the markers
/// are detected in order and the ordinary text is forwarded unchanged.
#[test]
fn filter_markers_interleaved_with_text() {
    let mut filter = PromptMarkerFilter::new();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"hello ");
    bytes.extend_from_slice(&osc("133 ; A"));
    bytes.extend_from_slice(b"world ");
    bytes.extend_from_slice(&osc("133 ; B"));
    bytes.extend_from_slice(b"done");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, b"hello world done".to_vec());
    assert_eq!(
        events,
        vec![(6, ev(MarkerKind::A)), (12, ev(MarkerKind::B))]
    );
}

// ---------------------------------------------------------------------------
// Filter: non-133 OSC frames are forwarded unchanged
// ---------------------------------------------------------------------------

/// Feed a non-133 OSC frame (`OSC 0 ; title ST`); assert it is forwarded
/// unchanged with no event.
#[test]
fn filter_non_133_osc_forwarded() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("0 ; title");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes, "non-133 OSC frame forwarded unchanged");
    assert!(events.is_empty(), "no event for a non-133 OSC frame");
}

/// Feed a non-133 OSC frame with a different command number (`OSC 7 ; ...`);
/// assert it is forwarded unchanged with no event.
#[test]
fn filter_non_133_command_forwarded() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("7 ; saved-cursor");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes);
    assert!(events.is_empty());
}

// ---------------------------------------------------------------------------
// Filter: no OSC frames — all bytes forwarded, no events
// ---------------------------------------------------------------------------

/// Feed bytes with no OSC frames; assert all bytes are forwarded and no
/// events are produced.
#[test]
fn filter_no_osc_frames() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = b"hello world\r\nplain text";
    let (forward, events) = filter.process(bytes);
    assert_eq!(forward, bytes, "all bytes forwarded");
    assert!(events.is_empty(), "no events");
}

/// Feed a CSI sequence (an ESC not followed by `]`); assert all bytes are
/// forwarded and no events are produced.
#[test]
fn filter_csi_sequence_forwarded() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = b"\x1b[1;31mHello\x0d\x0a";
    let (forward, events) = filter.process(bytes);
    assert_eq!(forward, bytes, "CSI sequence forwarded unchanged");
    assert!(events.is_empty(), "no events for a CSI sequence");
}

// ---------------------------------------------------------------------------
// Filter: malformed OSC 133 frames are forwarded unchanged
// ---------------------------------------------------------------------------

/// Feed `OSC 133 ; X ST` (unknown kind); assert it is forwarded unchanged
/// with no event.
#[test]
fn filter_malformed_unknown_kind() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("133 ; X");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes, "unknown kind forwarded unchanged");
    assert!(events.is_empty(), "no event for unknown kind");
}

/// Feed `OSC 133 ST` (no kind); assert it is forwarded unchanged with no
/// event.
#[test]
fn filter_malformed_no_kind() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("133");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes, "no kind forwarded unchanged");
    assert!(events.is_empty(), "no event for missing kind");
}

/// Feed `OSC 133 ; AB ST` (multi-char kind); assert it is forwarded
/// unchanged with no event.
#[test]
fn filter_malformed_multi_char_kind() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("133 ; AB");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes);
    assert!(events.is_empty());
}

/// Feed `OSC 133  ;  A ST` (extra spaces); assert it is forwarded
/// unchanged with no event (only a single optional space is tolerated).
#[test]
fn filter_malformed_extra_spaces() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("133  ;  A");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes);
    assert!(events.is_empty());
}

// ---------------------------------------------------------------------------
// Filter: chunking reassembles a marker across process calls
// ---------------------------------------------------------------------------

/// Feed the same marker split across multiple process calls (byte-by-byte
/// and irregular chunks); verify it reassembles to the same event.
#[test]
fn filter_chunking_reassembles() {
    let bytes = osc("133 ; A");

    // Byte-by-byte.
    let mut filter = PromptMarkerFilter::new();
    let mut all_events = Vec::new();
    let mut all_forward = Vec::new();
    for &byte in &bytes {
        let (forward, events) = filter.process(&[byte]);
        all_forward.extend(forward);
        all_events.extend(events);
    }
    assert!(all_forward.is_empty(), "byte-by-byte: frame suppressed");
    assert_eq!(
        all_events,
        vec![(0, ev(MarkerKind::A))],
        "byte-by-byte reassembly"
    );

    // Irregular chunks (split in the middle of the data).
    let mut filter = PromptMarkerFilter::new();
    let mut all_events = Vec::new();
    let split = bytes.len() / 2;
    let (first, second) = bytes.split_at(split);
    all_events.extend(filter.process(first).1);
    all_events.extend(filter.process(second).1);
    assert_eq!(
        all_events,
        vec![(0, ev(MarkerKind::A))],
        "irregular-chunk reassembly"
    );
}

// ---------------------------------------------------------------------------
// MarkerQueue: byte-bounded
// ---------------------------------------------------------------------------

/// Push more events than the byte cap; verify the queue evicts the oldest
/// event(s) and stays within the cap, with the newest event surviving.
#[test]
fn queue_oversize_evicts_oldest() {
    let mut queue = MarkerQueue::new();
    // Each event costs 1 byte; push more than the 1 MiB cap.
    let n = 1024 * 1024 + 10;
    let mut last_kind = MarkerKind::A;
    for i in 0..n {
        last_kind = match i % 4 {
            0 => MarkerKind::A,
            1 => MarkerKind::B,
            2 => MarkerKind::C,
            _ => MarkerKind::D,
        };
        queue.push(entry(last_kind));
    }
    assert!(
        queue.total_bytes() <= 1024 * 1024,
        "queue must stay within cap"
    );
    assert!(queue.len() < n, "oldest event(s) must have been evicted");
    // Drain and verify the newest event survives (the last kind pushed).
    let drained = queue.drain();
    assert!(!drained.is_empty());
    assert_eq!(
        drained.last().unwrap().event.kind,
        last_kind,
        "newest event must survive"
    );
}

/// Drain an empty queue; verify it returns an empty vec.
#[test]
fn queue_drain_empty() {
    let mut queue = MarkerQueue::new();
    assert!(queue.is_empty());
    assert_eq!(queue.len(), 0);
    assert_eq!(queue.total_bytes(), 0);
    let drained = queue.drain();
    assert!(drained.is_empty());
}

/// Push a few events, drain, and verify order (oldest first) and that the
/// queue is empty afterward.
#[test]
fn queue_push_and_drain_order() {
    let mut queue = MarkerQueue::new();
    queue.push(entry(MarkerKind::A));
    queue.push(entry(MarkerKind::B));
    queue.push(entry(MarkerKind::C));
    assert_eq!(queue.len(), 3);
    assert_eq!(queue.total_bytes(), 3);
    let drained = queue.drain();
    assert_eq!(
        drained,
        vec![
            entry(MarkerKind::A),
            entry(MarkerKind::B),
            entry(MarkerKind::C)
        ]
    );
    assert!(queue.is_empty());
    assert_eq!(queue.total_bytes(), 0);
}

// ---------------------------------------------------------------------------
// MarkerStore: 1-based, bounded, byte-accounted
// ---------------------------------------------------------------------------

/// Insert two markers; verify IDs are 1-based (1, then 2) and queryable.
#[test]
fn store_auto_assign_1_based() {
    let mut store = MarkerStore::new();
    let id = store.insert(entry(MarkerKind::A)).expect("insert");
    assert_eq!(id, 1, "IDs are 1-based");
    assert_eq!(store.len(), 1);
    let marker = store.query(id).expect("query");
    assert_eq!(marker.event.kind, MarkerKind::A);
    let id2 = store.insert(entry(MarkerKind::B)).expect("insert");
    assert_eq!(id2, 2);
    assert!(store.query(2).is_some());
}

/// Insert up to the cap; verify the cap is enforced (the next insert
/// returns None) and the store is bounded.
#[test]
fn store_bounded_by_max_markers() {
    let mut store = MarkerStore::new();
    let cap = nora::prompt_marker::MAX_MARKERS;
    for _ in 0..cap {
        assert!(store.insert(entry(MarkerKind::A)).is_some());
    }
    assert_eq!(store.len(), cap);
    // One more exceeds the cap.
    assert!(store.insert(entry(MarkerKind::B)).is_none());
    assert_eq!(store.len(), cap);
}

/// Insert, delete, and clear; verify byte accounting mirrors the ImageStore
/// style (total_bytes tracks the stored markers).
#[test]
fn store_byte_accounted() {
    let mut store = MarkerStore::new();
    store.insert(entry(MarkerKind::A)).expect("insert");
    store.insert(entry(MarkerKind::B)).expect("insert");
    assert_eq!(store.len(), 2);
    assert_eq!(store.total_bytes(), 2);
    // Delete one; the byte cost is subtracted.
    let deleted = store.delete(1);
    assert!(deleted.is_some());
    assert_eq!(deleted.unwrap().event.kind, MarkerKind::A);
    assert_eq!(store.len(), 1);
    assert_eq!(store.total_bytes(), 1);
    // Clear the rest.
    let n = store.clear();
    assert_eq!(n, 1);
    assert_eq!(store.len(), 0);
    assert_eq!(store.total_bytes(), 0);
}

/// Delete a nonexistent ID; verify it returns None and the store is
/// unchanged.
#[test]
fn store_delete_missing() {
    let mut store = MarkerStore::new();
    store.insert(entry(MarkerKind::A)).expect("insert");
    assert!(store.delete(99).is_none());
    assert_eq!(store.len(), 1);
    assert_eq!(store.total_bytes(), 1);
}

/// Query a nonexistent ID; verify it returns None.
#[test]
fn store_query_missing() {
    let store = MarkerStore::new();
    assert!(store.query(1).is_none());
    assert!(store.is_empty());
}

// ---------------------------------------------------------------------------
// MarkerAtLine::current_line (N35b)
// ---------------------------------------------------------------------------

/// A marker stored when `total_scrolled = 0` and `total_pulled = 0`
/// stays put until the grid scrolls or resizes.
#[test]
fn current_line_no_scroll() {
    let m = MarkerAtLine {
        line: 0,
        scrolled: 0,
        pulled: 0,
        event: ev(MarkerKind::A),
    };
    assert_eq!(m.current_line(0, 0), Some(0));
}

/// After 3 lines are added to the bottom (full-screen scrolls) since the
/// marker was stored, it is 3 lines above where it was.
#[test]
fn current_line_after_scroll() {
    let m = MarkerAtLine {
        line: 0,
        scrolled: 0,
        pulled: 0,
        event: ev(MarkerKind::A),
    };
    assert_eq!(m.current_line(3, 0), Some(-3));
}

/// A marker stored above the bottom (`line = -2`) at `total_scrolled = 10`
/// is at `Line(-2 - (15 - 10)) = Line(-7)` after 5 more lines scroll.
#[test]
fn current_line_above_bottom() {
    let m = MarkerAtLine {
        line: -2,
        scrolled: 10,
        pulled: 0,
        event: ev(MarkerKind::A),
    };
    assert_eq!(m.current_line(15, 0), Some(-7));
}

/// A vertical resize that pulls 5 lines from history (no scroll) moves the
/// marker down by 5: `line - 0 + (5 - 0)`.
#[test]
fn current_line_after_resize_pull() {
    let m = MarkerAtLine {
        line: 23,
        scrolled: 8,
        pulled: 0,
        event: ev(MarkerKind::A),
    };
    // 10 lines scrolled (total_scrolled 8 -> 18) and a resize pulled 5
    // lines from history (total_pulled 0 -> 5).
    assert_eq!(m.current_line(18, 5), Some(18));
}

/// `current_line` saturates: a marker stored at `total_scrolled = 10`
/// queried at `total_scrolled = 5` (a clock that went backwards, which
/// should not happen) does not underflow.
#[test]
fn current_line_saturates() {
    let m = MarkerAtLine {
        line: 0,
        scrolled: 10,
        pulled: 0,
        event: ev(MarkerKind::A),
    };
    assert_eq!(m.current_line(5, 0), Some(0));
}

// ---------------------------------------------------------------------------
// MarkerStore::iter (N35b)
// ---------------------------------------------------------------------------

/// `iter` yields every stored marker as `(id, &MarkerAtLine)`.
#[test]
fn store_iter_yields_all() {
    let mut store = MarkerStore::new();
    let a = store.insert(entry(MarkerKind::A)).expect("insert A");
    let b = store.insert(entry(MarkerKind::B)).expect("insert B");
    let mut seen: Vec<usize> = Vec::new();
    for (id, _) in store.iter() {
        seen.push(id);
    }
    seen.sort();
    assert_eq!(seen, vec![a, b]);
}
