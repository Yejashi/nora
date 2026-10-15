//! N16a regression suite for the pure mouse encoder
//! (`nora::input::mouse::encode_mouse_event`) and the focus encoder
//! (`nora::input::focus::encode_focus_event`).
//!
//! Every test is headless: plain event fields (no winit `WindowEvent`),
//! no PTY, no display, no clocks.
//!
//! Pinned byte table (verified against `src/input/mouse.rs`):
//! - format: `CSI < b ; x y M` (press/motion/wheel) / `CSI < b ; x y m`
//!   (release), for every reporting mode;
//! - button codes: 0/1/2 = left/middle/right, motion = 32 + held button
//!   (35 when no button is held), wheel 64/65/66/67 = up/down/left/right;
//! - modifiers: shift +4, ctrl +8, alt +64;
//! - coordinates: `col + 1` / `row + 1` (1-based), clamped to the last
//!   valid cell when the event sits on the grid edge, `None` beyond the
//!   grid edge or at a coordinate of 256 or more;
//! - gating: `X10` reports no motion; `Normal` reports motion only while
//!   a button is held; `CellMotion`/`AllMotion` always report motion;
//!   press/release and wheel are reported by every reporting mode;
//! - focus: `CSI I` on focus in, `CSI O` on focus out, `None` when focus
//!   reporting is disabled.

use alacritty_terminal::term::TermMode;
use nora::input::focus::encode_focus_event;
use nora::input::mouse::{
    MouseEncoding, MouseEvent, MouseKind, MouseMode, MouseModifiers, button_number,
    encode_mouse_event, logical_to_cell, mouse_modifiers, physical_to_cell, should_report,
    track_button, wheel_directions,
};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta};
use winit::keyboard::ModifiersState;

/// The grid every coordinate test is encoded against: 80x24.
const COLS: u16 = 80;
const ROWS: u16 = 24;

/// A plain event (no modifiers) of the given kind at the given cell.
fn event(kind: MouseKind, button: Option<u8>, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        button,
        col,
        row,
        modifiers: MouseModifiers::default(),
    }
}

fn sgr(mode: MouseMode, e: &MouseEvent) -> Option<String> {
    encode_mouse_event(mode, MouseEncoding::Sgr, e, COLS, ROWS)
        .map(|b| String::from_utf8(b).expect("SGR reports are ASCII"))
}

fn legacy(mode: MouseMode, e: &MouseEvent) -> Option<Vec<u8>> {
    encode_mouse_event(mode, MouseEncoding::Default, e, COLS, ROWS)
}

/// SGR (1006): `CSI < b ; x ; y M`, release `m` with the button kept,
/// 1-based coordinates (xterm ctlseqs, "SGR (1006)").
#[test]
fn sgr_press_release_left_middle_right() {
    for (button, code) in [(0u8, 0u16), (1, 1), (2, 2)] {
        assert_eq!(
            sgr(
                MouseMode::Normal,
                &event(MouseKind::Press, Some(button), 4, 9)
            ),
            Some(format!("\x1b[<{code};5;10M"))
        );
        assert_eq!(
            sgr(
                MouseMode::Normal,
                &event(MouseKind::Release, Some(button), 4, 9)
            ),
            Some(format!("\x1b[<{code};5;10m"))
        );
    }
}

/// Legacy (default) encoding: `CSI M Cb Cx Cy`, each value + 32; every
/// release reports button 3.
#[test]
fn legacy_press_and_release() {
    assert_eq!(
        legacy(MouseMode::Normal, &event(MouseKind::Press, Some(0), 0, 0)),
        Some(vec![0x1b, b'[', b'M', 32, 33, 33])
    );
    assert_eq!(
        legacy(MouseMode::Normal, &event(MouseKind::Release, Some(2), 9, 4)),
        Some(vec![0x1b, b'[', b'M', 32 + 3, 32 + 10, 32 + 5])
    );
}

