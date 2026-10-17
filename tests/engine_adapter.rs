//! N03 replay fixtures for the headless terminal adapter (`nora::engine`).
//!
//! Each fixture feeds raw PTY bytes into a fresh [`Engine`] and asserts the
//! exact resulting screen state, cursor, modes, captured events and reply
//! bytes. Chunk-invariance fixtures feed the same stream in different byte
//! chunks and require identical snapshots, proving parser state survives
//! arbitrary chunk boundaries.

use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

use nora::engine::{CellText, Damage, Engine, EventRecord, Snapshot};

/// Cell at (row, col) in a row-major snapshot.
fn cell(snap: &Snapshot, row: usize, col: usize) -> &CellText {
    &snap.cells[row * snap.cols + col]
}

/// One row's cells.
fn row_cells(snap: &Snapshot, row: usize) -> &[CellText] {
    let start = row * snap.cols;
    &snap.cells[start..start + snap.cols]
}

/// A row's characters (wide-char spacers contribute their stored char).
fn row_chars(snap: &Snapshot, row: usize) -> String {
    row_cells(snap, row).iter().map(|c| c.ch).collect()
}

#[test]
fn cursor_overwrite_preserves_other_cells() {
    let mut engine = Engine::new(4, 8);
    assert_eq!((engine.rows(), engine.cols()), (4, 8));
    engine.feed(b"aaaa");
    engine.feed(b"\x1b[2;1H"); // CUP: row 2, col 1 (1-based)
    engine.feed(b"bb");

    let snap = engine.snapshot();
    assert_eq!(snap.cursor, Some((1, 2)));
    assert_eq!(row_chars(&snap, 0), "aaaa    ");
    assert_eq!(row_chars(&snap, 1), "bb      ");
    assert_eq!(row_chars(&snap, 2), "        ");

    // Cells not written by the second feed keep their state.
    assert_eq!(cell(&snap, 0, 3).ch, 'a');
    assert_eq!(cell(&snap, 1, 0).ch, 'b');
    assert!(!cell(&snap, 1, 0).flags.contains(Flags::BOLD));
}

#[test]
fn pending_wrap_continues_on_next_row() {
    let mut engine = Engine::new(2, 8);
    engine.feed(b"aaaaaaaaa"); // 9 chars in 8 columns

    let snap = engine.snapshot();
    assert_eq!(snap.cursor, Some((1, 1)));
    assert_eq!(row_chars(&snap, 0), "aaaaaaaa");
    assert_eq!(row_chars(&snap, 1), "a       ");
}

#[test]
fn sgr_style_and_color_apply_until_reset() {
    let mut engine = Engine::new(4, 8);
    engine.feed(b"\x1b[1;31mred\x1b[0mX");

    let snap = engine.snapshot();
    assert_eq!(snap.cursor, Some((0, 4)));
    assert_eq!(row_chars(&snap, 0), "redX    ");

    for (rendered, expected) in snap.cells[0..3].iter().zip(['r', 'e', 'd']) {
        assert_eq!(rendered.ch, expected);
        assert!(rendered.flags.contains(Flags::BOLD));
        assert_eq!(rendered.fg, Color::Named(NamedColor::Red));
        assert_eq!(rendered.bg, Color::Named(NamedColor::Background));
    }

    let plain = &snap.cells[3];
    assert_eq!(plain.ch, 'X');
    assert!(!plain.flags.contains(Flags::BOLD));
    assert_eq!(plain.fg, Color::Named(NamedColor::Foreground));

    // A cell never written keeps the engine defaults (Cell::default).
    let blank = &snap.cells[4];
    assert_eq!(blank.ch, ' ');
    assert!(blank.flags.is_empty());
    assert_eq!(blank.fg, Color::Named(NamedColor::Foreground));
    assert_eq!(blank.bg, Color::Named(NamedColor::Background));
}

