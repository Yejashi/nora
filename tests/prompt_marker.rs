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
    ShellEvent,
};

/// Wrap an OSC data string in a full OSC frame: `ESC ] <data> ESC \`.
fn osc(data: &str) -> Vec<u8> {
    let mut out = vec![0x1B, 0x5D]; // ESC ]
    out.extend_from_slice(data.as_bytes());
    out.push(0x1B); // ESC
    out.push(0x5C); // \ (ST terminator)
    out
}

/// A single prompt-marker shell event (the filter's output type).
fn ev(kind: MarkerKind) -> ShellEvent {
    ShellEvent::Marker(PromptMarkerEvent { kind })
}

/// A bare marker event (the `MarkerAtLine.event` field type).
fn marker_event(kind: MarkerKind) -> PromptMarkerEvent {
    PromptMarkerEvent { kind }
}

/// A marker paired with a grid line (for queue/store tests).
fn entry(kind: MarkerKind) -> MarkerAtLine {
    MarkerAtLine {
        line: 0,
        scrolled: 0,
        pulled: 0,
        alt: false,
        alt_session: 0,
        event: marker_event(kind),
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

/// `0x9C` inside a frame is data, as in vte's UTF-8 mode (it is a common
/// UTF-8 continuation byte): the frame runs on to its real terminator.
#[test]
fn filter_0x9c_is_frame_data() {
    let mut filter = PromptMarkerFilter::new();
    let mut bytes = vec![0x1B, 0x5D]; // ESC ]
    bytes.extend_from_slice(b"133 ; A");
    bytes.push(0x9C);
    bytes.extend_from_slice(b"x\x07after");
    let (forward, events) = filter.process(&bytes);
    // `133 ; A\x9Cx` is not a well-formed marker, so the whole frame is
    // forwarded unchanged and nothing leaks out as text early.
    assert!(events.is_empty());
    assert_eq!(forward, bytes);
}

/// A command line with an emoji ending in 0x9C (😜 = F0 9F 98 9C) keeps
/// the marker and its command line; nothing shows as text.
#[test]
fn filter_cmdline_with_emoji() {
    let mut filter = PromptMarkerFilter::new();
    let (forward, events) = filter.process("\x1b]133;C;cmdline=echo 😜 hi\x07".as_bytes());
    assert!(forward.is_empty(), "forwarded {forward:?}");
    assert_eq!(
        events,
        vec![
            (0, ev(MarkerKind::C)),
            (0, ShellEvent::CommandLine("echo 😜 hi".to_string())),
        ]
    );
}

/// vte ends an OSC at any ESC, which starts the next sequence: a title
/// followed directly by `ESC [2J` is forwarded at once (with an explicit
/// ST) and the CSI after it is not held back.
#[test]
fn filter_esc_ends_frame_and_starts_next_sequence() {
    let mut filter = PromptMarkerFilter::new();
    let (forward, events) = filter.process(b"\x1b]0;t\x1b[2Jtext");
    assert!(events.is_empty());
    assert_eq!(forward, b"\x1b]0;t\x1b\\\x1b[2Jtext");

    // A marker ended by an ESC is detected and the next sequence flows.
    let (forward, events) = filter.process(b"\x1b]133;A\x1b[1mbold");
    assert_eq!(events, vec![(0, ev(MarkerKind::A))]);
    assert_eq!(forward, b"\x1b[1mbold");

    // An ESC that starts another OSC: both frames are seen.
    let (forward, events) = filter.process(b"\x1b]0;t\x1b]133;B\x07x");
    assert_eq!(events, vec![(7, ev(MarkerKind::B))]);
    assert_eq!(forward, b"\x1b]0;t\x1b\\x");
}

/// The ESC-terminated case works when the chunk splits right after the
/// ESC.
#[test]
fn filter_esc_terminator_across_chunks() {
    let mut filter = PromptMarkerFilter::new();
    let (forward, events) = filter.process(b"\x1b]133;A\x1b");
    assert!(forward.is_empty());
    assert!(events.is_empty());
    let (forward, events) = filter.process(b"[2J");
    assert_eq!(events, vec![(0, ev(MarkerKind::A))]);
    assert_eq!(forward, b"\x1b[2J");

    let (forward, _) = filter.process(b"\x1b]0;t\x1b");
    assert!(forward.is_empty());
    let (forward, _) = filter.process(b"\x1b]133;D\x07");
    assert_eq!(forward, b"\x1b]0;t\x1b\\");
}

/// CAN and SUB end the frame (vte dispatches it) and are then executed:
/// a marker ended by CAN is detected and the CAN byte is forwarded.
#[test]
fn filter_can_sub_end_frame_and_are_forwarded() {
    let mut filter = PromptMarkerFilter::new();
    let (forward, events) = filter.process(b"\x1b]133;A\x18after");
    assert_eq!(events, vec![(0, ev(MarkerKind::A))]);
    assert_eq!(forward, b"\x18after");

    let (forward, events) = filter.process(b"\x1b]0;t\x1aafter");
    assert!(events.is_empty());
    assert_eq!(forward, b"\x1b]0;t\x1aafter");
}

/// The C0 controls vte ignores inside an OSC are not frame data.
#[test]
fn filter_ignored_c0_inside_frame() {
    let mut filter = PromptMarkerFilter::new();
    let (forward, events) = filter.process(b"\x1b]133;\nA\x07");
    assert!(forward.is_empty());
    assert_eq!(events, vec![(0, ev(MarkerKind::A))]);
}

/// A doubled ESC restarts the escape, as in vte: `ESC ESC ] 133;A BEL`
/// detects the marker, and the first ESC (a no-op in vte) is not left
/// to pair with the bytes after the suppressed frame.
#[test]
fn filter_double_esc_before_osc() {
    let mut filter = PromptMarkerFilter::new();
    let (forward, events) = filter.process(b"\x1b\x1b]133;A\x07t");
    assert_eq!(forward, b"t");
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
// Filter: OSC 7 cwd frames (N35d)
// ---------------------------------------------------------------------------

/// Feed `OSC 7 ; file:///home/user ST`; assert one `Cwd` event with the
/// decoded path and the frame is suppressed.
#[test]
fn filter_cwd_file_uri() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("7;file:///home/user");
    let (forward, events) = filter.process(&bytes);
    assert!(forward.is_empty(), "well-formed cwd frame suppressed");
    assert_eq!(events, vec![(0, ShellEvent::Cwd("/home/user".to_string()))]);
}

/// Audit RV-OSC7: a `%` followed by a multi-byte character used to panic
/// the decoder (and with it the pane's PTY reader). The frame now decodes
/// with the malformed escape kept literally.
#[test]
fn filter_cwd_percent_before_non_ascii_does_not_panic() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("7;file:///%a\u{e9}/caf%C3%A9");
    let (forward, events) = filter.process(&bytes);
    assert!(forward.is_empty());
    assert_eq!(
        events,
        vec![(0, ShellEvent::Cwd("/%a\u{e9}/caf\u{e9}".to_string()))]
    );
}

/// Feed `OSC 7 ; /relative/path ST` (no `file://` prefix); assert one
/// `Cwd` event with the raw path.
#[test]
fn filter_cwd_relative_path() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("7;/relative/path");
    let (forward, events) = filter.process(&bytes);
    assert!(forward.is_empty());
    assert_eq!(
        events,
        vec![(0, ShellEvent::Cwd("/relative/path".to_string()))]
    );
}

