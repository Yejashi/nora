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

/// `Engine::feed` loads files before it locks the terminal (ADR-014,
/// "Media reads"); replies keep their order with the parser's.
#[test]
fn files_through_feed_keep_the_reply_order() {
    use base64::Engine as _;
    let dir = std::env::temp_dir().join(format!("nora-feed-media-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("pixels.rgba");
    std::fs::write(&file, vec![0u8; 4]).unwrap();
    let path = base64::engine::general_purpose::STANDARD.encode(file.to_string_lossy().as_bytes());
    let mut e = engine();
    let mut bytes = apc(&format!("Ga=q,t=f,s=1,v=1,i=4;{path}"));
    bytes.extend_from_slice(b"\x1b[c");
    bytes.extend_from_slice(&apc(&format!("Ga=t,t=f,s=1,v=1,i=5;{path}")));
    e.feed(&bytes);
    std::fs::remove_dir_all(&dir).unwrap();
    let replies = e.replies();
    assert_eq!(replies.len(), 3, "{replies:?}");
    assert_eq!(replies[0], "\x1b_Gi=4;OK\x1b\\");
    assert_eq!(replies[1], "\x1b[?62;22c");
    assert_eq!(replies[2], "\x1b_Gi=5;OK\x1b\\");
    assert!(e.graphics().image(5).is_some());
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

/// Line feed/new line mode (`CSI 20 h`) does not change the movement
/// after an image: it moves down with IND, which keeps the column.
/// (vte 0.15 never applies the mode to LF either, so this guards the
/// movement against a future engine that does.)
#[test]
fn display_movement_ignores_line_feed_new_line_mode() {
    let mut e = engine();
    e.feed(b"\x1b[20hab");
    e.feed(&apc(&format!("Ga=T,s=16,v=32;{}", zeros(16, 32))));
    assert_eq!(e.snapshot().cursor, Some((1, 4)));
    e.feed(b"\x1b[24;3H");
    e.feed(&apc(&format!("Ga=T,s=16,v=48;{}", zeros(16, 48))));
    assert_eq!(e.snapshot().cursor, Some((23, 4)), "scrolled at the bottom");
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

/// The alternate screen's grid scrolls on its own (as `less` does): its
/// counters must not age, and so delete, the main screen's placements,
/// nor may its `CSI 3 J` (it has no scrollback).
#[test]
fn alternate_screen_scrolling_keeps_the_main_screens_images() {
    let mut e = engine();
    e.feed(&apc(&format!("Ga=T,i=1,C=1,s=8,v=16;{}", zeros(8, 16))));
    // Scrolled into the main screen's history, still within it.
    e.feed(&b"\n".repeat(30));
    e.feed(b"\x1b[?1049h");
    e.feed(&b"\n".repeat(200));
    assert!(e.snapshot().images.is_empty());
    e.feed(b"\x1b[3J");
    assert!(e.snapshot().images.is_empty());
    e.feed(b"\x1b[?1049l");
    e.scroll(alacritty_terminal::grid::Scroll::Delta(30));
    assert_eq!(
        e.snapshot().images.len(),
        1,
        "the main screen's image is still in its history"
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

/// Unicode placeholders (ADR-014): a virtual placement (`U=1`) draws
/// nothing by itself and does not move the cursor; `U+10EEEE` cells show
/// its tiles. The image id is the foreground color, row and column are
/// combining marks, and a cell without marks continues the one to its
/// left.
#[test]
fn unicode_placeholders_show_tiles() {
    let mut e = engine();
    // 20x20 px into 2x1 cells of 8x16: fitted to 16x16, centered (no gap
    // horizontally: 2 cells are 16 px wide).
    e.feed(&apc(&format!(
        "Ga=T,U=1,i=42,c=2,r=1,q=2,s=20,v=20;{}",
        zeros(20, 20)
    )));
    let snap = e.snapshot();
    assert_eq!(
        snap.cursor,
        Some((0, 0)),
        "a virtual placement keeps the cursor"
    );
    assert!(snap.images.is_empty(), "nothing without placeholder cells");
    // 256-color id 42; row 0 col 0 by marks (U+0305 = 0), then inherited.
    e.feed("\x1b[38;5;42m\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\x1b[m".as_bytes());
    let tiles = e.snapshot().images;
    let geometry: Vec<_> = tiles
        .iter()
        .map(|t| (t.row, t.col, t.source, t.size))
        .collect();
    assert_eq!(
        geometry,
        vec![
            (0, 0, [0, 0, 10, 20], (8, 16)),
            (0, 1, [10, 0, 10, 20], (8, 16))
        ],
        "each cell shows its half of the fitted image"
    );
}

#[test]
fn placeholder_ids_from_true_color_and_high_byte() {
    let mut e = engine();
    // Image id 0x05_000102: 24-bit color 0,1,2 and a third mark for 5.
    let id = 0x0500_0102u32;
    e.feed(&apc(&format!(
        "Ga=T,U=1,i={id},c=1,r=1,q=2,s=8,v=16;{}",
        zeros(8, 16)
    )));
    // Marks: row 0 (U+0305), col 0 (U+0305), high byte 5 (the 6th mark,
    // U+033D).
    e.feed("\x1b[38;2;0;1;2m\u{10EEEE}\u{0305}\u{0305}\u{033D}\x1b[m".as_bytes());
    let tiles = e.snapshot().images;
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].image_id, id);
    // Without the high byte it names another image: nothing to show.
    e.feed("\r\n\x1b[38;2;0;1;2m\u{10EEEE}\u{0305}\u{0305}\x1b[m".as_bytes());
    assert_eq!(e.snapshot().images.len(), 1);
}

#[test]
fn placeholder_placement_ids_and_missing_placements() {
    let mut e = engine();
    e.feed(&apc(&format!("Ga=t,i=7,q=2,s=8,v=16;{}", zeros(8, 16))));
    e.feed(&apc("Ga=p,U=1,i=7,p=3,c=1,r=1,q=2"));
    // Underline color 3 names placement 3; 4 names none.
    e.feed("\x1b[38;5;7;58;5;3m\u{10EEEE}\u{0305}\u{0305}\x1b[m".as_bytes());
    assert_eq!(e.snapshot().images.len(), 1);
    e.feed("\r\n\x1b[38;5;7;58;5;4m\u{10EEEE}\u{0305}\u{0305}\x1b[m".as_bytes());
    assert_eq!(
        e.snapshot().images.len(),
        1,
        "no tile for a missing placement"
    );
    // Deleting the image's placements removes the virtual one too.
    e.feed(&apc("Ga=d,d=i,i=7"));
    assert!(e.snapshot().images.is_empty());
}

/// A display with an enormous `r` moves the cursor at most one screen
/// down (no unbounded line-feed buffer is built on the PTY thread).
#[test]
fn a_huge_row_count_moves_at_most_a_screen() {
    let mut e = engine();
    // 268 million rows would be a 268 MB line-feed buffer.
    e.feed(&apc("Ga=T,f=24,s=1,v=1,i=7,r=268000000;AAAA"));
    assert_eq!(e.replies(), vec!["\x1b_Gi=7;OK\x1b\\".to_string()]);
    assert_eq!(
        e.snapshot().cursor,
        Some((23, 0)),
        "at the bottom, past the edge"
    );
    e.feed(b"x");
    assert_eq!(e.snapshot().cursor, Some((23, 1)), "the terminal goes on");
}