#[test]
fn alternate_screen_isolates_and_restores() {
    let mut engine = Engine::new(4, 8);
    engine.feed(b"primary");
    assert!(!engine.modes().contains(TermMode::ALT_SCREEN));
    assert_eq!(engine.cursor(), Some((0, 7)));
    assert_eq!(engine.replies(), Vec::<String>::new());

    engine.feed(b"\x1b[?1049h"); // DECSET 1049: switch to alt screen
    assert!(engine.modes().contains(TermMode::ALT_SCREEN));
    let snap = engine.snapshot();
    // The pinned engine preserves the live cursor position when entering
    // the alt screen (swap_alt copies the primary cursor before swapping),
    // and clears alt-screen contents.
    assert_eq!(snap.cursor, Some((0, 7)));
    assert!(snap.cells.iter().all(|c| c.ch == ' ' && c.flags.is_empty()));

    engine.feed(b"alt");
    let snap = engine.snapshot();
    // The preserved cursor sat on the last column, so 'a' fills it and the
    // remaining two characters wrap to the next row.
    assert_eq!(snap.cursor, Some((1, 2)));
    assert_eq!(row_chars(&snap, 0), "       a");
    assert_eq!(row_chars(&snap, 1), "lt      ");

    engine.feed(b"\x1b[?1049l"); // DECRST 1049: restore cursor, switch back
    assert!(!engine.modes().contains(TermMode::ALT_SCREEN));
    let snap = engine.snapshot();
    assert_eq!(snap.cursor, Some((0, 7)));
    assert_eq!(row_chars(&snap, 0), "primary ");
    assert_eq!(row_chars(&snap, 1), "        ");
}

#[test]
fn utf8_stream_is_chunk_boundary_invariant() {
    // 13 chars / 15 cells / 18 bytes: row 0 "hello 你" (你 spans cols 6-7),
    // row 1 " wor😀l" (😀 spans cols 4-5); cursor ends at (1, 7).
    const STREAM: &str = "hello 你 wor😀l";
    let bytes = STREAM.as_bytes();
    assert_eq!(bytes.len(), 18);

    let mut baseline = Engine::new(2, 8);
    baseline.feed(bytes);
    let expected = baseline.snapshot();

    // Explicit expected state for the single feed.
    assert_eq!(expected.cursor, Some((1, 7)));
    assert_eq!(expected.damage, Damage::Full);

    let row0 = row_cells(&expected, 0);
    assert_eq!(
        row0.iter().take(6).map(|c| c.ch).collect::<String>(),
        "hello "
    );
    assert_eq!(row0[6].ch, '你');
    assert!(row0[6].flags.contains(Flags::WIDE_CHAR));
    assert!(row0[7].flags.contains(Flags::WIDE_CHAR_SPACER));

    let row1 = row_cells(&expected, 1);
    assert_eq!(
        row1.iter().take(4).map(|c| c.ch).collect::<String>(),
        " wor"
    );
    assert_eq!(row1[4].ch, '😀');
    assert!(row1[4].flags.contains(Flags::WIDE_CHAR));
    assert!(row1[5].flags.contains(Flags::WIDE_CHAR_SPACER));
    assert_eq!(row1[6].ch, 'l');
    assert_eq!(row1[7].ch, ' ');

    // Splitting the stream at any byte boundary must not change the result.
    for split in 1..bytes.len() {
        let mut engine = Engine::new(2, 8);
        engine.feed(&bytes[..split]);
        engine.feed(&bytes[split..]);
        assert_eq!(engine.snapshot(), expected, "split at byte {split}");
    }
}

#[test]
fn escape_sequence_split_across_feeds_still_applies() {
    let mut engine = Engine::new(4, 8);
    engine.feed(b"\x1b[1;3");
    engine.feed(b"1m");
    engine.feed(b"X");
    engine.feed(b"\x1b[0");
    engine.feed(b"mY");

    let snap = engine.snapshot();
    assert_eq!(snap.cursor, Some((0, 2)));
    let x = cell(&snap, 0, 0);
    assert_eq!(x.ch, 'X');
    assert!(x.flags.contains(Flags::BOLD));
    assert_eq!(x.fg, Color::Named(NamedColor::Red));

    let y = cell(&snap, 0, 1);
    assert_eq!(y.ch, 'Y');
    assert!(!y.flags.contains(Flags::BOLD));
    assert_eq!(y.fg, Color::Named(NamedColor::Foreground));
}