/// Feed `OSC 7 ; http://example.com ST` (non-file scheme); assert no
/// event and the frame is forwarded unchanged.
#[test]
fn filter_cwd_non_file_scheme_forwarded() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("7;http://example.com");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes, "non-file cwd frame forwarded unchanged");
    assert!(events.is_empty(), "no event for a non-file cwd");
}

/// Feed `OSC 7 ST` (no path); assert no event and the frame is forwarded
/// unchanged.
#[test]
fn filter_cwd_no_path() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("7");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes);
    assert!(events.is_empty());
}

// ---------------------------------------------------------------------------
// Filter: OSC 8 hyperlink frames (N35d)
// ---------------------------------------------------------------------------

/// Feed `OSC 8 ; ; https://example.com ST`; assert one `Hyperlink` event
/// with the URI and the frame is forwarded (display effect).
#[test]
fn filter_hyperlink_basic() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("8;;https://example.com");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes, "hyperlink frame forwarded");
    assert_eq!(
        events,
        vec![(0, ShellEvent::Hyperlink("https://example.com".to_string()))]
    );
}

/// Feed `OSC 8 ; id=123 ; https://example.com ST`; assert one `Hyperlink`
/// event (params are ignored, URI is the last part) and the frame is
/// forwarded.
#[test]
fn filter_hyperlink_with_params() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("8;id=123;https://example.com");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes);
    assert_eq!(
        events,
        vec![(0, ShellEvent::Hyperlink("https://example.com".to_string()))]
    );
}

