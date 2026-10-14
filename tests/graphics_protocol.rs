//! N33b: neutral Rust protocol client + placement/clip/scroll tests.
//!
//! The protocol client builds raw kitty-graphics APC sequences (wire
//! format), feeds them through the N32a [`GraphicsFilter`] (the protocol
//! parser) and the N33b [`GraphicsHandler`] (the protocol processor), and
//! verifies the documented replies. It is independent of nora's rendering:
//! it exercises the protocol path (wire → filter → handler → reply) only.
//!
//! Placement, clipping, scroll and alternate-screen behavior are tested
//! against the [`PlacementStore`] and [`clip`] directly.

use nora::graphics_filter::GraphicsFilter;
use nora::graphics_image::{
    GraphicsHandler, ImageStore, MAX_DIMENSION, PlacementError, PlacementStore, UploadError, clip,
};

/// Build a raw graphics APC: `ESC _ <cmd>;<params> ST` (ST = `ESC \`).
fn apc(cmd: u8, params: &str) -> Vec<u8> {
    let mut out = vec![0x1b, b'_'];
    out.push(cmd);
    out.push(b';');
    out.extend_from_slice(params.as_bytes());
    out.extend_from_slice(&[0x1b, b'\\']);
    out
}

/// Feed raw bytes through the filter and process the extracted events
/// through the handler, returning all replies in order.
fn run(bytes: &[u8], handler: &mut GraphicsHandler) -> Vec<String> {
    let mut filter = GraphicsFilter::new();
    let (_forward, events) = filter.process(bytes);
    let mut replies = Vec::new();
    for event in events {
        if let Some(reply) = handler.process(&event) {
            replies.push(reply);
        }
    }
    replies
}