/// Legacy coordinates stop at 223 (one byte); SGR has no limit; UTF-8
/// (1005) extends to 2015.
#[test]
fn encodings_and_their_coordinate_limits() {
    let wide = 1000;
    let e = event(MouseKind::Press, Some(0), 300, 0);
    assert_eq!(
        encode_mouse_event(MouseMode::Normal, MouseEncoding::Default, &e, wide, ROWS),
        None,
        "column 301 is not representable in one byte"
    );
    assert_eq!(
        encode_mouse_event(MouseMode::Normal, MouseEncoding::Sgr, &e, wide, ROWS),
        Some(b"\x1b[<0;301;1M".to_vec())
    );
    let utf8 = encode_mouse_event(MouseMode::Normal, MouseEncoding::Utf8, &e, wide, ROWS)
        .expect("column 301 fits the UTF-8 encoding");
    let text = String::from_utf8(utf8).expect("UTF-8 encoding is valid UTF-8");
    let values: Vec<u32> = text.chars().skip(3).map(u32::from).collect();
    assert_eq!(values, vec![32, 301 + 32, 1 + 32]);
}

#[test]
fn encoding_follows_the_term_modes() {
    assert_eq!(
        MouseEncoding::from_term_mode(TermMode::empty()),
        MouseEncoding::Default
    );
    assert_eq!(
        MouseEncoding::from_term_mode(TermMode::UTF8_MOUSE),
        MouseEncoding::Utf8
    );
    assert_eq!(
        MouseEncoding::from_term_mode(TermMode::SGR_MOUSE | TermMode::UTF8_MOUSE),
        MouseEncoding::Sgr
    );
}

/// 1000 (Normal): press, release, wheel; never motion.
#[test]
fn normal_mode_reports_clicks_and_wheel_but_no_motion() {
    assert!(sgr(MouseMode::Normal, &event(MouseKind::WheelUp, None, 0, 0)).is_some());
    assert_eq!(
        sgr(MouseMode::Normal, &event(MouseKind::Motion, Some(0), 1, 1)),
        None
    );
    assert_eq!(
        sgr(MouseMode::Normal, &event(MouseKind::Motion, None, 1, 1)),
        None
    );
}

/// 1002 (button-event): motion only while a button is held; motion adds
/// 32 to the button code.
#[test]
fn cellmotion_reports_drag_only() {
    assert_eq!(
        sgr(
            MouseMode::CellMotion,
            &event(MouseKind::Motion, Some(0), 2, 3)
        ),
        Some("\x1b[<32;3;4M".to_string())
    );
    assert_eq!(
        sgr(MouseMode::CellMotion, &event(MouseKind::Motion, None, 2, 3)),
        None
    );
}

/// 1003 (any-event): all motion; no button held reports button 3 + 32.
#[test]
fn allmotion_reports_hover() {
    assert_eq!(
        sgr(MouseMode::AllMotion, &event(MouseKind::Motion, None, 2, 3)),
        Some("\x1b[<35;3;4M".to_string())
    );
}

/// X10 (9): presses only.
#[test]
fn x10_reports_presses_only() {
    assert!(sgr(MouseMode::X10, &event(MouseKind::Press, Some(0), 0, 0)).is_some());
    assert_eq!(
        sgr(MouseMode::X10, &event(MouseKind::Release, Some(0), 0, 0)),
        None
    );
    assert_eq!(
        sgr(MouseMode::X10, &event(MouseKind::WheelUp, None, 0, 0)),
        None
    );
}

#[test]
fn none_mode_reports_nothing() {
    for kind in [
        MouseKind::Press,
        MouseKind::Release,
        MouseKind::Motion,
        MouseKind::WheelDown,
    ] {
        assert_eq!(sgr(MouseMode::None, &event(kind, Some(0), 0, 0)), None);
    }
}