#[test]
fn events_and_replies_are_captured_in_order() {
    let mut engine = Engine::new(4, 8);
    engine.feed(b"\x1b]2;hello\x07"); // OSC 2: window title
    engine.feed(b"\x1b[5n"); // DSR: cursor mode
    engine.feed(b"\x1b[6n"); // DSR: cursor position
    engine.feed(b"\x1b]2;world\x07"); // OSC 2: window title

    // N06b: titles coalesce to the latest before eviction, so the two
    // title feeds collapse to `Title("world")` in the first title's
    // position; the records keep their relative order.
    assert_eq!(
        engine.events(),
        vec![
            EventRecord::Title("world".to_string()),
            EventRecord::PtyWrite("\x1b[0n".to_string()),
            EventRecord::PtyWrite("\x1b[1;1R".to_string()),
        ]
    );
    assert_eq!(
        engine.replies(),
        vec!["\x1b[0n".to_string(), "\x1b[1;1R".to_string()]
    );

    engine.clear_events();
    assert_eq!(engine.events(), Vec::<EventRecord>::new());
    assert_eq!(engine.replies(), Vec::<String>::new());
}

#[test]
fn seeded_random_chunking_matches_single_feed() {
    // Styles, wide chars, CUP jumps and a DSR query in one stream.
    const STREAM: &str = "\x1b[1;32m你A€\x1b[2;1Hback\x1b[0m\x1b[1;1Htop\x1b[6n";
    let bytes = STREAM.as_bytes();

    let mut baseline = Engine::new(4, 12);
    baseline.feed(bytes);
    let expected = baseline.snapshot();
    let expected_replies = baseline.replies();

    // Explicit baseline checks.
    assert_eq!(expected.cursor, Some((0, 3)));
    assert_eq!(expected_replies, vec!["\x1b[1;4R".to_string()]);
    assert_eq!(row_chars(&expected, 0), "top€        ");

    let back = cell(&expected, 1, 0);
    assert_eq!(back.ch, 'b');
    assert!(back.flags.contains(Flags::BOLD));
    assert_eq!(back.fg, Color::Named(NamedColor::Green));

    let euro = cell(&expected, 0, 3);
    assert_eq!(euro.ch, '€');
    assert!(euro.flags.contains(Flags::BOLD));
    assert_eq!(euro.fg, Color::Named(NamedColor::Green));

    // "top" overwrote the wide 你: the cell must be a plain narrow char.
    let top = cell(&expected, 0, 0);
    assert_eq!(top.ch, 't');
    assert!(!top.flags.contains(Flags::BOLD));
    assert!(!top.flags.contains(Flags::WIDE_CHAR));
    assert_eq!(top.fg, Color::Named(NamedColor::Foreground));

    // 64 deterministic chunkings (sizes 1-5) must each match the single feed.
    let mut rng = SplitMix64(0x5EED);
    for iteration in 1..=64 {
        let mut engine = Engine::new(4, 12);
        let mut pos = 0;
        while pos < bytes.len() {
            let size = rng.chunk_size(5);
            let end = (pos + size).min(bytes.len());
            engine.feed(&bytes[pos..end]);
            pos = end;
        }
        assert_eq!(
            engine.snapshot(),
            expected,
            "seeded chunking iteration {iteration}"
        );
        assert_eq!(
            engine.replies(),
            expected_replies,
            "seeded chunking iteration {iteration}"
        );
    }
}

