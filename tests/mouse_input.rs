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
    MouseEvent, MouseKind, MouseMode, MouseModifiers, button_number, encode_mouse_event,
    logical_to_cell, mouse_modifiers, physical_to_cell, should_report, track_button,
    wheel_directions,
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

/// The expected report for button code `b` at 1-based cell (x, y).
fn rep(b: u16, x: u16, y: u16, terminator: char) -> String {
    format!("\x1b[<{};{} {}{}", b, x, y, terminator)
}

/// The expected report for the given modifiers on a left button press at
/// (col, row) — used by the modifier rows.
fn mod_press(shift: bool, ctrl: bool, alt: bool) -> MouseEvent {
    MouseEvent {
        kind: MouseKind::Press,
        button: Some(0),
        col: 0,
        row: 0,
        modifiers: MouseModifiers { shift, ctrl, alt },
    }
}

// ---------------------------------------------------------------- X10 ---

#[test]
fn x10_press_release_left_middle_right() {
    for (button, code) in [(Some(0), 0), (Some(1), 1), (Some(2), 2)] {
        assert_eq!(
            encode_mouse_event(
                MouseMode::X10,
                &event(MouseKind::Press, button, 0, 0),
                COLS,
                ROWS
            ),
            Some(rep(code, 1, 1, 'M'))
        );
        assert_eq!(
            encode_mouse_event(
                MouseMode::X10,
                &event(MouseKind::Release, button, 10, 5),
                COLS,
                ROWS
            ),
            Some(rep(code, 11, 6, 'm'))
        );
        // Last valid cell.
        assert_eq!(
            encode_mouse_event(
                MouseMode::X10,
                &event(MouseKind::Press, button, COLS - 1, ROWS - 1),
                COLS,
                ROWS
            ),
            Some(rep(code, COLS, ROWS, 'M'))
        );
    }
}

#[test]
fn x10_reports_no_motion() {
    assert_eq!(
        encode_mouse_event(
            MouseMode::X10,
            &event(MouseKind::Motion, Some(0), 3, 4),
            COLS,
            ROWS
        ),
        None
    );
    assert_eq!(
        encode_mouse_event(
            MouseMode::X10,
            &event(MouseKind::Motion, None, 3, 4),
            COLS,
            ROWS
        ),
        None
    );
}

#[test]
fn x10_reports_wheel() {
    for (kind, code) in [
        (MouseKind::WheelUp, 64),
        (MouseKind::WheelDown, 65),
        (MouseKind::WheelLeft, 66),
        (MouseKind::WheelRight, 67),
    ] {
        assert_eq!(
            encode_mouse_event(MouseMode::X10, &event(kind, None, 0, 0), COLS, ROWS),
            Some(rep(code, 1, 1, 'M'))
        );
    }
}

// -------------------------------------------------------------- Normal ---

#[test]
fn normal_press_release_and_motion() {
    // Press/release are identical to X10.
    assert_eq!(
        encode_mouse_event(
            MouseMode::Normal,
            &event(MouseKind::Press, Some(0), 0, 0),
            COLS,
            ROWS
        ),
        Some(rep(0, 1, 1, 'M'))
    );
    assert_eq!(
        encode_mouse_event(
            MouseMode::Normal,
            &event(MouseKind::Release, Some(2), 1, 1),
            COLS,
            ROWS
        ),
        Some(rep(2, 2, 2, 'm'))
    );
    // Motion while a button is held: 32 + button.
    for (button, code) in [(Some(0), 32), (Some(1), 33), (Some(2), 34)] {
        assert_eq!(
            encode_mouse_event(
                MouseMode::Normal,
                &event(MouseKind::Motion, button, 7, 8),
                COLS,
                ROWS
            ),
            Some(rep(code, 8, 9, 'M'))
        );
    }
}

#[test]
fn normal_reports_no_motion_without_button() {
    assert_eq!(
        encode_mouse_event(
            MouseMode::Normal,
            &event(MouseKind::Motion, None, 3, 4),
            COLS,
            ROWS
        ),
        None
    );
}

// -------------------------------------------------------- CellMotion ---