/// DECSET 1000/1002/1003 map to Normal/CellMotion/AllMotion; the most
/// inclusive set mode wins.
#[test]
fn from_term_mode_maps_engine_flags() {
    assert_eq!(
        MouseMode::from_term_mode(TermMode::empty()),
        MouseMode::None
    );
    assert_eq!(
        MouseMode::from_term_mode(TermMode::MOUSE_REPORT_CLICK),
        MouseMode::Normal
    );
    assert_eq!(
        MouseMode::from_term_mode(TermMode::MOUSE_DRAG),
        MouseMode::CellMotion
    );
    assert_eq!(
        MouseMode::from_term_mode(TermMode::MOUSE_MOTION),
        MouseMode::AllMotion
    );
    assert_eq!(
        MouseMode::from_term_mode(TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG),
        MouseMode::CellMotion
    );
}

/// Wheel buttons 64-67; extra buttons 8/9 report 128/129.
#[test]
fn wheel_and_extra_button_codes() {
    let codes = [
        (MouseKind::WheelUp, 64),
        (MouseKind::WheelDown, 65),
        (MouseKind::WheelLeft, 66),
        (MouseKind::WheelRight, 67),
    ];
    for (kind, code) in codes {
        assert_eq!(
            sgr(MouseMode::Normal, &event(kind, None, 0, 0)),
            Some(format!("\x1b[<{code};1;1M"))
        );
    }
    assert_eq!(
        sgr(MouseMode::Normal, &event(MouseKind::Press, Some(8), 0, 0)),
        Some("\x1b[<128;1;1M".to_string())
    );
    assert_eq!(
        sgr(MouseMode::Normal, &event(MouseKind::Press, Some(9), 0, 0)),
        Some("\x1b[<129;1;1M".to_string())
    );
}

/// Modifier bits: Shift +4, Meta/Alt +8, Ctrl +16 (xterm ctlseqs).
#[test]
fn modifiers_add_to_the_button_code() {
    let with = |shift, alt, ctrl| MouseEvent {
        modifiers: MouseModifiers { shift, ctrl, alt },
        ..event(MouseKind::Press, Some(0), 0, 0)
    };
    for (e, code) in [
        (with(true, false, false), 4),
        (with(false, true, false), 8),
        (with(false, false, true), 16),
        (with(true, true, true), 28),
    ] {
        assert_eq!(
            sgr(MouseMode::Normal, &e),
            Some(format!("\x1b[<{code};1;1M"))
        );
    }
}

/// Positions outside the grid and empty grids report nothing.
#[test]
fn out_of_grid_and_empty_grid_report_nothing() {
    assert_eq!(
        sgr(
            MouseMode::Normal,
            &event(MouseKind::Press, Some(0), COLS, 0)
        ),
        None
    );
    assert_eq!(
        sgr(
            MouseMode::Normal,
            &event(MouseKind::Press, Some(0), 0, ROWS)
        ),
        None
    );
    assert_eq!(
        encode_mouse_event(
            MouseMode::Normal,
            MouseEncoding::Sgr,
            &event(MouseKind::Press, Some(0), 0, 0),
            0,
            0
        ),
        None
    );
    assert_eq!(
        sgr(
            MouseMode::Normal,
            &event(MouseKind::Press, Some(0), COLS - 1, ROWS - 1)
        ),
        Some(format!("\x1b[<0;{COLS};{ROWS}M"))
    );
}

#[test]
fn focus_reports_when_enabled() {
    assert_eq!(encode_focus_event(true, true), Some("\x1b[I".to_string()));
    assert_eq!(encode_focus_event(true, false), Some("\x1b[O".to_string()));
}

#[test]
fn focus_reports_nothing_when_disabled() {
    assert_eq!(encode_focus_event(false, true), None);
    assert_eq!(encode_focus_event(false, false), None);
}

// ========================================================== N16b wiring ===
//
// Headless rows for the pure window-side decisions the N16b wiring in
// `src/app/mod.rs` makes per event: coordinate conversion, button
// tracking, modifier mapping, the report-vs-local routing decision and
// the wheel direction mapping.

