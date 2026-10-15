//! Tests for the kitty-graphics image store, placement store, and protocol
//! handler (N33a, N34).
//!
//! The handler speaks the real kitty-graphics protocol (per ADR-011): a `G`
//! command character followed by comma-separated `key=value` control data,
//! with the optional base64 payload after a `;`.

use nora::graphics_filter::{GraphicsEvent, parse_payload};
use nora::graphics_image::{
    GraphicsHandler, ImageStore, MAX_DIMENSION, MAX_IMAGE_BYTES, PlacementStore, UploadError,
    convert_to_rgba,
};

/// Base64-encode a byte slice (standard alphabet, with padding).
/// Hand-written, mirroring the hand-written decoder in `graphics_filter`
/// (no external test dependency).
fn b64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Build a [`GraphicsEvent`] from a full `G<control data>[;<base64>]`
/// payload (the bytes that appear between `ESC _` and `ESC \`).
fn ev(payload: &str) -> GraphicsEvent {
    parse_payload(payload.as_bytes()).expect("parse payload")
}

/// A 2x2 RGBA image (16 bytes).
fn sample_rgba() -> Vec<u8> {
    vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
}

// ---------------------------------------------------------------------------
// parse_payload (real protocol)
// ---------------------------------------------------------------------------

#[test]
fn parse_payload_transmit() {
    let data = b64(&sample_rgba());
    let payload = format!("Gi=5,s=2,v=2,f=32;{data}");
    let event = ev(&payload);
    assert_eq!(event.command, b'G');
    assert_eq!(event.get("i"), Some("5"));
    assert_eq!(event.get("s"), Some("2"));
    assert_eq!(event.get("v"), Some("2"));
    assert_eq!(event.get("f"), Some("32"));
    assert_eq!(event.data, sample_rgba());
}

#[test]
fn parse_payload_query_no_data() {
    let event = ev("Ga=q,i=5");
    assert_eq!(event.command, b'G');
    assert_eq!(event.get("a"), Some("q"));
    assert_eq!(event.get("i"), Some("5"));
    assert!(event.data.is_empty());
}

#[test]
fn parse_payload_delete_no_data() {
    let event = ev("Ga=d,d=i,i=10");
    assert_eq!(event.get("a"), Some("d"));
    assert_eq!(event.get("d"), Some("i"));
    assert_eq!(event.get("i"), Some("10"));
    assert!(event.data.is_empty());
}

#[test]
fn parse_payload_get_u32() {
    let event = ev("Gs=100,v=30,X=-5");
    assert_eq!(event.get_u32("s"), Some(100));
    assert_eq!(event.get_u32("v"), Some(30));
    // X is negative, so get_u32 returns None.
    assert_eq!(event.get_u32("X"), None);
    assert_eq!(event.get_i32("X"), Some(-5));
}

// ---------------------------------------------------------------------------
// convert_to_rgba
// ---------------------------------------------------------------------------

#[test]
fn convert_to_rgba_rgb_f24() {
    // 2x2 RGB (f=24): 12 bytes in, 16 bytes RGBA out.
    let data: Vec<u8> = (0..12).collect();
    let out = convert_to_rgba(24, 2, 2, &data).expect("convert");
    assert_eq!(out.len(), 16);
    assert_eq!(&out[0..4], &[0, 1, 2, 255]);
    assert_eq!(&out[4..8], &[3, 4, 5, 255]);
}

#[test]
fn convert_to_rgba_rgba_f32() {
    // 2x2 RGBA (f=32, default): 16 bytes in, 16 bytes out (passthrough).
    let data: Vec<u8> = (0..16).collect();
    let out = convert_to_rgba(32, 2, 2, &data).expect("convert");
    assert_eq!(out, data);
}

#[test]
fn convert_to_rgba_unsupported_format() {
    // f=100 (PNG) is a later slice; unsupported here.
    let data: Vec<u8> = vec![0; 4];
    assert_eq!(
        convert_to_rgba(100, 1, 1, &data),
        Err(UploadError::UnsupportedFormat)
    );
}

#[test]
fn convert_to_rgba_length_mismatch() {
    // f=32, 2x2 expects 16 bytes; 15 is a mismatch.
    let data: Vec<u8> = vec![0; 15];
    assert_eq!(
        convert_to_rgba(32, 2, 2, &data),
        Err(UploadError::DataLengthMismatch)
    );
}

#[test]
fn convert_to_rgba_zero_dimension() {
    let data: Vec<u8> = vec![];
    assert_eq!(
        convert_to_rgba(32, 0, 4, &data),
        Err(UploadError::DimensionTooLarge)
    );
}

// ---------------------------------------------------------------------------
// ImageStore::upload_rgba
// ---------------------------------------------------------------------------

#[test]
fn upload_rgba_auto_assign() {
    let mut store = ImageStore::new();
    let rgba = vec![0u8; 16]; // 2x2 RGBA.
    let id = store.upload_rgba(2, 2, &rgba, None).expect("upload");
    assert_eq!(id, 1); // IDs are 1-based.
    assert_eq!(store.len(), 1);
    let img = store.query(id).expect("query");
    assert_eq!(img.width, 2);
    assert_eq!(img.height, 2);
    assert_eq!(img.pixels, rgba);
    let id2 = store.upload_rgba(2, 2, &rgba, None).expect("upload");
    assert_eq!(id2, 2);
}

