//! N16a: pure mouse encoding.
//!
//! Translates one mouse event (plain fields, no winit `WindowEvent`) into
//! the terminal's mouse report, with no terminal state involved, so it is
//! headless-testable. The window layer (N16b) converts winit mouse events
//! into [`MouseEvent`], derives [`MouseMode`] from the engine's `TermMode`
//! via [`MouseMode::from_term_mode`], the encoding via
//! [`MouseEncoding::from_term_mode`], and calls [`encode_mouse_event`] with
//! the current grid size.
//!
//! N16b adds the pure window-side decisions the wiring makes per event:
//! coordinate conversion ([`physical_to_cell`], [`logical_to_cell`]),
//! button press tracking ([`track_button`]), the modifier mapping
//! ([`mouse_modifiers`]), the report-vs-local routing decision
//! ([`should_report`]), the wheel direction mapping
//! ([`wheel_directions`]) and the scroll accumulator that turns touchpad
//! pixels and fractional lines into whole steps ([`ScrollAccumulator`],
//! [`wheel_steps`]).

use alacritty_terminal::term::TermMode;
use winit::event::{ElementState, MouseButton, MouseScrollDelta};
use winit::keyboard::ModifiersState;

/// The application's mouse tracking mode (xterm DECSET 1000/1002/1003).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    /// No reporting: the mouse drives local selection and scrollback.
    None,
    /// X10 compatibility (DECSET 9): presses only. alacritty_terminal does
    /// not track mode 9 separately, so [`MouseMode::from_term_mode`] never
    /// yields it; it is kept for completeness of the encoder.
    X10,
    /// Normal tracking (1000): press, release and wheel.
    Normal,
    /// Button-event tracking (1002): Normal plus motion while a button is
    /// held.
    CellMotion,
    /// Any-event tracking (1003): Normal plus all motion.
    AllMotion,
}

impl MouseMode {
    /// The effective mode from the engine's flags; the most inclusive set
    /// mode wins (an application may set several).
    pub fn from_term_mode(modes: TermMode) -> Self {
        if modes.contains(TermMode::MOUSE_MOTION) {
            Self::AllMotion
        } else if modes.contains(TermMode::MOUSE_DRAG) {
            Self::CellMotion
        } else if modes.contains(TermMode::MOUSE_REPORT_CLICK) {
            Self::Normal
        } else {
            Self::None
        }
    }
}

/// The report encoding the application selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEncoding {
    /// `CSI M Cb Cx Cy`, each value + 32 in one byte (coordinates up to
    /// 223).
    Default,
    /// DECSET 1005: as Default, values UTF-8 encoded (coordinates up to
    /// 2015).
    Utf8,
    /// DECSET 1006: `CSI < b ; x ; y M` / `m` for release, no limit.
    Sgr,
}

impl MouseEncoding {
    pub fn from_term_mode(modes: TermMode) -> Self {
        if modes.contains(TermMode::SGR_MOUSE) {
            Self::Sgr
        } else if modes.contains(TermMode::UTF8_MOUSE) {
            Self::Utf8
        } else {
            Self::Default
        }
    }
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Press,
    Release,
    Motion,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

/// Modifiers that take part in a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MouseModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// One mouse event in grid cells (0-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    /// xterm button number (0 left, 1 middle, 2 right, 8+ extra); for
    /// motion, the held button or `None`.
    pub button: Option<u8>,
    pub col: u16,
    pub row: u16,
    pub modifiers: MouseModifiers,
}

/// Whether `mode` reports `event` at all.
fn reports(mode: MouseMode, event: &MouseEvent) -> bool {
    match (mode, event.kind) {
        (MouseMode::None, _) => false,
        (MouseMode::X10, kind) => kind == MouseKind::Press,
        (MouseMode::Normal, MouseKind::Motion) => false,
        (MouseMode::CellMotion, MouseKind::Motion) => event.button.is_some(),
        _ => true,
    }
}