/// Feed `OSC 8 ;; ST` (empty URI); assert no event and the frame is
/// forwarded unchanged.
#[test]
fn filter_hyperlink_empty_uri() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("8;;");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes);
    assert!(events.is_empty());
}

/// Feed `OSC 8 ; id=123 ; ST` (params but no URI); assert no event and the
/// frame is forwarded unchanged.
#[test]
fn filter_hyperlink_no_uri_part() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("8;id=123;");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes);
    assert!(events.is_empty());
}

/// Feed a hyperlink with a `file://` URI; assert one `Hyperlink` event and
/// the frame is forwarded.
#[test]
fn filter_hyperlink_file_uri() {
    let mut filter = PromptMarkerFilter::new();
    let bytes = osc("8;;file:///home/user/doc.txt");
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes);
    assert_eq!(
        events,
        vec![(
            0,
            ShellEvent::Hyperlink("file:///home/user/doc.txt".to_string())
        )]
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
    // Push more than fits in the 1 MiB cap.
    let n = 1024 * 1024 / entry(MarkerKind::A).byte_cost() + 10;
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
    assert_eq!(queue.total_bytes(), 3 * entry(MarkerKind::A).byte_cost());
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

/// Insert past the cap: the store stays bounded and evicts the oldest
/// marker, so the newest prompt is always kept (it used to refuse new
/// markers once full, leaving navigation on stale prompts).
#[test]
fn store_bounded_by_max_markers() {
    let mut store = MarkerStore::new();
    let cap = nora::prompt_marker::MAX_MARKERS;
    for _ in 0..cap {
        assert!(store.insert(entry(MarkerKind::A)).is_some());
    }
    assert_eq!(store.len(), cap);
    let bytes = store.total_bytes();
    let newest = store
        .insert(entry(MarkerKind::B))
        .expect("insert at the cap");
    assert_eq!(store.len(), cap);
    assert_eq!(store.total_bytes(), bytes, "one evicted, one added");
    assert!(store.query(1).is_none(), "the oldest was evicted");
    assert_eq!(
        store.query(newest).map(|m| m.event.kind),
        Some(MarkerKind::B)
    );
}

/// Insert, delete, and clear; verify byte accounting mirrors the ImageStore
/// style (total_bytes tracks the stored markers).
#[test]
fn store_byte_accounted() {
    let mut store = MarkerStore::new();
    store.insert(entry(MarkerKind::A)).expect("insert");
    store.insert(entry(MarkerKind::B)).expect("insert");
    let cost = entry(MarkerKind::A).byte_cost();
    assert_eq!(store.len(), 2);
    assert_eq!(store.total_bytes(), 2 * cost);
    // Delete one; the byte cost is subtracted.
    let deleted = store.delete(1);
    assert!(deleted.is_some());
    assert_eq!(deleted.unwrap().event.kind, MarkerKind::A);
    assert_eq!(store.len(), 1);
    assert_eq!(store.total_bytes(), cost);
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
    assert_eq!(store.total_bytes(), entry(MarkerKind::A).byte_cost());
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
        alt: false,
        alt_session: 0,
        event: marker_event(MarkerKind::A),
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
        alt: false,
        alt_session: 0,
        event: marker_event(MarkerKind::A),
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
        alt: false,
        alt_session: 0,
        event: marker_event(MarkerKind::A),
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
        alt: false,
        alt_session: 0,
        event: marker_event(MarkerKind::A),
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
        alt: false,
        alt_session: 0,
        event: marker_event(MarkerKind::A),
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

// ---------------------------------------------------------------------------
// Filter terminators through the engine (vte parity)
// ---------------------------------------------------------------------------

/// The text of viewport row `row`, trailing blanks trimmed.
fn engine_row(engine: &mut nora::engine::Engine, row: usize) -> String {
    let snapshot = engine.snapshot();
    let text: String = (0..snapshot.cols)
        .map(|col| snapshot.cells[row * snapshot.cols + col].ch)
        .collect();
    text.trim_end().to_string()
}

/// A marker whose command line holds a 0x9C byte is consumed whole: no
/// part of it shows, and the marker is queued.
#[test]
fn engine_cmdline_emoji_marker_is_not_shown() {
    let mut engine = nora::engine::Engine::new(4, 40);
    engine.feed("\x1b]133;C;cmdline=echo 😜 hi\x07ok".as_bytes());
    assert_eq!(engine_row(&mut engine, 0), "ok");
    assert_eq!(engine.drain_markers().len(), 1);
}

/// An OSC ended by the ESC of the next sequence does not hold back the
/// output after it.
#[test]
fn engine_osc_ended_by_esc_does_not_hold_output() {
    let mut engine = nora::engine::Engine::new(4, 40);
    engine.feed(b"junk\x1b]0;t\x1b[2J\x1b[Hshown");
    assert_eq!(engine_row(&mut engine, 0), "shown");
}

// ---------------------------------------------------------------------------
// Queue and store memory bounds
// ---------------------------------------------------------------------------

fn link(uri: &str) -> nora::prompt_marker::HyperlinkAtLine {
    nora::prompt_marker::HyperlinkAtLine {
        line: 0,
        col: 0,
        scrolled: 0,
        pulled: 0,
        alt: false,
        alt_session: 0,
        uri: uri.to_string(),
    }
}

/// A marker entry costs at least its own size, so the 1 MiB budget bounds
/// the queue's real memory (it used to cost 1 byte, allowing ~1M entries).
#[test]
fn marker_queue_costs_entry_size() {
    let cost = entry(MarkerKind::A).byte_cost();
    assert!(cost >= std::mem::size_of::<MarkerAtLine>());
    let mut queue = MarkerQueue::new();
    for _ in 0..200_000 {
        queue.push(entry(MarkerKind::A));
    }
    assert!(queue.total_bytes() <= 1024 * 1024);
    assert!(queue.len() * std::mem::size_of::<MarkerAtLine>() <= 1024 * 1024);
}

/// A hyperlink entry costs its size plus the URI allocation, so a flood
/// of short links stays within 1 MiB of real memory.
#[test]
fn hyperlink_queue_costs_entry_size_and_uri() {
    let one = link("x");
    assert!(one.byte_cost() > std::mem::size_of::<nora::prompt_marker::HyperlinkAtLine>());
    let mut queue = nora::prompt_marker::HyperlinkQueue::new();
    for _ in 0..200_000 {
        queue.push(link("x"));
    }
    assert!(queue.total_bytes() <= 1024 * 1024);
    assert!(queue.len() * one.byte_cost() <= 1024 * 1024);
}

/// The hyperlink store evicts the oldest link at the cap instead of
/// refusing the newest.
#[test]
fn hyperlink_store_evicts_oldest_at_cap() {
    let mut store = nora::prompt_marker::HyperlinkStore::new();
    let cap = nora::prompt_marker::MAX_MARKERS;
    for i in 0..cap {
        store.insert(link(&format!("https://e/{i}")));
    }
    let bytes = store.total_bytes();
    let newest = store.insert(link("https://e/new")).expect("insert at cap");
    assert_eq!(store.len(), cap);
    assert!(store.query(1).is_none(), "the oldest was evicted");
    assert_eq!(
        store.query(newest).map(|l| l.uri.as_str()),
        Some("https://e/new")
    );
    assert!(store.total_bytes() <= bytes + link("https://e/new").byte_cost());
}

/// A stream of OSC 8 links through the engine keeps the undrained queue
/// within its budget.
#[test]
fn engine_hyperlink_flood_is_bounded() {
    let mut engine = nora::engine::Engine::new(4, 40);
    let mut chunk = Vec::new();
    for _ in 0..1000 {
        chunk.extend_from_slice(b"\x1b]8;;https://e/x\x07l\x1b]8;;\x07");
    }
    for _ in 0..100 {
        engine.feed(&chunk);
    }
    let handle = engine.hyperlink_handle();
    let queue = handle.lock().unwrap();
    assert!(queue.total_bytes() <= 1024 * 1024);
    assert!(
        queue.len() * std::mem::size_of::<nora::prompt_marker::HyperlinkAtLine>() <= 1024 * 1024
    );
}