/// The pinned mono cell metrics in logical pixels (N13).
const CELL_W: u32 = 10;
const CELL_H: u32 = 20;

// --------------------------------------------------------- coordinates ---

#[test]
fn logical_to_cell_truncates_to_the_grid() {
    assert_eq!(logical_to_cell(0.0, 0.0, CELL_W, CELL_H), Some((0, 0)));
    assert_eq!(logical_to_cell(9.9, 19.9, CELL_W, CELL_H), Some((0, 0)));
    assert_eq!(logical_to_cell(10.0, 20.0, CELL_W, CELL_H), Some((1, 1)));
    assert_eq!(logical_to_cell(123.0, 61.0, CELL_W, CELL_H), Some((12, 3)));
}

#[test]
fn logical_to_cell_rejects_degenerate_metrics() {
    assert_eq!(logical_to_cell(5.0, 5.0, 0, CELL_H), None);
    assert_eq!(logical_to_cell(5.0, 5.0, CELL_W, 0), None);
}

#[test]
fn logical_to_cell_saturates_negative_positions() {
    // A cursor above/left of the origin maps to cell (0, 0).
    assert_eq!(logical_to_cell(-1.0, -1.0, CELL_W, CELL_H), Some((0, 0)));
}

#[test]
fn physical_to_cell_divides_by_the_scale_factor() {
    // Scale 2.0: physical (25, 45) = logical (12.5, 22.5) = cell (1, 1).
    assert_eq!(
        physical_to_cell(25.0, 45.0, 2.0, CELL_W, CELL_H),
        Some((1, 1))
    );
    // Scale 1.0: identical to the logical mapping.
    assert_eq!(
        physical_to_cell(123.0, 61.0, 1.0, CELL_W, CELL_H),
        Some((12, 3))
    );
    // Scale 0.5: physical (10, 10) = logical (20, 20) = cell (2, 1).
    assert_eq!(
        physical_to_cell(10.0, 10.0, 0.5, CELL_W, CELL_H),
        Some((2, 1))
    );
}

#[test]
fn physical_to_cell_rejects_non_positive_scale() {
    assert_eq!(physical_to_cell(5.0, 5.0, 0.0, CELL_W, CELL_H), None);
    assert_eq!(physical_to_cell(5.0, 5.0, -1.0, CELL_W, CELL_H), None);
}

// ------------------------------------------------------ button tracking ---

#[test]
fn track_button_press_release_transitions() {
    let mut pressed = Vec::new();
    assert_eq!(
        track_button(&mut pressed, 0, ElementState::Pressed),
        Some(MouseKind::Press)
    );
    assert_eq!(pressed, vec![0]);
    // A duplicate press is not a transition.
    assert_eq!(track_button(&mut pressed, 0, ElementState::Pressed), None);
    assert_eq!(pressed, vec![0]);
    // A release removes the button.
    assert_eq!(
        track_button(&mut pressed, 0, ElementState::Released),
        Some(MouseKind::Release)
    );
    assert_eq!(pressed, Vec::<u8>::new());
    // A duplicate release is not a transition.
    assert_eq!(track_button(&mut pressed, 0, ElementState::Released), None);
}

#[test]
fn track_button_keeps_each_button_in_press_order() {
    let mut pressed = Vec::new();
    track_button(&mut pressed, 2, ElementState::Pressed);
    track_button(&mut pressed, 0, ElementState::Pressed);
    track_button(&mut pressed, 1, ElementState::Pressed);
    assert_eq!(pressed, vec![2, 0, 1]);
    // The first held button is the one motion events report.
    assert_eq!(pressed.first().copied(), Some(2));
    track_button(&mut pressed, 0, ElementState::Released);
    assert_eq!(pressed, vec![2, 1]);
}

#[test]
fn button_number_maps_winit_buttons() {
    assert_eq!(button_number(MouseButton::Left), 0);
    assert_eq!(button_number(MouseButton::Middle), 1);
    assert_eq!(button_number(MouseButton::Right), 2);
    assert_eq!(button_number(MouseButton::Back), 8);
    assert_eq!(button_number(MouseButton::Forward), 9);
    assert_eq!(button_number(MouseButton::Other(7)), 7);
}

