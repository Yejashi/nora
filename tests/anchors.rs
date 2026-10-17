//! Stable line anchors (prompt markers) across vertical resizes.
//!
//! A marker records the cursor line and the grid's `total_scrolled` /
//! `total_pulled` counters; its current line is
//! `line - Δscrolled + Δpulled`. These tests check that the anchor stays
//! on the row whose content it marked when the viewport grows taller,
//! with and without history to pull, on both screens.

use nora::engine::Engine;

const OSC_133_A: &[u8] = b"\x1b]133;A\x07";

/// The current line of every queued marker, computed from the grid's
/// counters now.
fn marker_lines(engine: &mut Engine) -> Vec<i32> {
    let (scrolled, pulled) = {
        let term = engine.term_handle();
        let term = term.lock();
        (term.grid().total_scrolled(), term.grid().total_pulled())
    };
    engine
        .drain_markers()
        .iter()
        .map(|m| m.current_line(scrolled, pulled).expect("in range"))
        .collect()
}

/// The row whose text starts with `prefix`, as a grid line.
fn row_of(engine: &mut Engine, prefix: &str) -> Option<i32> {
    let snapshot = engine.snapshot();
    (0..snapshot.rows)
        .find(|&row| {
            let text: String = (0..snapshot.cols)
                .map(|col| snapshot.cells[row * snapshot.cols + col].ch)
                .collect();
            text.starts_with(prefix)
        })
        .map(|row| row as i32)
}

/// No history: growing 24 -> 50 rows adds blank rows at the bottom and
/// moves no content, so a marker at line 0 stays at line 0.
#[test]
fn grow_without_history_keeps_marker_line() {
    let mut engine = Engine::with_history(24, 80, 0);
    engine.feed(OSC_133_A);
    engine.feed(b"$ prompt");
    engine.resize(50, 80);
    assert_eq!(marker_lines(&mut engine), vec![0]);
    assert_eq!(row_of(&mut engine, "$ prompt"), Some(0));
}

/// Some history: 30 lines on a 24-row screen leave 7 in history. Growing
/// to 50 pulls those 7 back (content moves down 7) and adds 19 blank rows
/// below; the marker follows its row.
#[test]
fn grow_with_partial_history_follows_content() {
    let mut engine = Engine::with_history(24, 80, 100);
    for i in 0..30 {
        engine.feed(format!("line {i}\r\n").as_bytes());
    }
    engine.feed(OSC_133_A);
    engine.feed(b"$ prompt");
    let before = row_of(&mut engine, "$ prompt").expect("prompt row");
    assert_eq!(before, 23);
    engine.resize(50, 80);
    let after = row_of(&mut engine, "$ prompt").expect("prompt row");
    assert_eq!(after, 30);
    assert_eq!(marker_lines(&mut engine), vec![after]);
}

/// The alternate screen has no history: growing it moves nothing, so a
/// marker placed there stays on its row.
#[test]
fn grow_on_alt_screen_keeps_marker_line() {
    let mut engine = Engine::with_history(24, 80, 100);
    engine.feed(b"\x1b[?1049h\x1b[6;1H");
    engine.feed(OSC_133_A);
    engine.feed(b"$ alt");
    engine.resize(50, 80);
    assert_eq!(row_of(&mut engine, "$ alt"), Some(5));
    assert_eq!(marker_lines(&mut engine), vec![5]);
}

/// Repeated grow/shrink cycles never let the anchor drift.
#[test]
fn grow_shrink_cycles_do_not_drift() {
    let mut engine = Engine::with_history(24, 80, 0);
    engine.feed(OSC_133_A);
    engine.feed(b"$ prompt");
    for _ in 0..3 {
        engine.resize(50, 80);
        engine.resize(24, 80);
    }
    assert_eq!(row_of(&mut engine, "$ prompt"), Some(0));
    assert_eq!(marker_lines(&mut engine), vec![0]);
}

// Each screen has its own grid counters, so an anchor belongs to the
// screen it was recorded on; the alternate screen's anchors end with its
// session.

const ENTER_ALT: &[u8] = b"\x1b[?1049h";
const LEAVE_ALT: &[u8] = b"\x1b[?1049l";

fn link(uri: &str, text: &str) -> Vec<u8> {
    format!("\x1b]8;;{uri}\x1b\\{text}\x1b]8;;\x1b\\").into_bytes()
}

