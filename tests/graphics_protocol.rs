//! End-to-end tests for the kitty-graphics protocol: the APC filter (state
//! machine) and the protocol handler, speaking the real kitty-graphics
//! protocol (`G` + comma-separated `key=value` control data, per ADR-011).

use nora::graphics_filter::{GraphicsFilter, parse_payload};
use nora::graphics_image::GraphicsHandler;

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

/// Wrap a `G<control data>[;<base64>]` payload in a full APC
/// (`ESC _ ... ESC \`) and feed it to the filter.
fn apc(payload: &str) -> Vec<u8> {
    let mut bytes = vec![0x1B, b'_'];
    bytes.extend_from_slice(payload.as_bytes());
    bytes.extend_from_slice(&[0x1B, b'\\']);
    bytes
}

/// A 2x2 RGBA image (16 bytes).
fn sample_rgba() -> Vec<u8> {
    vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
}

// ---------------------------------------------------------------------------
// Filter (state machine)
// ---------------------------------------------------------------------------

#[test]
fn filter_extracts_transmit() {
    let data = b64(&sample_rgba());
    let payload = format!("Gi=5,s=2,v=2,f=32;{data}");
    let bytes = apc(&payload);
    let mut filter = GraphicsFilter::new();
    let (forward, events) = filter.process(&bytes);
    // All bytes are forwarded unchanged.
    assert_eq!(forward, bytes.as_slice());
    // One event is extracted.
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.command, b'G');
    assert_eq!(event.get("i"), Some("5"));
    assert_eq!(event.get("s"), Some("2"));
    assert_eq!(event.get("v"), Some("2"));
    assert_eq!(event.get("f"), Some("32"));
    assert_eq!(event.data, sample_rgba());
}

#[test]
fn filter_extracts_query_no_data() {
    let bytes = apc("Ga=q,i=7");
    let mut filter = GraphicsFilter::new();
    let (_, events) = filter.process(&bytes);
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.command, b'G');
    assert_eq!(event.get("a"), Some("q"));
    assert_eq!(event.get("i"), Some("7"));
    assert!(event.data.is_empty());
}

#[test]
fn filter_ignores_non_graphics_apc() {
    // A non-G command (e.g. a title APC `ESC _ 1;title ESC \`) is not the
    // graphics protocol and yields no event.
    let bytes = apc("1;hello");
    let mut filter = GraphicsFilter::new();
    let (_, events) = filter.process(&bytes);
    assert!(events.is_empty());
}

#[test]
fn filter_passthrough_no_apc() {
    // Plain text (no APC) is passed through unchanged, no events.
    let bytes = b"hello world\r\n".to_vec();
    let mut filter = GraphicsFilter::new();
    let (forward, events) = filter.process(&bytes);
    assert_eq!(forward, bytes.as_slice());
    assert!(events.is_empty());
}

#[test]
fn filter_split_across_chunks() {
    // The APC is split across two process() calls; the filter reassembles it.
    let data = b64(&sample_rgba());
    let payload = format!("Gi=1,s=2,v=2;{data}");
    let mut bytes = vec![0x1B, b'_'];
    bytes.extend_from_slice(payload.as_bytes());
    bytes.extend_from_slice(&[0x1B, b'\\']);
    let split = bytes.len() / 2;
    let mut filter = GraphicsFilter::new();
    let (f1, e1) = filter.process(&bytes[..split]);
    let (f2, e2) = filter.process(&bytes[split..]);
    let mut forward = Vec::new();
    forward.extend_from_slice(f1);
    forward.extend_from_slice(f2);
    assert_eq!(forward, bytes);
    let mut events = Vec::new();
    events.extend(e1);
    events.extend(e2);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].command, b'G');
    assert_eq!(events[0].data, sample_rgba());
}

// ---------------------------------------------------------------------------
// Handler (real protocol)
// ---------------------------------------------------------------------------

#[test]
fn handler_replies_ok() {
    let data = b64(&sample_rgba());
    let event = parse_payload(format!("Gi=5,s=2,v=2;{data}").as_bytes()).unwrap();
    let mut handler = GraphicsHandler::new();
    let reply = handler.process(&event).expect("reply");
    assert_eq!(reply, "ESC _ G i=5;OK ST");
}

#[test]
fn handler_replies_err() {
    let data = b64(&sample_rgba());
    // f=100 (PNG) is unsupported in this subset.
    let event = parse_payload(format!("Gi=5,f=100,s=2,v=2;{data}").as_bytes()).unwrap();
    let mut handler = GraphicsHandler::new();
    let reply = handler.process(&event).expect("reply");
    assert_eq!(reply, "ESC _ G i=5;ERR ST");
}

#[test]
fn handler_query() {
    let data = b64(&sample_rgba());
    let mut handler = GraphicsHandler::new();
    let transmit = parse_payload(format!("Gi=5,s=2,v=2;{data}").as_bytes()).unwrap();
    handler.process(&transmit);
    let query = parse_payload("Ga=q,i=5".as_bytes()).unwrap();
    let reply = handler.process(&query).expect("reply");
    assert_eq!(reply, "ESC _ G i=5,s=2,v=2;OK ST");
}

#[test]
fn handler_delete() {
    let data = b64(&sample_rgba());
    let mut handler = GraphicsHandler::new();
    let transmit = parse_payload(format!("Gi=5,s=2,v=2;{data}").as_bytes()).unwrap();
    handler.process(&transmit);
    let delete = parse_payload("Ga=d,d=i,i=5".as_bytes()).unwrap();
    let reply = handler.process(&delete).expect("reply");
    assert_eq!(reply, "ESC _ G i=5;OK ST");
    assert_eq!(handler.images.len(), 0);
}

#[test]
fn handler_quiet_no_reply() {
    let data = b64(&sample_rgba());
    let event = parse_payload(format!("Gq=1,i=5,s=2,v=2;{data}").as_bytes()).unwrap();
    let mut handler = GraphicsHandler::new();
    let reply = handler.process(&event);
    // q=1: no reply.
    assert!(reply.is_none());
    assert_eq!(handler.images.len(), 1);
}

#[test]
fn handler_non_g_command() {
    // A non-G command is not the graphics protocol.
    let data = b64(&sample_rgba());
    let mut event = parse_payload(format!("es=2,v=2;{data}").as_bytes()).unwrap();
    event.command = b'e';
    let mut handler = GraphicsHandler::new();
    let reply = handler.process(&event);
    assert!(reply.is_none());
}