// ------------------------------------------------------- routing decision ---

#[test]
fn should_report_gates_on_mode_and_shift() {
    // No mouse reporting: nothing goes to the terminal.
    assert!(!should_report(MouseMode::None, false));
    assert!(!should_report(MouseMode::None, true));
    // Shift held: the event is for local selection.
    assert!(!should_report(MouseMode::X10, true));
    assert!(!should_report(MouseMode::AllMotion, true));
    // Otherwise: report.
    assert!(should_report(MouseMode::X10, false));
    assert!(should_report(MouseMode::Normal, false));
    assert!(should_report(MouseMode::CellMotion, false));
    assert!(should_report(MouseMode::AllMotion, false));
}

#[test]
fn mouse_modifiers_map_winit_state() {
    assert_eq!(
        mouse_modifiers(&ModifiersState::empty()),
        MouseModifiers::default()
    );
    assert_eq!(
        mouse_modifiers(&ModifiersState::SHIFT),
        MouseModifiers {
            shift: true,
            ctrl: false,
            alt: false
        }
    );
    assert_eq!(
        mouse_modifiers(&ModifiersState::CONTROL),
        MouseModifiers {
            shift: false,
            ctrl: true,
            alt: false
        }
    );
    assert_eq!(
        mouse_modifiers(&ModifiersState::ALT),
        MouseModifiers {
            shift: false,
            ctrl: false,
            alt: true
        }
    );
    assert_eq!(
        mouse_modifiers(&(ModifiersState::SHIFT | ModifiersState::CONTROL | ModifiersState::ALT)),
        MouseModifiers {
            shift: true,
            ctrl: true,
            alt: true
        }
    );
}

// ----------------------------------------------------------------- wheel ---

#[test]
fn wheel_directions_map_lines() {
    assert_eq!(
        wheel_directions(MouseScrollDelta::LineDelta(0.0, 3.0)),
        vec![MouseKind::WheelUp]
    );
    assert_eq!(
        wheel_directions(MouseScrollDelta::LineDelta(0.0, -1.0)),
        vec![MouseKind::WheelDown]
    );
    assert_eq!(
        wheel_directions(MouseScrollDelta::LineDelta(2.0, 0.0)),
        vec![MouseKind::WheelRight]
    );
    assert_eq!(
        wheel_directions(MouseScrollDelta::LineDelta(-2.0, 0.0)),
        vec![MouseKind::WheelLeft]
    );
    // A diagonal delta reports both, vertical first.
    assert_eq!(
        wheel_directions(MouseScrollDelta::LineDelta(1.0, 1.0)),
        vec![MouseKind::WheelUp, MouseKind::WheelRight]
    );
}

#[test]
fn wheel_directions_map_pixels() {
    let px = |x: f64, y: f64| MouseScrollDelta::PixelDelta(PhysicalPosition::new(x, y));
    assert_eq!(wheel_directions(px(0.0, 120.0)), vec![MouseKind::WheelUp]);
    assert_eq!(
        wheel_directions(px(0.0, -120.0)),
        vec![MouseKind::WheelDown]
    );
    assert_eq!(
        wheel_directions(px(120.0, 0.0)),
        vec![MouseKind::WheelRight]
    );
    assert_eq!(
        wheel_directions(px(-120.0, 0.0)),
        vec![MouseKind::WheelLeft]
    );
}

#[test]
fn wheel_directions_zero_is_nothing() {
    assert_eq!(
        wheel_directions(MouseScrollDelta::LineDelta(0.0, 0.0)),
        Vec::<MouseKind>::new()
    );
    assert_eq!(
        wheel_directions(MouseScrollDelta::PixelDelta(PhysicalPosition::new(
            0.0, 0.0
        ))),
        Vec::<MouseKind>::new()
    );
}