/// The viewport text, one string per row (trailing blanks dropped).
fn rows(engine: &mut Engine) -> Vec<String> {
    let snapshot = engine.snapshot();
    (0..snapshot.rows)
        .map(|row| {
            let text: String = (0..snapshot.cols)
                .map(|col| snapshot.cells[row * snapshot.cols + col].ch)
                .collect();
            text.trim_end().to_string()
        })
        .collect()
}

/// A link `less` showed is not found on the main screen's text at its
/// position after it exits, nor in the next alternate screen session.
#[test]
fn alt_screen_hyperlink_is_not_found_after_its_session() {
    let mut engine = Engine::with_history(8, 40, 100);
    engine.feed(b"one\r\ntwo\r\nunrelated main text\r\n");
    engine.feed(ENTER_ALT);
    engine.feed(b"\x1b[3;1H");
    engine.feed(&link("https://alt.example", "alt link"));
    engine.drain_hyperlinks();
    assert_eq!(
        engine.hyperlink_at(2, 3).as_deref(),
        Some("https://alt.example"),
        "found while its screen is shown"
    );
    engine.feed(LEAVE_ALT);
    engine.drain_hyperlinks();
    assert_eq!(engine.hyperlink_at(2, 3), None, "main screen");
    engine.feed(ENTER_ALT);
    engine.feed(b"\x1b[3;1Hvim text");
    engine.drain_hyperlinks();
    assert_eq!(engine.hyperlink_at(2, 3), None, "a new alternate screen");
}

/// A frame of the main screen ends the alternate screen's session, as it
/// does for its images, without a drain in between.
#[test]
fn a_main_screen_frame_drops_alt_screen_hyperlinks() {
    let mut engine = Engine::with_history(8, 40, 100);
    engine.feed(ENTER_ALT);
    engine.feed(b"\x1b[3;1H");
    engine.feed(&link("https://stored.example", "stored"));
    engine.drain_hyperlinks();
    engine.feed(b"\x1b[5;1H");
    engine.feed(&link("https://queued.example", "queued"));
    engine.feed(LEAVE_ALT);
    let _ = engine.snapshot();
    engine.feed(ENTER_ALT);
    engine.drain_hyperlinks();
    assert_eq!(engine.hyperlink_count(), 0);
    assert_eq!(engine.hyperlink_at(2, 1), None);
    assert_eq!(engine.hyperlink_at(4, 1), None);
}

/// The shell's prompt printed between two alternate screen sessions ends
/// the first one's queued entries.
#[test]
fn a_main_screen_prompt_drops_queued_alt_screen_entries() {
    let mut engine = Engine::with_history(8, 40, 100);
    engine.feed(ENTER_ALT);
    engine.feed(b"\x1b[3;1H");
    engine.feed(OSC_133_A);
    engine.feed(&link("https://alt.example", "alt link"));
    engine.feed(LEAVE_ALT);
    engine.feed(OSC_133_A);
    engine.feed(b"$ ");
    engine.feed(ENTER_ALT);
    engine.drain_hyperlinks();
    assert_eq!(engine.hyperlink_at(2, 3), None);
    let markers = engine.drain_markers();
    assert_eq!(markers.len(), 1, "only the main screen's prompt");
    assert!(!markers[0].alt);
}

/// A main-screen link is hidden while the alternate screen is shown and
/// found on its text again afterwards.
#[test]
fn main_screen_hyperlink_is_found_again_after_the_alt_screen() {
    let mut engine = Engine::with_history(8, 40, 100);
    engine.feed(b"see ");
    engine.feed(&link("https://main.example", "main link"));
    engine.feed(b"\r\n");
    engine.drain_hyperlinks();
    engine.feed(ENTER_ALT);
    engine.feed(b"alt screen text");
    engine.drain_hyperlinks();
    assert_eq!(
        engine.hyperlink_at(0, 5),
        None,
        "not on the alternate screen"
    );
    engine.feed(LEAVE_ALT);
    let _ = engine.snapshot();
    engine.drain_hyperlinks();
    assert_eq!(
        engine.hyperlink_at(0, 5).as_deref(),
        Some("https://main.example")
    );
}

