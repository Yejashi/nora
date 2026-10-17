//! N17a regression suite for the pure paste policy
//! (`nora::input::paste::validate_paste`) and the bounded paste stream
//! (`nora::input::paste::PasteStream`).
//!
//! Every test is headless: no winit, no PTY, no display, no clocks.
//!
//! Pinned policy (verified against `src/input/paste.rs`):
//! - unbracketed newlines become Return: `\r\n` → `\r`, `\n` → `\r`,
//!   `\r` unchanged;
//! - bracketed newlines become `\n` (`\r\n` and standalone `\r`), and
//!   ESC / U+009B are removed so the payload cannot end the paste early;
//! - bracketed paste wraps the normalized bytes in
//!   `CSI 200 ~ ... CSI 201 ~` (6 bytes per marker);
//! - the size check compares the final bytes (including markers when
//!   bracketed) against `MAX_PASTE_SIZE` (1 MiB): strictly larger is
//!   `TooLarge`, exactly equal is `Ok`.

use nora::input::paste::{MAX_PASTE_SIZE, PasteStream, PasteValidation, validate_paste};

const START: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";

/// `validate_paste` with `bracketed=false`: no markers, size check.
#[test]
fn ordinary_paste_has_no_markers() {
    let v = validate_paste("hello", false);
    assert_eq!(
        v,
        PasteValidation::Ok {
            bytes: b"hello".to_vec(),
            bracketed: false,
        }
    );
}

/// `validate_paste` with `bracketed=true`: markers added, size includes
/// the markers.
#[test]
fn bracketed_paste_wraps_markers() {
    let v = validate_paste("hello", true);
    let expected = [START, b"hello".as_ref(), END].concat();
    assert_eq!(
        v,
        PasteValidation::Ok {
            bytes: expected,
            bracketed: true,
        }
    );
}

/// The bracket markers are exactly `CSI 200 ~` / `CSI 201 ~`, and an
/// empty bracketed paste is the two markers alone.
#[test]
fn bracketed_markers_are_exact_bytes() {
    assert_eq!(START, &b"\x1b[200~"[..]);
    assert_eq!(END, &b"\x1b[201~"[..]);
    let v = validate_paste("", true);
    assert_eq!(
        v,
        PasteValidation::Ok {
            bytes: [START, END].concat(),
            bracketed: true,
        }
    );
}

/// Unbracketed newline normalization: `\r\n` → `\r`, `\n` → `\r`, `\r`
/// unchanged — what the Return key sends, as in xterm and alacritty, so a
/// raw-mode application sees Enter rather than Ctrl+J.
#[test]
fn newline_normalization_crlf_cr_lf() {
    let v = validate_paste("a\r\nb\rc\nd", false);
    assert_eq!(
        v,
        PasteValidation::Ok {
            bytes: b"a\rb\rc\rd".to_vec(),
            bracketed: false,
        }
    );
}

/// A bracketed payload cannot end the paste early: an embedded
/// `CSI 201 ~` (7-bit ESC form or C1 U+009B form) loses its introducer, so
/// the only end marker is the one `validate_paste` appends.
#[test]
fn bracketed_payload_cannot_end_the_paste() {
    let v = validate_paste("ls\x1b[201~rm -rf ~\n\u{9b}201~x\x1b", true);
    let bytes = match v {
        PasteValidation::Ok { bytes, .. } => bytes,
        other => panic!("expected Ok, got {other:?}"),
    };
    assert_eq!(
        bytes,
        [START, b"ls[201~rm -rf ~\n201~x".as_ref(), END].concat()
    );
    let ends = bytes.windows(END.len()).filter(|w| *w == END).count();
    assert_eq!(ends, 1, "only the closing marker");
    assert!(bytes.ends_with(END));
    let escs = bytes.iter().filter(|&&b| b == 0x1b).count();
    assert_eq!(escs, 2, "only the two markers' ESCs");
    // Unbracketed text is sent as typed (there is no marker to protect).
    assert_eq!(
        validate_paste("a\x1bb", false),
        PasteValidation::Ok {
            bytes: b"a\x1bb".to_vec(),
            bracketed: false,
        }
    );
}

/// Normalization happens BEFORE bracketing: the bracketed content
/// carries the normalized newlines.
#[test]
fn newline_normalization_happens_before_bracketing() {
    let v = validate_paste("a\r\nb", true);
    let expected = [START, b"a\nb".as_ref(), END].concat();
    assert_eq!(
        v,
        PasteValidation::Ok {
            bytes: expected,
            bracketed: true,
        }
    );
}

/// An oversized paste returns `TooLarge` with the actual size and the
/// configured maximum.
#[test]
fn oversized_paste_is_too_large() {
    let big = "x".repeat(MAX_PASTE_SIZE + 1);
    let v = validate_paste(&big, false);
    assert_eq!(
        v,
        PasteValidation::TooLarge {
            size: MAX_PASTE_SIZE + 1,
            max: MAX_PASTE_SIZE,
        }
    );
}