/// Encode one mouse report, or `None` when the mode does not report the
/// event, the grid is empty, the position is outside it, or the encoding
/// cannot represent the position.
pub fn encode_mouse_event(
    mode: MouseMode,
    encoding: MouseEncoding,
    event: &MouseEvent,
    cols: u16,
    rows: u16,
) -> Option<Vec<u8>> {
    if !reports(mode, event) || cols == 0 || rows == 0 || event.col >= cols || event.row >= rows {
        return None;
    }
    let x = u32::from(event.col) + 1;
    let y = u32::from(event.row) + 1;
    match encoding {
        MouseEncoding::Sgr => {
            let terminator = if event.kind == MouseKind::Release {
                'm'
            } else {
                'M'
            };
            let code = button_code(event, false);
            Some(format!("\x1b[<{code};{x};{y}{terminator}").into_bytes())
        }
        MouseEncoding::Default => {
            let value = |v: u32| u8::try_from(v + 32).ok();
            let code = button_code(event, true);
            Some(vec![
                0x1b,
                b'[',
                b'M',
                value(u32::from(code))?,
                value(x)?,
                value(y)?,
            ])
        }
        MouseEncoding::Utf8 => {
            let mut out = b"\x1b[M".to_vec();
            for v in [u32::from(button_code(event, true)), x, y] {
                let v = v + 32;
                if v > 2047 {
                    return None;
                }
                let c = char::from_u32(v)?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            Some(out)
        }
    }
}

/// The xterm button code: button (0-2, wheel 64-67, extra buttons 128+),
/// +32 for motion, +4 Shift, +8 Alt (meta), +16 Ctrl. The legacy
/// encodings report every release as button 3; SGR keeps the button.
fn button_code(event: &MouseEvent, legacy: bool) -> u16 {
    let button = |b: u8| -> u16 {
        match b {
            0..=2 => u16::from(b),
            n => 128 + u16::from(n.saturating_sub(8)),
        }
    };
    let base: u16 = match event.kind {
        MouseKind::Release if legacy => 3,
        MouseKind::Press | MouseKind::Release => button(event.button.unwrap_or(0)),
        MouseKind::Motion => event.button.map_or(3, button) + 32,
        MouseKind::WheelUp => 64,
        MouseKind::WheelDown => 65,
        MouseKind::WheelLeft => 66,
        MouseKind::WheelRight => 67,
    };
    let mut code = base;
    if event.modifiers.shift {
        code += 4;
    }
    if event.modifiers.alt {
        code += 8;
    }
    if event.modifiers.ctrl {
        code += 16;
    }
    code
}

/// The xterm button number a winit [`MouseButton`] maps to: 0 left,
/// 1 middle, 2 right, 8 back, 9 forward (reported as codes 128 and 129);
/// `Other(n)` carries its own number.
pub fn button_number(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        // xterm buttons 8 and 9 (codes 128 and 129); 4-7 are the wheel.
        MouseButton::Back => 8,
        MouseButton::Forward => 9,
        MouseButton::Other(n) => u8::try_from(n).unwrap_or(u8::MAX),
    }
}

/// Map winit's modifier state to the N16a modifier fields (N16b).
pub fn mouse_modifiers(mods: &ModifiersState) -> MouseModifiers {
    MouseModifiers {
        shift: mods.shift_key(),
        ctrl: mods.control_key(),
        alt: mods.alt_key(),
    }
}

/// Whether a mouse event should be reported to the terminal at all (N16b):
/// `false` when Shift is held (the event belongs to local selection) or
/// when the terminal reports no mouse.
pub fn should_report(mode: MouseMode, shift: bool) -> bool {
    !shift && mode != MouseMode::None
}

/// Map a window cell `(col, row)` to the pane at `(px, py)` of `w` x `h`
/// cells, as the pane's own 0-based `(col, row)`. A position outside the
/// pane is `None`, unless `clamp` is set (the release or drag of a button
/// pressed inside the pane): it is then clamped to the pane's nearest edge
/// cell, as xterm and alacritty do, so the application always sees the
/// release that ends its drag. An empty pane is always `None`.
pub fn cell_in_pane(
    col: u16,
    row: u16,
    (px, py, w, h): (u16, u16, u16, u16),
    clamp: bool,
) -> Option<(u16, u16)> {
    if w == 0 || h == 0 {
        return None;
    }
    let inside =
        (px..px.saturating_add(w)).contains(&col) && (py..py.saturating_add(h)).contains(&row);
    if inside {
        return Some((col - px, row - py));
    }
    clamp.then(|| {
        (
            col.saturating_sub(px).min(w - 1),
            row.saturating_sub(py).min(h - 1),
        )
    })
}

/// Map a logical-pixel position to the grid `(col, row)` (N16b): the pinned
/// mono cell metrics are logical pixels (N11a), so the position is divided
/// by the cell size and truncated. `None` when a cell metric is not
/// positive. Negative positions saturate to cell 0.
pub fn logical_to_cell(x: f64, y: f64, cell_w: u32, cell_h: u32) -> Option<(u16, u16)> {
    let cell_w = cell_w as f64;
    let cell_h = cell_h as f64;
    if cell_w <= 0.0 || cell_h <= 0.0 {
        return None;
    }
    Some(((x / cell_w) as u16, (y / cell_h) as u16))
}