/// Prompt and command markers an application wrote on the alternate
/// screen are not prompt-jump or command-selection targets on the main
/// screen. Aged with the main screen's counters, the alternate screen's
/// top line would land in the main screen's history, above the view.
#[test]
fn alt_screen_prompts_are_not_main_screen_targets() {
    let mut engine = Engine::with_history(5, 40, 100);
    engine.feed(b"\x1b]133;A\x07$ one\r\n\x1b]133;C\x07");
    for i in 0..12 {
        engine.feed(format!("out {i}\r\n").as_bytes());
    }
    engine.feed(b"\x1b]133;D\x07\x1b]133;A\x07$ ");
    engine.drain_markers();
    engine.feed(ENTER_ALT);
    engine.feed(b"\x1b[1;1H\x1b]133;A\x07alt prompt\r\n\x1b]133;C\x07alt out\r\n\x1b]133;D\x07");
    engine.drain_markers();
    engine.feed(LEAVE_ALT);
    engine.drain_markers();
    assert!(engine.jump_to_previous_prompt().is_some());
    assert_eq!(rows(&mut engine)[0], "$ one");
    assert!(engine.jump_to_previous_prompt().is_none(), "the only one");
    // The main screen's command, scrolled to its end (the selection is
    // the view's rows).
    assert!(engine.select_previous_command().is_some());
    let copied = engine.copy_selection().expect("selected");
    assert!(copied.ends_with("out 10\nout 11"), "{copied:?}");
    assert!(!copied.contains("alt"), "{copied:?}");
}

/// Leaving and re-entering the alternate screen in one write (`less`
/// exits and `vim` starts within one pump tick) ends the first session,
/// with no frame, drain or main-screen shell event in between.
#[test]
fn reentering_the_alt_screen_in_one_write_ends_its_session() {
    let mut engine = Engine::with_history(8, 40, 100);
    let mut bytes = ENTER_ALT.to_vec();
    bytes.extend_from_slice(b"\x1b[3;1H");
    bytes.extend_from_slice(OSC_133_A);
    bytes.extend(link("https://first.example", "first"));
    bytes.extend_from_slice(LEAVE_ALT);
    bytes.extend_from_slice(ENTER_ALT);
    bytes.extend_from_slice(b"\x1b[5;1H");
    bytes.extend(link("https://second.example", "second"));
    engine.feed(&bytes);
    assert!(
        engine.drain_markers().is_empty(),
        "the first session's prompt"
    );
    let links = engine.drain_hyperlinks();
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0].uri, "https://second.example");
    assert_eq!(engine.hyperlink_count(), 1);
    assert_eq!(engine.hyperlink_at(2, 1), None, "the first session's link");
    assert_eq!(
        engine.hyperlink_at(4, 1).as_deref(),
        Some("https://second.example")
    );
}

/// A reset (`ESC c`) leaves the alternate screen without `swap_alt`; the
/// next entry is still a new session.
#[test]
fn reentering_the_alt_screen_after_a_reset_ends_its_session() {
    let mut engine = Engine::with_history(8, 40, 100);
    let mut bytes = ENTER_ALT.to_vec();
    bytes.extend_from_slice(b"\x1b[3;1H");
    bytes.extend(link("https://first.example", "first"));
    bytes.extend_from_slice(b"\x1bc");
    bytes.extend_from_slice(ENTER_ALT);
    engine.feed(&bytes);
    assert!(engine.drain_hyperlinks().is_empty());
    assert_eq!(engine.hyperlink_at(2, 1), None);
}

/// Entries already stored from an alternate screen session are absent in
/// the next one from its first byte, and dropped at the next drain.
#[test]
fn stored_entries_of_an_earlier_alt_session_are_absent() {
    let mut engine = Engine::with_history(8, 40, 100);
    engine.feed(ENTER_ALT);
    engine.feed(b"\x1b[3;1H");
    engine.feed(&link("https://first.example", "first"));
    engine.feed(b"\r\n\x1b]133;A\x07$ cmd\r\n\x1b]133;C\x07out\r\n\x1b]133;D\x07");
    engine.drain_markers();
    engine.drain_hyperlinks();
    assert_eq!(
        engine.hyperlink_at(2, 1).as_deref(),
        Some("https://first.example")
    );
    assert!(engine.select_previous_command().is_some());
    let mut bytes = LEAVE_ALT.to_vec();
    bytes.extend_from_slice(ENTER_ALT);
    bytes.extend_from_slice(b"\x1b[3;1Hvim text");
    engine.feed(&bytes);
    assert_eq!(engine.hyperlink_at(2, 1), None, "a new alternate screen");
    assert!(
        engine.select_previous_command().is_none(),
        "the first session's command"
    );
    assert_eq!(engine.hyperlink_count(), 1, "still stored before a drain");
    engine.drain_hyperlinks();
    engine.drain_markers();
    assert_eq!(engine.hyperlink_count(), 0, "dropped at the next drain");
}