#[test]
fn upload_rgba_explicit_id() {
    let mut store = ImageStore::new();
    let rgba = vec![0u8; 16];
    let id = store.upload_rgba(2, 2, &rgba, Some(5)).expect("upload");
    assert_eq!(id, 5);
    assert!(store.query(5).is_some());
    assert!(store.query(1).is_none());
}

#[test]
fn upload_rgba_id_collision() {
    let mut store = ImageStore::new();
    let rgba = vec![0u8; 16];
    assert!(store.upload_rgba(2, 2, &rgba, Some(5)).is_ok());
    assert_eq!(
        store.upload_rgba(2, 2, &rgba, Some(5)),
        Err(UploadError::MalformedParams)
    );
}

#[test]
fn upload_rgba_dimension_too_large() {
    let mut store = ImageStore::new();
    let rgba = vec![0u8; 16];
    assert_eq!(
        store.upload_rgba(MAX_DIMENSION + 1, 1, &rgba, None),
        Err(UploadError::DimensionTooLarge)
    );
}

#[test]
fn upload_rgba_total_budget() {
    let mut store = ImageStore::new();
    // 2048x4096 RGBA = 32 MiB per image (== MAX_IMAGE_BYTES). Four such
    // images fill MAX_TOTAL_BYTES (128 MiB); a fifth exceeds it.
    let rgba: Vec<u8> = vec![0; 2048 * 4096 * 4];
    assert_eq!(rgba.len(), MAX_IMAGE_BYTES);
    for _ in 0..4 {
        assert!(store.upload_rgba(2048, 4096, &rgba, None).is_ok());
    }
    // One more byte exceeds the total budget.
    let rgba2: Vec<u8> = vec![0; 4];
    assert_eq!(
        store.upload_rgba(1, 1, &rgba2, None),
        Err(UploadError::TotalBudget)
    );
}

#[test]
fn image_store_clear() {
    let mut store = ImageStore::new();
    let rgba = vec![0u8; 16];
    store.upload_rgba(2, 2, &rgba, None).expect("upload");
    store.upload_rgba(2, 2, &rgba, None).expect("upload");
    assert_eq!(store.len(), 2);
    assert_eq!(store.total_bytes(), 32);
    let n = store.clear();
    assert_eq!(n, 2);
    assert_eq!(store.len(), 0);
    assert_eq!(store.total_bytes(), 0);
}

// ---------------------------------------------------------------------------
// PlacementStore
// ---------------------------------------------------------------------------

#[test]
fn placement_store_place_and_delete() {
    let mut store = PlacementStore::new();
    let id = store.place(1, 0, 0, 10, 5).expect("place");
    assert_eq!(id, 1);
    assert_eq!(store.len(), 1);
    assert!(store.get(1).is_some());
    let p = store.delete(1);
    assert!(p.is_some());
    assert!(store.is_empty());
}

#[test]
fn placement_store_zero_dimensions() {
    let mut store = PlacementStore::new();
    assert!(store.place(1, 0, 0, 0, 5).is_err());
    assert!(store.place(1, 0, 0, 10, 0).is_err());
}

#[test]
fn placement_store_clear() {
    let mut store = PlacementStore::new();
    store.place(1, 0, 0, 10, 5).expect("place");
    store.place(1, 1, 1, 10, 5).expect("place");
    assert_eq!(store.len(), 2);
    store.clear();
    assert!(store.is_empty());
}

// ---------------------------------------------------------------------------
// GraphicsHandler (real protocol)
// ---------------------------------------------------------------------------

#[test]
fn handler_transmit_no_id_no_reply() {
    let mut h = GraphicsHandler::new();
    let data = b64(&sample_rgba());
    let event = ev(&format!("Gs=2,v=2;{data}"));
    let reply = h.process(&event);
    // No `i` key: no reply (real protocol), but the image is stored.
    assert!(reply.is_none());
    assert_eq!(h.images.len(), 1);
}

#[test]
fn handler_transmit_with_id_reply() {
    let mut h = GraphicsHandler::new();
    let data = b64(&sample_rgba());
    let event = ev(&format!("Gi=5,s=2,v=2;{data}"));
    let reply = h.process(&event).expect("reply");
    assert_eq!(reply, "ESC _ G i=5;OK ST");
    assert_eq!(h.images.len(), 1);
    assert!(h.images.query(5).is_some());
}

#[test]
fn handler_transmit_bad_format() {
    let mut h = GraphicsHandler::new();
    let data = b64(&sample_rgba());
    let event = ev(&format!("Gi=5,f=100,s=2,v=2;{data}"));
    let reply = h.process(&event).expect("reply");
    assert_eq!(reply, "ESC _ G i=5;ERR ST");
    assert_eq!(h.images.len(), 0);
}

