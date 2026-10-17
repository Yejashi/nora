//! Stream positions inside a synchronized update (`CSI ? 2026 h … l`).
//!
//! The parser only buffers bytes during a synchronized update, so a
//! prompt marker or graphics command inside one must see the terminal
//! after the bytes before it, not the state from before the update.

use nora::engine::Engine;

const BSU: &[u8] = b"\x1b[?2026h";
const ESU: &[u8] = b"\x1b[?2026l";
const OSC_133_A: &[u8] = b"\x1b]133;A\x07";

fn apc(payload: &str) -> Vec<u8> {
    format!("\x1b_{payload}\x1b\\").into_bytes()
}

fn marker_lines(engine: &mut Engine) -> Vec<i32> {
    let (scrolled, pulled) = {
        let term = engine.term_handle();
        let term = term.lock();
        (term.grid().total_scrolled(), term.grid().total_pulled())
    };
    engine
        .drain_markers()
        .iter()
        .filter_map(|m| m.current_line(scrolled, pulled))
        .collect()
}

fn row_text(engine: &mut Engine, row: usize) -> String {
    let snapshot = engine.snapshot();
    let text: String = (0..snapshot.cols)
        .map(|col| snapshot.cells[row * snapshot.cols + col].ch)
        .collect();
    text.trim_end().to_string()
}

#[test]
fn marker_inside_sync_block_records_its_line() {
    let mut engine = Engine::with_history(24, 80, 0);
    let mut bytes = BSU.to_vec();
    bytes.extend_from_slice(b"a\r\nb\r\nc");
    bytes.extend_from_slice(OSC_133_A);
    bytes.extend_from_slice(b"d");
    bytes.extend_from_slice(ESU);
    engine.feed(&bytes);
    assert_eq!(marker_lines(&mut engine), vec![2]);
    assert_eq!(row_text(&mut engine, 2), "cd");
}

#[test]
fn marker_inside_sync_block_across_chunks() {
    let mut engine = Engine::with_history(24, 80, 0);
    engine.feed(BSU);
    engine.feed(b"a\r\nb\r\n");
    engine.feed(OSC_133_A);
    engine.feed(b"c");
    engine.feed(ESU);
    assert_eq!(marker_lines(&mut engine), vec![2]);
    assert_eq!(row_text(&mut engine, 2), "c");
}

/// The update stays synchronized after the position: bytes after it are
/// buffered until the update ends.
#[test]
fn sync_resumes_after_the_marker() {
    let mut engine = Engine::with_history(24, 80, 0);
    let mut bytes = BSU.to_vec();
    bytes.extend_from_slice(b"a");
    bytes.extend_from_slice(OSC_133_A);
    bytes.extend_from_slice(b"b");
    engine.feed(&bytes);
    assert_eq!(row_text(&mut engine, 0), "a");
    engine.feed(ESU);
    assert_eq!(row_text(&mut engine, 0), "ab");
}

/// A kitty query reply inside a synchronized update comes after the
/// replies to the queries before it.
#[test]
fn graphics_reply_inside_sync_is_ordered_after_dsr() {
    let mut engine = Engine::with_history(24, 80, 100);
    let mut bytes = BSU.to_vec();
    bytes.extend_from_slice(b"\x1b[6n");
    bytes.extend_from_slice(&apc("Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA"));
    bytes.extend_from_slice(ESU);
    engine.feed(&bytes);
    let replies = engine.replies();
    assert_eq!(replies, vec!["\x1b[1;1R", "\x1b_Gi=31;OK\x1b\\"]);
}

/// An image displayed inside a synchronized update is placed at the
/// cursor the bytes before it left.
#[test]
fn image_inside_sync_is_placed_at_the_cursor() {
    use base64::Engine as _;
    let pixels = base64::engine::general_purpose::STANDARD.encode(vec![0u8; 8 * 16 * 4]);
    let mut engine = Engine::with_history(24, 80, 100);
    let mut bytes = BSU.to_vec();
    bytes.extend_from_slice(b"\r\n\r\nab");
    bytes.extend_from_slice(&apc(&format!("Ga=T,s=8,v=16;{pixels}")));
    bytes.extend_from_slice(ESU);
    engine.feed(&bytes);
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.images.len(), 1);
    assert_eq!((snapshot.images[0].row, snapshot.images[0].col), (2, 2));
}