/// A paste of exactly `MAX_PASTE_SIZE` bytes is accepted.
#[test]
fn exactly_max_size_is_ok() {
    let exact = "x".repeat(MAX_PASTE_SIZE);
    let v = validate_paste(&exact, false);
    match v {
        PasteValidation::Ok {
            bytes,
            bracketed: false,
        } => assert_eq!(bytes.len(), MAX_PASTE_SIZE),
        other => panic!("expected Ok, got {other:?}"),
    }
}

/// `MAX_PASTE_SIZE + 1` bytes is rejected.
#[test]
fn max_size_plus_one_is_too_large() {
    let over = "x".repeat(MAX_PASTE_SIZE + 1);
    let v = validate_paste(&over, false);
    assert_eq!(
        v,
        PasteValidation::TooLarge {
            size: MAX_PASTE_SIZE + 1,
            max: MAX_PASTE_SIZE,
        }
    );
}

/// In bracketed mode the 12 marker bytes count against the cap: a
/// payload of `MAX - 12` is accepted (final size exactly `MAX`), and
/// `MAX - 11` is rejected (final size `MAX + 1`).
#[test]
fn bracketed_size_includes_markers() {
    let fits = "x".repeat(MAX_PASTE_SIZE - 12);
    let v = validate_paste(&fits, true);
    match v {
        PasteValidation::Ok {
            bytes,
            bracketed: true,
        } => {
            assert_eq!(bytes.len(), MAX_PASTE_SIZE);
            assert!(bytes.starts_with(START));
            assert!(bytes.ends_with(END));
        }
        other => panic!("expected Ok, got {other:?}"),
    }
    let over = "x".repeat(MAX_PASTE_SIZE - 11);
    assert_eq!(
        validate_paste(&over, true),
        PasteValidation::TooLarge {
            size: MAX_PASTE_SIZE + 1,
            max: MAX_PASTE_SIZE,
        },
    );
}

/// Normalization happens before the size check: 1 MiB of CRLF pairs is
/// 2 MiB of input but normalizes to exactly `MAX_PASTE_SIZE` and is
/// accepted.
#[test]
fn normalization_can_shrink_below_max() {
    let big = "a\r\n".repeat(512 * 1024);
    let v = validate_paste(&big, false);
    match v {
        PasteValidation::Ok { bytes, .. } => assert_eq!(bytes.len(), MAX_PASTE_SIZE),
        other => panic!("expected Ok, got {other:?}"),
    }
}

/// `PasteStream` basics: create, drain, done.
#[test]
fn paste_stream_basic() {
    let mut s = PasteStream::new(b"hello".to_vec());
    assert!(!s.is_done());
    assert_eq!(s.remaining(), 5);
    assert_eq!(s.next_chunk(16), Some(b"hello" as &[u8]));
    assert!(s.is_done());
    assert_eq!(s.remaining(), 0);
    assert!(s.next_chunk(16).is_none());
}

/// An empty stream is done immediately.
#[test]
fn paste_stream_empty_is_done() {
    let mut s = PasteStream::new(Vec::new());
    assert!(s.is_done());
    assert_eq!(s.remaining(), 0);
    assert!(s.next_chunk(16).is_none());
}

/// `PasteStream` with `max_size`: chunks are split correctly and
/// reassemble to the original bytes in order.
#[test]
fn paste_stream_chunks_split_correctly() {
    let mut s = PasteStream::new(b"abcdefgh".to_vec());
    let mut got = Vec::new();
    while let Some(chunk) = s.next_chunk(3) {
        got.push(chunk.to_vec());
    }
    assert_eq!(got, vec![b"abc".to_vec(), b"def".to_vec(), b"gh".to_vec()]);
    assert!(s.is_done());
}

/// Cancellation: after `cancel`, `next_chunk` returns `None`, the
/// stream is done, and the remaining count is 0.
#[test]
fn paste_stream_cancel_drops_remaining() {
    let mut s = PasteStream::new(b"abcdefgh".to_vec());
    assert_eq!(s.next_chunk(3), Some(b"abc" as &[u8]));
    s.cancel();
    assert!(s.is_done());
    assert_eq!(s.remaining(), 0);
    assert!(s.next_chunk(3).is_none());
}

/// `remaining` counts down correctly before and after chunks.
#[test]
fn paste_stream_remaining_counts_down() {
    let mut s = PasteStream::new(b"abcdefgh".to_vec());
    assert_eq!(s.remaining(), 8);
    assert_eq!(s.next_chunk(3), Some(b"abc" as &[u8]));
    assert_eq!(s.remaining(), 5);
    assert_eq!(s.next_chunk(10), Some(b"defgh" as &[u8]));
    assert_eq!(s.remaining(), 0);
    assert!(s.is_done());
}

/// End-to-end ordering: a validated bracketed paste delivered through
/// the stream reassembles to the exact bracketed byte sequence.
#[test]
fn stream_delivers_bracketed_paste_in_order() {
    let v = validate_paste("ab\rcd", true);
    let bytes = match v {
        PasteValidation::Ok { bytes, .. } => bytes,
        other => panic!("expected Ok, got {other:?}"),
    };
    let mut s = PasteStream::new(bytes);
    let mut got = Vec::new();
    while let Some(chunk) = s.next_chunk(2) {
        got.extend_from_slice(chunk);
    }
    assert_eq!(got, [START, b"ab\ncd".as_ref(), END].concat());
}