#[test]
fn cellmotion_reports_motion_without_button() {
    // No button held: 32 + 3 = 35.
    assert_eq!(
        encode_mouse_event(
            MouseMode::CellMotion,
            &event(MouseKind::Motion, None, 3, 4),
            COLS,
            ROWS
        ),
        Some(rep(35, 4, 5, 'M'))
    );
    // With a button held, the held button number is kept.
    assert_eq!(
        encode_mouse_event(
            MouseMode::CellMotion,
            &event(MouseKind::Motion, Some(1), 3, 4),
            COLS,
            ROWS
        ),
        Some(rep(33, 4, 5, 'M'))
    );
}

// ---------------------------------------------------------- AllMotion ---

#[test]
fn allmotion_reports_wheel_and_motion() {
    for (kind, code) in [
        (MouseKind::WheelUp, 64),
        (MouseKind::WheelDown, 65),
        (MouseKind::WheelLeft, 66),
        (MouseKind::WheelRight, 67),
    ] {
        assert_eq!(
            encode_mouse_event(MouseMode::AllMotion, &event(kind, None, 2, 3), COLS, ROWS),
            Some(rep(code, 3, 4, 'M'))
        );
    }
    assert_eq!(
        encode_mouse_event(
            MouseMode::AllMotion,
            &event(MouseKind::Motion, None, 2, 3),
            COLS,
            ROWS
        ),
        Some(rep(35, 3, 4, 'M'))
    );
}

// ---------------------------------------------------------- MouseMode ---

#[test]
fn none_mode_reports_nothing() {
    for kind in [
        MouseKind::Press,
        MouseKind::Release,
        MouseKind::Motion,
        MouseKind::WheelUp,
    ] {
        assert_eq!(
            encode_mouse_event(MouseMode::None, &event(kind, Some(0), 0, 0), COLS, ROWS),
            None
        );
    }
}

#[test]
fn from_term_mode_maps_engine_flags() {
    assert_eq!(
        MouseMode::from_term_mode(TermMode::empty()),
        MouseMode::None
    );
    assert_eq!(
        MouseMode::from_term_mode(TermMode::SHOW_CURSOR),
        MouseMode::None
    );
    assert_eq!(
        MouseMode::from_term_mode(TermMode::MOUSE_REPORT_CLICK),
        MouseMode::X10
    );
    assert_eq!(
        MouseMode::from_term_mode(TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_MOTION),
        MouseMode::Normal
    );
    assert_eq!(
        MouseMode::from_term_mode(
            TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG
        ),
        MouseMode::AllMotion
    );
    // The most specific flag wins, with or without the lower bits set.
    assert_eq!(
        MouseMode::from_term_mode(TermMode::MOUSE_DRAG),
        MouseMode::AllMotion
    );
    // SGR_MOUSE / UTF8_MOUSE are format switches: no bytes change in N16a.
    assert_eq!(
        MouseMode::from_term_mode(TermMode::SGR_MOUSE | TermMode::MOUSE_REPORT_CLICK),
        MouseMode::X10
    );
    // Focus reporting is not a mouse mode.
    assert_eq!(
        MouseMode::from_term_mode(TermMode::FOCUS_IN_OUT),
        MouseMode::None
    );
}

// ---------------------------------------------------------- Modifiers ---

#[test]
fn modifiers_add_to_the_button_code() {
    // Left press (base 0) with each modifier alone.
    assert_eq!(
        encode_mouse_event(MouseMode::X10, &mod_press(true, false, false), COLS, ROWS),
        Some(rep(4, 1, 1, 'M'))
    );
    assert_eq!(
        encode_mouse_event(MouseMode::X10, &mod_press(false, true, false), COLS, ROWS),
        Some(rep(8, 1, 1, 'M'))
    );
    assert_eq!(
        encode_mouse_event(MouseMode::X10, &mod_press(false, false, true), COLS, ROWS),
        Some(rep(64, 1, 1, 'M'))
    );
    // All three together: 0 + 4 + 8 + 64 = 76.
    assert_eq!(
        encode_mouse_event(MouseMode::X10, &mod_press(true, true, true), COLS, ROWS),
        Some(rep(76, 1, 1, 'M'))
    );
    // Modifiers apply to releases and motion too.
    let release = MouseEvent {
        kind: MouseKind::Release,
        button: Some(0),
        col: 0,
        row: 0,
        modifiers: MouseModifiers {
            shift: true,
            ctrl: true,
            alt: false,
        },
    };
    assert_eq!(
        encode_mouse_event(MouseMode::Normal, &release, COLS, ROWS),
        Some(rep(12, 1, 1, 'm'))
    );
    let motion = MouseEvent {
        kind: MouseKind::Motion,
        button: Some(0),
        col: 0,
        row: 0,
        modifiers: MouseModifiers {
            shift: false,
            ctrl: false,
            alt: true,
        },
    };
    assert_eq!(
        encode_mouse_event(MouseMode::Normal, &motion, COLS, ROWS),
        Some(rep(32 + 64, 1, 1, 'M'))
    );
}