#[test]
fn handler_transmit_default_format() {
    // No `f` key: defaults to f=32 (RGBA).
    let mut h = GraphicsHandler::new();
    let data = b64(&sample_rgba());
    let event = ev(&format!("Gi=1,s=2,v=2;{data}"));
    let reply = h.process(&event).expect("reply");
    assert_eq!(reply, "ESC _ G i=1;OK ST");
}

#[test]
fn handler_place() {
    let mut h = GraphicsHandler::new();
    let data = b64(&sample_rgba());
    h.process(&ev(&format!("Gi=5,s=2,v=2;{data}")));
    let place_event = ev("Ga=p,i=5,X=1,Y=2,c=10,r=5");
    let reply = h.process(&place_event).expect("reply");
    assert_eq!(reply, "ESC _ G i=5,p=1;OK ST");
    assert_eq!(h.placements.len(), 1);
}

#[test]
fn handler_place_missing_image() {
    let mut h = GraphicsHandler::new();
    let place_event = ev("Ga=p,i=99,X=0,Y=0,c=10,r=5");
    let reply = h.process(&place_event).expect("reply");
    assert_eq!(reply, "ESC _ G i=99;ERR ST");
}

#[test]
fn handler_query_found() {
    let mut h = GraphicsHandler::new();
    let data = b64(&sample_rgba());
    h.process(&ev(&format!("Gi=5,s=2,v=2;{data}")));
    let query_event = ev("Ga=q,i=5");
    let reply = h.process(&query_event).expect("reply");
    assert_eq!(reply, "ESC _ G i=5,s=2,v=2;OK ST");
}

#[test]
fn handler_query_not_found() {
    let mut h = GraphicsHandler::new();
    let query_event = ev("Ga=q,i=99");
    let reply = h.process(&query_event).expect("reply");
    assert_eq!(reply, "ESC _ G i=99;ERR ST");
}

#[test]
fn handler_delete_image() {
    let mut h = GraphicsHandler::new();
    let data = b64(&sample_rgba());
    h.process(&ev(&format!("Gi=5,s=2,v=2;{data}")));
    let delete_event = ev("Ga=d,d=i,i=5");
    let reply = h.process(&delete_event).expect("reply");
    assert_eq!(reply, "ESC _ G i=5;OK ST");
    assert_eq!(h.images.len(), 0);
}

#[test]
fn handler_delete_image_not_found() {
    let mut h = GraphicsHandler::new();
    let delete_event = ev("Ga=d,d=i,i=99");
    let reply = h.process(&delete_event).expect("reply");
    assert_eq!(reply, "ESC _ G i=99;ERR ST");
}

#[test]
fn handler_delete_all_images() {
    let mut h = GraphicsHandler::new();
    let data = b64(&sample_rgba());
    h.process(&ev(&format!("Gi=1,s=2,v=2;{data}")));
    h.process(&ev(&format!("Gi=2,s=2,v=2;{data}")));
    assert_eq!(h.images.len(), 2);
    let delete_event = ev("Ga=d,d=i");
    let reply = h.process(&delete_event).expect("reply");
    assert_eq!(reply, "ESC _ G i=0;OK ST");
    assert_eq!(h.images.len(), 0);
}

#[test]
fn handler_delete_placement() {
    let mut h = GraphicsHandler::new();
    let data = b64(&sample_rgba());
    h.process(&ev(&format!("Gi=5,s=2,v=2;{data}")));
    h.process(&ev("Ga=p,i=5,X=0,Y=0,c=10,r=5"));
    assert_eq!(h.placements.len(), 1);
    let delete_event = ev("Ga=d,d=p,p=1");
    let reply = h.process(&delete_event).expect("reply");
    assert_eq!(reply, "ESC _ G p=1;OK ST");
    assert_eq!(h.placements.len(), 0);
}

#[test]
fn handler_delete_clear_placements() {
    let mut h = GraphicsHandler::new();
    let data = b64(&sample_rgba());
    h.process(&ev(&format!("Gi=5,s=2,v=2;{data}")));
    h.process(&ev("Ga=p,i=5,X=0,Y=0,c=10,r=5"));
    assert_eq!(h.placements.len(), 1);
    // a=d with no d key: clear all visible placements.
    let delete_event = ev("Ga=d");
    let reply = h.process(&delete_event).expect("reply");
    assert_eq!(reply, "ESC _ G i=0;OK ST");
    assert_eq!(h.placements.len(), 0);
    // The image is still stored.
    assert_eq!(h.images.len(), 1);
}

#[test]
fn handler_quiet_suppresses_reply() {
    let mut h = GraphicsHandler::new();
    let data = b64(&sample_rgba());
    let event = ev(&format!("Gq=1,i=5,s=2,v=2;{data}"));
    let reply = h.process(&event);
    // q=1: no reply, but the image is still stored.
    assert!(reply.is_none());
    assert_eq!(h.images.len(), 1);
}

#[test]
fn handler_non_g_command_no_reply() {
    let mut h = GraphicsHandler::new();
    let data = b64(&sample_rgba());
    // A non-G command is not the graphics protocol.
    let mut event = ev(&format!("es=2,v=2;{data}"));
    event.command = b'e';
    let reply = h.process(&event);
    assert!(reply.is_none());
}