/// Map a physical-pixel position (winit's `CursorMoved`/`MouseWheel`
/// coordinate space) to the grid `(col, row)` (N16b): divide by the window
/// scale factor first, then by the logical cell metrics. `None` when the
/// scale or a cell metric is not positive.
pub fn physical_to_cell(
    x: f64,
    y: f64,
    scale: f64,
    cell_w: u32,
    cell_h: u32,
) -> Option<(u16, u16)> {
    if scale <= 0.0 {
        return None;
    }
    logical_to_cell(x / scale, y / scale, cell_w, cell_h)
}

/// Track a mouse button press/release in `pressed` (N16b): a press records
/// the button (first press first), a release removes it. Returns the event
/// kind to encode for a real state transition, or `None` for a duplicate
/// press/release (no state change).
pub fn track_button(pressed: &mut Vec<u8>, button: u8, state: ElementState) -> Option<MouseKind> {
    match state {
        ElementState::Pressed => {
            if pressed.contains(&button) {
                return None;
            }
            pressed.push(button);
            Some(MouseKind::Press)
        }
        ElementState::Released => {
            if !pressed.contains(&button) {
                return None;
            }
            pressed.retain(|b| *b != button);
            Some(MouseKind::Release)
        }
    }
}

/// The wheel direction(s) a scroll delta produces (N16b): positive
/// `y` = content moves down = scroll up; positive `x` = content moves
/// right = scroll right (winit's sign convention). Vertical is reported
/// before horizontal, so a diagonal delta yields `[vertical, horizontal]`.
/// A zero delta yields no direction.
pub fn wheel_directions(delta: MouseScrollDelta) -> Vec<MouseKind> {
    let (dx, dy) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (x as f64, y as f64),
        MouseScrollDelta::PixelDelta(pos) => (pos.x, pos.y),
    };
    let mut kinds: Vec<MouseKind> = Vec::with_capacity(2);
    if dy > 0.0 {
        kinds.push(MouseKind::WheelUp);
    } else if dy < 0.0 {
        kinds.push(MouseKind::WheelDown);
    }
    if dx > 0.0 {
        kinds.push(MouseKind::WheelRight);
    } else if dx < 0.0 {
        kinds.push(MouseKind::WheelLeft);
    }
    kinds
}

/// The most whole steps one scroll event yields per axis: a runaway delta
/// (a flung touchpad, a bogus driver value) cannot queue an unbounded run
/// of wheel reports or cursor keys.
pub const MAX_SCROLL_STEPS: i32 = 200;

/// Accumulates scroll deltas into whole steps per axis (one step = one
/// line of scrollback or one wheel report), keeping the fraction for the
/// next event. A touchpad sends many small pixel deltas and some mice
/// fractional line deltas; without accumulation each is truncated to
/// nothing, or (reported) inflated to a whole wheel event. Pixels are
/// converted with the cell size. A direction change on an axis drops that
/// axis' remainder, so a reversal responds at once.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScrollAccumulator {
    x: f64,
    y: f64,
}

impl ScrollAccumulator {
    /// Add `delta` and return the whole steps `(x, y)` it completes, in
    /// winit's signs (positive `y` = scroll up, positive `x` = right),
    /// each clamped to [`MAX_SCROLL_STEPS`].
    pub fn add(&mut self, delta: MouseScrollDelta, cell_w: u32, cell_h: u32) -> (i32, i32) {
        let per = |px: f64, cell: u32| if cell > 0 { px / f64::from(cell) } else { 0.0 };
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (f64::from(x), f64::from(y)),
            MouseScrollDelta::PixelDelta(pos) => (per(pos.x, cell_w), per(pos.y, cell_h)),
        };
        (step(&mut self.x, dx), step(&mut self.y, dy))
    }
}

fn step(acc: &mut f64, delta: f64) -> i32 {
    if delta == 0.0 || !delta.is_finite() {
        return 0;
    }
    if *acc != 0.0 && acc.signum() != delta.signum() {
        *acc = 0.0;
    }
    *acc += delta;
    let whole = acc.trunc();
    *acc -= whole;
    let limit = f64::from(MAX_SCROLL_STEPS);
    whole.clamp(-limit, limit) as i32
}

/// The wheel reports for whole scroll steps `(x, y)` from
/// [`ScrollAccumulator::add`]: one report per step, vertical first.
pub fn wheel_steps((x, y): (i32, i32)) -> Vec<MouseKind> {
    let vertical = if y > 0 {
        MouseKind::WheelUp
    } else {
        MouseKind::WheelDown
    };
    let horizontal = if x > 0 {
        MouseKind::WheelRight
    } else {
        MouseKind::WheelLeft
    };
    let mut kinds = vec![vertical; y.unsigned_abs() as usize];
    kinds.extend(std::iter::repeat_n(horizontal, x.unsigned_abs() as usize));
    kinds
}
