//! C0 controls inside escape sequences, as vte 0.15 handles them: CAN and
//! SUB abort a sequence; other C0 controls are executed without leaving it.
//! The byte filters that run beside the parser must agree.

use nora::engine::Engine;
use nora::graphics_filter::{GraphicsFilter, GraphicsSignal};
use nora::prompt_marker::{MarkerKind, PromptMarkerFilter, ShellEvent};

fn graphics_signals(bytes: &[u8]) -> Vec<GraphicsSignal> {
    let (_, signals) = GraphicsFilter::new().process(bytes);
    signals.into_iter().map(|(_, s)| s).collect()
}

#[test]
fn can_and_sub_abort_a_csi() {
    assert!(graphics_signals(b"\x1b[\x182J").is_empty(), "CAN");
    assert!(graphics_signals(b"\x1b[2\x1a3J").is_empty(), "SUB");
    assert!(graphics_signals(b"\x1b\x18[2J").is_empty(), "CAN after ESC");
}

#[test]
fn other_c0_controls_stay_in_the_sequence() {
    assert_eq!(
        graphics_signals(b"\x1b\x00[2J"),
        vec![GraphicsSignal::ClearScreen]
    );
    assert_eq!(
        graphics_signals(b"\x1b\x07[\n3J"),
        vec![GraphicsSignal::ClearScrollback]
    );
    assert_eq!(graphics_signals(b"\x1b\x0dc"), vec![GraphicsSignal::Reset]);
    // DEL and 8-bit bytes are ignored inside an escape or a CSI.
    assert_eq!(
        graphics_signals(b"\x1b\x7f[\x802J"),
        vec![GraphicsSignal::ClearScreen]
    );
}

/// The prompt filter forwards a held ESC and a C0 control after it in
/// their order (it does not track DCS/SOS/PM/APC strings, inside which
/// the order matters), so `ESC <C0> ] 133 …` is left to the parser and
/// is not a marker (a known, accepted limit).
#[test]
fn a_c0_control_after_esc_is_forwarded_in_order() {
    let mut filter = PromptMarkerFilter::new();
    let (out, events) = filter.process(b"\x1b\x00]133;A\x07x");
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(out, b"\x1b\x00]133;A\x07x");
    // The filter is back in ground: the next marker is recognised.
    let (out, events) = filter.process(b"\x1b]133;A\x07y");
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(
        &events[0].1,
        ShellEvent::Marker(m) if m.kind == MarkerKind::A
    ));
    assert_eq!(out, b"y");
    let (out, events) = filter.process(b"\x1b\x18]133;A\x07");
    assert!(events.is_empty(), "CAN ends the escape");
    assert_eq!(out, b"\x1b\x18]133;A\x07");
    // vte ignores DEL and 8-bit bytes inside an escape: still an OSC.
    let mut filter = PromptMarkerFilter::new();
    let (out, events) = filter.process(b"\x1b\x7f\xc3]133;B\x07y");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(out, b"y");
}

/// The parser agrees: an image survives `ESC [ CAN 2 J` and is removed by
/// `ESC NUL [ 2 J`.
#[test]
fn the_parser_and_the_graphics_filter_agree() {
    let mut e = Engine::with_history(24, 80, 100);
    e.feed(b"\x1b_Ga=T,C=1,f=24,s=1,v=1;AAAA\x1b\\");
    assert_eq!(e.snapshot().images.len(), 1);
    e.feed(b"\x1b[\x182J");
    assert_eq!(e.snapshot().images.len(), 1, "CAN aborted the clear");
    e.feed(b"\x1b\x00[2J");
    assert!(e.snapshot().images.is_empty(), "the clear ran");
}

/// An APC interrupted by an ESC, followed by CR LF: the ESC ends the APC
/// in vte, so the CR LF after it are executed and the cursor moves to the
/// start of the next line (the prompt filter used to forward them before
/// the ESC, inside the APC, where vte dropped them). The escape stays
/// open, as in vte: the next printable byte completes it, and text after
/// that is printed on the new line.
#[test]
fn controls_after_an_interrupted_apc_are_executed() {
    let stream = b"out\x1b_Gq=2\x1b\r\n";
    let mut e = Engine::with_history(24, 80, 100);
    e.feed(stream);
    assert_eq!(e.cursor(), Some((1, 0)));
    e.feed(b"\\$ ");
    assert_eq!(e.cursor(), Some((1, 2)));
    let text: String = e.snapshot().cells.iter().map(|c| c.ch).collect();
    assert!(text[80..].starts_with("$ "), "{:?}", &text[..90]);
    // Also when the bytes arrive one read at a time.
    let mut e = Engine::with_history(24, 80, 100);
    for byte in stream {
        e.feed(&[*byte]);
    }
    assert_eq!(e.cursor(), Some((1, 0)));
}
