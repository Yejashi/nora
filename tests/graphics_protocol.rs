//! Kitty graphics through the stream (ADR-014): commands are handled at
//! their place in the output, with the cursor there, through
//! `Engine::feed` (the PTY path runs the same `StreamFilter`).

use nora::engine::Engine;

/// `ESC _ <payload> ESC \`.
fn apc(payload: &str) -> Vec<u8> {
    format!("\x1b_{payload}\x1b\\").into_bytes()
}

/// RGBA pixels for a `w`x`h` image, base64 (all zero bytes).
fn zeros(w: usize, h: usize) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(vec![0u8; w * h * 4])
}

/// The default cell for graphics without a window is 8x16 pixels.
fn engine() -> Engine {
    Engine::with_history(24, 80, 100)
}

#[test]
fn detection_reply_comes_before_the_device_attributes() {
    let mut e = engine();
    let mut bytes = apc("Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA");
    bytes.extend_from_slice(b"\x1b[c");
    e.feed(&bytes);
    let replies = e.replies();
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0], "\x1b_Gi=31;OK\x1b\\");
    // Then DA1: a VT220 with ANSI color, longer than `?6c` (kitty's icat
    // ignores a shorter reply and waits for its timeout).
    assert_eq!(replies[1], "\x1b[?62;22c");
}

#[test]
fn display_moves_the_cursor_past_the_image() {
    let mut e = engine();
    e.feed(b"ab");
    // 16x32 px at 8x16 cells: 2 columns, 2 rows.
    e.feed(&apc(&format!("Ga=T,s=16,v=32;{}", zeros(16, 32))));
    let snap = e.snapshot();
    assert_eq!(
        snap.cursor,
        Some((1, 4)),
        "down rows-1, right by the columns"
    );
    assert_eq!(snap.images.len(), 1);
    assert_eq!((snap.images[0].row, snap.images[0].col), (0, 2));
    e.feed(&apc(&format!("Ga=T,C=1,s=16,v=32;{}", zeros(16, 32))));
    assert_eq!(e.snapshot().cursor, Some((1, 4)), "C=1 keeps the cursor");
}

#[test]
fn an_image_reaching_the_right_edge_ends_on_the_next_line() {
    let mut e = engine();
    e.feed(b"\x1b[1;79H");
    e.feed(&apc(&format!("Ga=T,s=16,v=16;{}", zeros(16, 16))));
    assert_eq!(e.snapshot().cursor, Some((1, 0)));
}

#[test]
fn display_at_the_bottom_scrolls_and_the_image_follows() {
    let mut e = engine();
    e.feed(b"\x1b[24;1H");
    // Three rows tall at the last line: the screen scrolls by two.
    e.feed(&apc(&format!("Ga=T,s=8,v=48;{}", zeros(8, 48))));
    let snap = e.snapshot();
    assert_eq!(snap.cursor, Some((23, 1)));
    assert_eq!(snap.images[0].row, 21, "the image moved up with the text");
    e.feed(&b"\n".repeat(30));
    assert!(e.snapshot().images.is_empty(), "scrolled into the history");
    e.scroll(alacritty_terminal::grid::Scroll::Delta(30));
    let snap = e.snapshot();
    assert_eq!(snap.images.len(), 1, "visible again when scrolled back");
    assert_eq!(snap.images[0].row, 21 - 30 + snap.display_offset as i32);
}

#[test]
fn clears_remove_placements_but_keep_images() {
    let mut e = engine();
    e.feed(&apc(&format!("Ga=T,i=3,s=8,v=16;{}", zeros(8, 16))));
    assert_eq!(e.snapshot().images.len(), 1);
    e.feed(b"\x1b[H\x1b[2J");
    assert!(e.snapshot().images.is_empty());
    assert!(e.graphics().image(3).is_some());
    e.feed(&apc("Ga=p,i=3"));
    assert_eq!(
        e.snapshot().images.len(),
        1,
        "placed again from the stored image"
    );
    e.feed(b"\x1bc");
    assert!(e.snapshot().images.is_empty());
    assert!(
        e.graphics().image(3).is_none(),
        "a reset deletes images too"
    );
}

#[test]
fn the_alternate_screen_has_its_own_images() {
    let mut e = engine();
    e.feed(&apc(&format!("Ga=T,i=1,C=1,s=8,v=16;{}", zeros(8, 16))));
    e.feed(b"\x1b[?1049h");
    assert!(
        e.snapshot().images.is_empty(),
        "the main screen's image is hidden"
    );
    e.feed(&apc("Ga=p,i=1,C=1"));
    assert_eq!(e.snapshot().images.len(), 1);
    e.feed(b"\x1b[?1049l");
    assert_eq!(
        e.snapshot().images.len(),
        1,
        "the main screen's image is back"
    );
    e.feed(b"\x1b[?1049h");
    assert!(
        e.snapshot().images.is_empty(),
        "the alternate screen's image is gone"
    );
}

#[test]
fn markers_and_images_in_one_chunk_keep_their_order() {
    let mut e = engine();
    let mut bytes = b"\x1b]133;A\x1b\\".to_vec();
    bytes.extend_from_slice(&apc(&format!("Ga=T,s=8,v=32;{}", zeros(8, 32))));
    bytes.extend_from_slice(b"\x1b]133;B\x1b\\");
    e.feed(&bytes);
    let markers = e.drain_markers();
    let lines: Vec<i32> = markers.iter().map(|m| m.line).collect();
    assert_eq!(
        lines,
        vec![0, 1],
        "B after the two-row image's cursor movement"
    );
}

#[test]
fn an_image_change_without_damage_is_a_frame() {
    let mut e = engine();
    e.feed(&apc(&format!("Ga=T,i=1,C=1,s=8,v=16;{}", zeros(8, 16))));
    let _ = e.snapshot();
    assert!(!e.graphics_changed());
    e.feed(&apc("Ga=d"));
    assert!(e.graphics_changed(), "a delete changes no cell");
    assert!(e.snapshot().images.is_empty());
    assert!(!e.graphics_changed());
}
