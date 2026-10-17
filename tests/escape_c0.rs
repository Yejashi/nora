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

#[test]
fn a_c0_control_after_esc_still_starts_an_osc() {
    let mut filter = PromptMarkerFilter::new();
    let (out, events) = filter.process(b"\x1b\x00]133;A\x07x");
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(
        &events[0].1,
        ShellEvent::Marker(m) if m.kind == MarkerKind::A
    ));
    // The control is still executed; the marker frame is suppressed.
    assert_eq!(out, b"\x00x");
    let (out, events) = filter.process(b"\x1b\x18]133;A\x07");
    assert!(events.is_empty(), "CAN ends the escape");
    assert_eq!(out, b"\x1b\x18]133;A\x07");
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