/// N06a (fixture H8): the raw color-table read must reflect exactly what
/// the parser stored — no seeding, no defaults (those are applied at
/// resolution time by the session layer).
#[test]
fn color_accessor_reflects_table() {
    let mut engine = Engine::new(4, 8);
    // The pinned table starts all-unset: no seeding anywhere.
    assert_eq!(engine.color(256), None, "a fresh table must be all-unset");

    // OSC 10 sets the foreground slot (index 256); `rgb:12/34/56` scales
    // to Rgb { 0x12, 0x34, 0x56 }.
    engine.feed(b"\x1b]10;rgb:12/34/56\x07");
    assert_eq!(
        engine.color(256),
        Some(Rgb {
            r: 0x12,
            g: 0x34,
            b: 0x56
        })
    );

    // A palette slot left unset stays unset.
    assert_eq!(engine.color(0), None);
}

/// Deterministic PRNG so the chunking sequence is reproducible.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Random chunk size in 1..=max.
    fn chunk_size(&mut self, max: usize) -> usize {
        (self.next() % max as u64 + 1) as usize
    }
}

/// N06b: the pinned term's config sets `Osc52::Disabled`, so an OSC 52
/// store is denied before decoding — zero records, zero replies. (The
/// pre-flip default `Osc52::OnlyCopy` recorded exactly one
/// `ClipboardStore` for this stream; the precedent fixture is
/// `events_and_replies_are_captured_in_order`.)
#[test]
fn osc52_store_is_denied_when_disabled() {
    let mut engine = Engine::new(24, 80);
    engine.feed(b"\x1b]52;c;aGVsbG8=\x1b\\"); // OSC 52 store, ST-terminated
    assert_eq!(
        engine.drain_events(),
        Vec::<EventRecord>::new(),
        "a denied OSC 52 store must record nothing"
    );
    assert_eq!(engine.replies(), Vec::<String>::new());
}

/// N06b: an OSC 52 load query (`\x1b]52;c?\x07` → pinned
/// `Term::clipboard_load`, vte-0.15.0 `ansi.rs:1483-1491`) is denied the
/// same way — no `ClipboardLoad` record, no reply bytes.
#[test]
fn osc52_load_query_is_denied_when_disabled() {
    let mut engine = Engine::new(24, 80);
    engine.feed(b"\x1b]52;c?\x07"); // OSC 52 load query for the clipboard
    assert_eq!(
        engine.drain_events(),
        Vec::<EventRecord>::new(),
        "a denied OSC 52 load must record nothing"
    );
    assert_eq!(engine.replies(), Vec::<String>::new());
}

/// Redraw and mouse-cursor notifications coalesce: a burst of them leaves
/// one pending record each, after the reply that came first.
#[test]
fn wakeups_coalesce_and_keep_pending_replies() {
    use alacritty_terminal::event::{Event, EventListener};
    let mut engine = Engine::new(4, 8);
    engine.feed(b"\x1b[6n");
    let recorder = engine.recorder();
    for _ in 0..1000 {
        recorder.send_event(Event::Wakeup);
        recorder.send_event(Event::MouseCursorDirty);
    }
    assert_eq!(
        engine.events(),
        vec![
            EventRecord::PtyWrite("\x1b[1;1R".to_string()),
            EventRecord::Wakeup,
            EventRecord::MouseCursorDirty,
        ]
    );
}

/// Presentation records over the cap are evicted before any pending reply.
#[test]
fn eviction_spares_pending_replies() {
    use alacritty_terminal::event::{Event, EventListener};
    let mut engine = Engine::new(4, 8);
    engine.feed(b"\x1b[6n");
    let recorder = engine.recorder();
    for _ in 0..1000 {
        recorder.send_event(Event::ResetTitle);
    }
    engine.feed(b"\x1b[5n");
    assert_eq!(engine.events().len(), 256);
    assert_eq!(
        engine.replies(),
        vec!["\x1b[1;1R".to_string(), "\x1b[0n".to_string()]
    );
}

/// A reply flood stays bounded: the newest replies are kept.
#[test]
fn reply_flood_stays_bounded() {
    let mut engine = Engine::new(4, 8);
    for _ in 0..1000 {
        engine.feed(b"\x1b[5n");
    }
    engine.feed(b"\x1b[6n");
    let replies = engine.replies();
    assert_eq!(replies.len(), 256);
    assert_eq!(replies.last().map(String::as_str), Some("\x1b[1;1R"));
}