// -------------------------------------------------------------- Bounds ---

#[test]
fn bounds_columns() {
    let press = |col: u16| event(MouseKind::Press, Some(0), col, 0);
    // First cell.
    assert_eq!(
        encode_mouse_event(MouseMode::X10, &press(0), COLS, ROWS),
        Some(rep(0, 1, 1, 'M'))
    );
    // Last valid cell.
    assert_eq!(
        encode_mouse_event(MouseMode::X10, &press(COLS - 1), COLS, ROWS),
        Some(rep(0, COLS, 1, 'M'))
    );
    // Exactly on the grid edge: clamped to the last valid cell.
    assert_eq!(
        encode_mouse_event(MouseMode::X10, &press(COLS), COLS, ROWS),
        Some(rep(0, COLS, 1, 'M'))
    );
    // Beyond the grid edge: outside the grid entirely.
    assert_eq!(
        encode_mouse_event(MouseMode::X10, &press(COLS + 100), COLS, ROWS),
        None
    );
}

#[test]
fn bounds_rows() {
    let press = |row: u16| event(MouseKind::Press, Some(0), 0, row);
    assert_eq!(
        encode_mouse_event(MouseMode::X10, &press(0), COLS, ROWS),
        Some(rep(0, 1, 1, 'M'))
    );
    assert_eq!(
        encode_mouse_event(MouseMode::X10, &press(ROWS - 1), COLS, ROWS),
        Some(rep(0, 1, ROWS, 'M'))
    );
    // Exactly on the grid edge: clamped to the last valid cell.
    assert_eq!(
        encode_mouse_event(MouseMode::X10, &press(ROWS), COLS, ROWS),
        Some(rep(0, 1, ROWS, 'M'))
    );
    // Beyond the grid edge: outside the grid entirely.
    assert_eq!(
        encode_mouse_event(MouseMode::X10, &press(ROWS + 100), COLS, ROWS),
        None
    );
}

#[test]
fn bounds_protocol_cap() {
    // A coordinate of 256 or more cannot be carried, even on a wide grid.
    assert_eq!(
        encode_mouse_event(
            MouseMode::X10,
            &event(MouseKind::Press, Some(0), 300, 0),
            400,
            ROWS
        ),
        None
    );
    assert_eq!(
        encode_mouse_event(
            MouseMode::X10,
            &event(MouseKind::Press, Some(0), 0, 300),
            COLS,
            400
        ),
        None
    );
    // 255 (0-based) is still accepted: it clamps to the last cell of a
    // 255-wide grid and encodes as 256 on a wider one.
    assert_eq!(
        encode_mouse_event(
            MouseMode::X10,
            &event(MouseKind::Press, Some(0), 255, 0),
            255,
            ROWS
        ),
        Some(rep(0, 255, 1, 'M'))
    );
    assert_eq!(
        encode_mouse_event(
            MouseMode::X10,
            &event(MouseKind::Press, Some(0), 255, 0),
            400,
            ROWS
        ),
        Some(rep(0, 256, 1, 'M'))
    );
}

#[test]
fn zero_sized_grid_reports_nothing() {
    assert_eq!(
        encode_mouse_event(
            MouseMode::X10,
            &event(MouseKind::Press, Some(0), 0, 0),
            0,
            ROWS
        ),
        None
    );
    assert_eq!(
        encode_mouse_event(
            MouseMode::X10,
            &event(MouseKind::Press, Some(0), 0, 0),
            COLS,
            0
        ),
        None
    );
}

// ---------------------------------------------------------------- Focus ---

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
    assert_eq!(button_number(MouseButton::Back), 4);
    assert_eq!(button_number(MouseButton::Forward), 5);
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