/// A minimal base64 encoder (standard alphabet, `=` padding) matching the
/// N32a filter's hand-written decoder. No external dependency.
fn b64_encode(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(A[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(A[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// A 4x4 RGBA image (16 pixels, 64 bytes), base64-encoded.
fn test_image_b64() -> String {
    let pixels = [0xFFu8, 0x00, 0x00, 0xFF].repeat(16); // 16 * 4 = 64 bytes
    b64_encode(&pixels)
}

// --- Upload (e) -----------------------------------------------------------

#[test]
fn upload_rgba_reply_success() {
    let mut handler = GraphicsHandler::new();
    let data = test_image_b64();
    // flags=1 (reply), id=0 (auto), w=4, h=4, colorspace=1, format=1 (RGBA).
    let bytes = apc(b'e', &format!("1;0;4;4;1;1;{data}"));
    let replies = run(&bytes, &mut handler);
    assert_eq!(replies.len(), 1);
    // Auto-assigned ID is 0 (first upload).
    assert_eq!(replies[0], "ESC _ e;0;0 ST");
    assert_eq!(handler.images.len(), 1);
}

#[test]
fn upload_explicit_id_reply() {
    let mut handler = GraphicsHandler::new();
    let data = test_image_b64();
    // flags=1, id=42 (explicit), w=4, h=4, colorspace=1, format=1.
    let bytes = apc(b'e', &format!("1;42;4;4;1;1;{data}"));
    let replies = run(&bytes, &mut handler);
    assert_eq!(replies, vec!["ESC _ e;0;42 ST".to_string()]);
    assert!(handler.images.query(42).is_some());
}

#[test]
fn upload_no_reply_flag_no_reply() {
    let mut handler = GraphicsHandler::new();
    let data = test_image_b64();
    // flags=0 (no reply).
    let bytes = apc(b'e', &format!("0;0;4;4;1;1;{data}"));
    let replies = run(&bytes, &mut handler);
    assert!(replies.is_empty());
    // But the image is still stored.
    assert_eq!(handler.images.len(), 1);
}

#[test]
fn upload_malformed_params_invalid() {
    let mut handler = GraphicsHandler::new();
    // Only 5 fields (missing colorspace, format, data).
    let bytes = apc(b'e', "1;0;4;4;1");
    let replies = run(&bytes, &mut handler);
    assert_eq!(replies, vec!["ESC _ e;2;0 ST".to_string()]);
    assert_eq!(handler.images.len(), 0);
}

#[test]
fn upload_unsupported_format_invalid() {
    let mut handler = GraphicsHandler::new();
    let data = test_image_b64();
    // format=0 (RGB) is not supported (only RGBA=1).
    let bytes = apc(b'e', &format!("1;0;4;4;1;0;{data}"));
    let replies = run(&bytes, &mut handler);
    assert_eq!(replies, vec!["ESC _ e;2;0 ST".to_string()]);
}

#[test]
fn upload_data_length_mismatch_corrupted() {
    let mut handler = GraphicsHandler::new();
    // Claim 4x4 (64 bytes) but provide only 16 bytes of base64 data.
    let short = b64_encode(&[0u8; 16]);
    let bytes = apc(b'e', &format!("1;0;4;4;1;1;{short}"));
    let replies = run(&bytes, &mut handler);
    assert_eq!(replies, vec!["ESC _ e;3;0 ST".to_string()]);
}

#[test]
fn upload_dimension_too_large_invalid() {
    let mut handler = GraphicsHandler::new();
    // width = MAX_DIMENSION + 1.
    let bytes = apc(b'e', &format!("1;0;{};4;1;1;AAAA", MAX_DIMENSION + 1));
    let replies = run(&bytes, &mut handler);
    assert_eq!(replies, vec!["ESC _ e;2;0 ST".to_string()]);
}

#[test]
fn upload_per_image_budget_alloc_failure() {
    let mut handler = GraphicsHandler::new();
    // 4096x4096 RGBA = 64 MiB > 32 MiB per-image budget.
    let w: u32 = 4096;
    let h: u32 = 4096;
    let bytes_needed = (w * h * 4) as usize;
    let data = vec![0u8; bytes_needed];
    let b64 = b64_encode(&data);
    let bytes = apc(b'e', &format!("1;0;{w};{h};1;1;{b64}"));
    let replies = run(&bytes, &mut handler);
    assert_eq!(replies, vec!["ESC _ e;4;0 ST".to_string()]);
    assert_eq!(handler.images.len(), 0);
}

// --- Query (q) -------------------------------------------------------------

#[test]
fn query_found_reply() {
    let mut handler = GraphicsHandler::new();
    let data = test_image_b64();
    let up = apc(b'e', &format!("0;7;4;4;1;1;{data}"));
    run(&up, &mut handler);
    // Query id=7 (reply).
    let q = apc(b'q', "1;7");
    let replies = run(&q, &mut handler);
    assert_eq!(replies, vec!["ESC _ q;0;7;4;4;1;1 ST".to_string()]);
}

#[test]
fn query_not_found() {
    let mut handler = GraphicsHandler::new();
    let q = apc(b'q', "1;99");
    let replies = run(&q, &mut handler);
    assert_eq!(replies, vec!["ESC _ q;1;99 ST".to_string()]);
}

// --- Place (t) -------------------------------------------------------------

#[test]
fn place_success_reply() {
    let mut handler = GraphicsHandler::new();
    let data = test_image_b64();
    run(&apc(b'e', &format!("0;5;4;4;1;1;{data}")), &mut handler);
    // Place image 5 at (0,0) size 4x4 (reply). First placement gets ID 1.
    let t = apc(b't', "1;5;0;0;4;4");
    let replies = run(&t, &mut handler);
    assert_eq!(replies, vec!["ESC _ t;0;1 ST".to_string()]);
    assert_eq!(handler.placements.len(), 1);
}

#[test]
fn place_missing_image_invalid() {
    let mut handler = GraphicsHandler::new();
    // Place a non-existent image (reply).
    let t = apc(b't', "1;99;0;0;4;4");
    let replies = run(&t, &mut handler);
    assert_eq!(replies, vec!["ESC _ t;2;0 ST".to_string()]);
}

// --- Delete image (c) ------------------------------------------------------

#[test]
fn delete_image_found() {
    let mut handler = GraphicsHandler::new();
    let data = test_image_b64();
    run(&apc(b'e', &format!("0;9;4;4;1;1;{data}")), &mut handler);
    assert_eq!(handler.images.len(), 1);
    // Delete image 9 (reply).
    let c = apc(b'c', "1;9");
    let replies = run(&c, &mut handler);
    assert_eq!(replies, vec!["ESC _ c;0;9 ST".to_string()]);
    assert_eq!(handler.images.len(), 0);
}

#[test]
fn delete_image_not_found() {
    let mut handler = GraphicsHandler::new();
    let c = apc(b'c', "1;99");
    let replies = run(&c, &mut handler);
    assert_eq!(replies, vec!["ESC _ c;1;99 ST".to_string()]);
}

// --- Delete placement (d) --------------------------------------------------

#[test]
fn delete_placement_found() {
    let mut handler = GraphicsHandler::new();
    let data = test_image_b64();
    run(&apc(b'e', &format!("0;5;4;4;1;1;{data}")), &mut handler);
    run(&apc(b't', "0;5;0;0;4;4"), &mut handler);
    assert_eq!(handler.placements.len(), 1);
    // Delete placement 1 (the first auto-assigned ID; reply).
    let d = apc(b'd', "1;1");
    let replies = run(&d, &mut handler);
    assert_eq!(replies, vec!["ESC _ d;0;1 ST".to_string()]);
    assert_eq!(handler.placements.len(), 0);
    // The image is still present (placements are separate from images).
    assert_eq!(handler.images.len(), 1);
}

#[test]
fn delete_placement_not_found() {
    let mut handler = GraphicsHandler::new();
    let d = apc(b'd', "1;99");
    let replies = run(&d, &mut handler);
    assert_eq!(replies, vec!["ESC _ d;1;99 ST".to_string()]);
}

// --- Placement store: scroll / alternate screen ----------------------------

#[test]
fn scroll_moves_placements_up_and_drops_offscreen() {
    let mut store = PlacementStore::new();
    let id = store.place(1, 0, 5, 4, 4).unwrap();
    // Scroll up by 5 rows: the placement (y=5, rows=4) moves to y=0.
    store.scroll(5);
    let p = store.get(id).unwrap();
    assert_eq!(p.y, 0);
    // Scroll up by 5 more: y=0 -> y=-5, and y+rows = -5+4 = -1 <= 0,
    // so it is dropped.
    store.scroll(5);
    assert!(store.get(id).is_none());
    assert!(store.is_empty());
}

#[test]
fn scroll_keeps_partially_visible() {
    let mut store = PlacementStore::new();
    let id = store.place(1, 0, 2, 4, 4).unwrap();
    // Scroll up by 2: y=2 -> y=0, y+rows = 0+4 = 4 > 0, kept.
    store.scroll(2);
    let p = store.get(id).unwrap();
    assert_eq!(p.y, 0);
}

#[test]
fn alternate_screen_clears_placements() {
    let mut store = PlacementStore::new();
    store.place(1, 0, 0, 4, 4).unwrap();
    store.place(2, 1, 0, 4, 4).unwrap();
    assert_eq!(store.len(), 2);
    // Switching to the alternate screen clears all placements.
    store.clear();
    assert!(store.is_empty());
}

// --- Clipping --------------------------------------------------------------

#[test]
fn clip_fully_visible() {
    // Placement (0,0) size 4x4 in a 10x10 viewport: fully visible.
    let r = clip(0, 0, 4, 4, 10, 10).unwrap();
    assert_eq!((r.x, r.y, r.columns, r.rows), (0, 0, 4, 4));
}

#[test]
fn clip_partial_offscreen_right() {
    // Placement (8,0) size 4x4 in a 10x10 viewport: x=8..12, clipped to 8..10.
    let r = clip(8, 0, 4, 4, 10, 10).unwrap();
    assert_eq!((r.x, r.y, r.columns, r.rows), (8, 0, 2, 4));
}

#[test]
fn clip_partial_offscreen_top() {
    // Placement (0,-2) size 4x4 in a 10x10 viewport: y=-2..2, clipped to 0..2.
    let r = clip(0, -2, 4, 4, 10, 10).unwrap();
    assert_eq!((r.x, r.y, r.columns, r.rows), (0, 0, 4, 2));
}

#[test]
fn clip_entirely_offscreen_none() {
    // Placement (20,0) size 4x4 in a 10x10 viewport: entirely to the right.
    assert!(clip(20, 0, 4, 4, 10, 10).is_none());
    // Placement (0,20) size 4x4: entirely below.
    assert!(clip(0, 20, 4, 4, 10, 10).is_none());
    // Placement (-4,0) size 4x4: entirely to the left.
    assert!(clip(-4, 0, 4, 4, 10, 10).is_none());
}

// --- Placement store errors ------------------------------------------------

#[test]
fn place_invalid_dimensions() {
    let mut store = PlacementStore::new();
    assert_eq!(
        store.place(1, 0, 0, 0, 4),
        Err(PlacementError::InvalidDimensions)
    );
    assert_eq!(
        store.place(1, 0, 0, 4, 0),
        Err(PlacementError::InvalidDimensions)
    );
}

// --- Image store budget (N33a regression, exercised via handler) -----------

#[test]
fn upload_total_budget_too_many() {
    let mut handler = GraphicsHandler::new();
    // Each image is 4 MiB (1024x1024 RGBA). 33 images = 132 MiB > 128 MiB.
    let w: u32 = 1024;
    let h: u32 = 1024;
    let data = vec![0u8; (w * h * 4) as usize];
    let b64 = b64_encode(&data);
    let mut last = String::new();
    for _ in 0..33 {
        let bytes = apc(b'e', &format!("1;0;{w};{h};1;1;{b64}"));
        let mut replies = run(&bytes, &mut handler);
        last = replies.pop().unwrap();
    }
    // The 33rd upload exceeds the total budget: status TOO_MANY (1).
    assert_eq!(last, "ESC _ e;1;0 ST");
}

// --- Unsupported command: no reply ----------------------------------------

#[test]
fn unsupported_command_no_reply() {
    let mut handler = GraphicsHandler::new();
    // 'r' (resize) is not implemented: no reply.
    let bytes = apc(b'r', "1;0;4;4");
    let replies = run(&bytes, &mut handler);
    assert!(replies.is_empty());
}

// ---------------------------------------------------------------------------
// Alternate screen
// ---------------------------------------------------------------------------

/// Switching to the alternate screen clears all placements (but keeps the
/// images); the protocol reflects the cleared state.
#[test]
fn alternate_screen_keeps_images() {
    let mut handler = GraphicsHandler::new();

    // Upload an image and place it on the main screen.
    let data = test_image_b64();
    run(&apc(b'e', &format!("0;1;4;4;1;1;{data}")), &mut handler);
    let mut replies = run(&apc(b't', "1;1;0;0;4;4"), &mut handler);
    assert_eq!(replies.pop(), Some("ESC _ t;0;1 ST".to_string()));

    // The placement is visible.
    assert!(handler.placements.get(1).is_some());

    // Switch to the alternate screen: clear all placements.
    handler.placements.clear();
    assert!(handler.placements.is_empty());

    // The image is still in the image store (only the placement was cleared).
    assert!(handler.images.query(1).is_some());

    // The protocol reflects the cleared state: a new placement gets a fresh
    // ID (the old ID 1 is gone; IDs are not reused, so the next is 2).
    let mut replies = run(&apc(b't', "1;1;0;0;4;4"), &mut handler);
    assert_eq!(replies.pop(), Some("ESC _ t;0;2 ST".to_string()));
}

// Silence unused-import warnings for the re-exported types used in docs.
#[allow(dead_code)]
fn _assert_types() {
    let _ = UploadError::MalformedParams;
    let _ = ImageStore::new();
}
