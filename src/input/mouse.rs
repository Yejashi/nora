//! N16a: pure mouse encoding.
//!
//! Translates one mouse event (plain fields, no winit `WindowEvent`) into
//! the terminal's mouse report, with no terminal state involved, so it is
//! headless-testable. The window layer (N16b) converts winit mouse events
//! into [`MouseEvent`], derives [`MouseMode`] from the engine's `TermMode`
//! via [`MouseMode::from_term_mode`] and calls [`encode_mouse_event`] with
//! the current grid size.

use alacritty_terminal::term::TermMode;

/// The terminal's effective mouse reporting mode (N16a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    /// No mouse reporting: every event encodes to `None`.
    None,
    /// Button press/release only, no motion (xterm `1000`).
    X10,
    /// Press/release plus motion while a button is held (xterm `1002`).
    Normal,
    /// Motion always reported, even without a button held (xterm `1003`).
    CellMotion,
    /// Like [`MouseMode::CellMotion`], plus wheel events (xterm `1003`).
    AllMotion,
}

impl MouseMode {
    /// Derive the effective mouse reporting mode from the terminal's mode
    /// flags (the engine's `TermMode`, `Engine::modes()`).
    ///
    /// `MOUSE_DRAG` (1003) maps to [`MouseMode::AllMotion`],
    /// `MOUSE_MOTION` (1002) to [`MouseMode::Normal`],
    /// `MOUSE_REPORT_CLICK` (1000) to [`MouseMode::X10`], and no mouse flag
    /// to [`MouseMode::None`]. The checks run from the most specific flag
    /// down, so a parser that sets the lower bits alongside a higher one
    /// still resolves to the highest requested mode.
    ///
    /// `SGR_MOUSE` (1006) and `UTF8_MOUSE` (1005) are encoding/format
    /// switches, not reporting modes: the N16a format is the SGR-style
    /// `CSI < b ; x y M` for every mode, so they change no bytes here
    /// (wiring is N16b).
    pub fn from_term_mode(modes: TermMode) -> Self {
        if modes.contains(TermMode::MOUSE_DRAG) {
            Self::AllMotion
        } else if modes.contains(TermMode::MOUSE_MOTION) {
            Self::Normal
        } else if modes.contains(TermMode::MOUSE_REPORT_CLICK) {
            Self::X10
        } else {
            Self::None
        }
    }
}

/// What kind of mouse event it is (N16a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    /// A button press (left/middle/right; `button` carries the number).
    Press,
    /// A button release (same button as the press).
    Release,
    /// Cursor motion; `button` carries the held button, if any.
    Motion,
    /// Wheel scroll up (button code 64).
    WheelUp,
    /// Wheel scroll down (button code 65).
    WheelDown,
    /// Wheel scroll left (button code 66).
    WheelLeft,
    /// Wheel scroll right (button code 67).
    WheelRight,
}

/// Modifier state at the time of a mouse event (N16a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MouseModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// One mouse event in grid coordinates (N16a).
///
/// Wheel events must use the `Wheel*` [`MouseKind`]s; `button` is then
/// informational. For `Press`/`Release`/`Motion`, `button` is the held
/// button number (0=left, 1=middle, 2=right), or `None` for motion with
/// no button held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub button: Option<u8>,
    /// 0-based column.
    pub col: u16,
    /// 0-based row.
    pub row: u16,
    pub modifiers: MouseModifiers,
}

/// Encode a mouse event into the appropriate escape sequence (N16a).
///
/// Returns `None` if `mode` does not report this event kind, or if the
/// event is outside the grid entirely (see the coordinate bounds below).
///
/// Format (all modes): `CSI < b ; x y M` for press/motion/wheel,
/// `CSI < b ; x y m` for release, where:
///
/// - `b` = button code: press/release use the button number (0/1/2);
///   motion uses the held button number, or 3 when no button is held,
///   with the motion bit set (32/33/34/35); wheel uses 64/65/66/67
///   (up/down/left/right); plus modifiers: shift +4, ctrl +8, alt +64;
/// - `x` = `col + 1`, `y` = `row + 1` (1-based), clamped to the last
///   valid cell when the event sits exactly on the grid edge
///   (`col == cols` / `row == rows`);
/// - an event beyond the grid edge (`col > cols` / `row > rows`) or with
///   a coordinate the protocol cannot carry (`>= 256`) returns `None`.
///
/// Motion gating: `X10` reports no motion; `Normal` reports motion only
/// while a button is held; `CellMotion`/`AllMotion` always report
/// motion. Press/release and wheel are button events and are reported
/// by every reporting mode.
pub fn encode_mouse_event(
    mode: MouseMode,
    event: &MouseEvent,
    cols: u16,
    rows: u16,
) -> Option<String> {
    // Mode gating: which event kinds does this mode report?
    let reports = match (mode, event.kind) {
        (MouseMode::None, _) => false,
        (MouseMode::X10, MouseKind::Motion) => false,
        (MouseMode::Normal, MouseKind::Motion) => event.button.is_some(),
        _ => true,
    };
    if !reports {
        return None;
    }

    // Coordinate bounds: a zero-sized grid has no valid cell; an event
    // beyond the grid edge or past the protocol's 255-cell limit is
    // outside the grid entirely; an event on the grid edge clamps to
    // the last valid cell.
    if cols == 0
        || rows == 0
        || event.col > cols
        || event.row > rows
        || event.col >= 256
        || event.row >= 256
    {
        return None;
    }
    let x = (event.col + 1).min(cols);
    let y = (event.row + 1).min(rows);

    let b = button_code(event);
    let terminator = match event.kind {
        MouseKind::Release => 'm',
        _ => 'M',
    };
    Some(format!("\x1b[<{};{} {}{}", b, x, y, terminator))
}

/// The `b` field for a mouse report: the button base code with the
/// motion bit (32) and modifiers (shift +4, ctrl +8, alt +64) added.
///
/// Button base codes pinned from the xterm ctlseqs mouse rows [S18]:
/// 0/1/2 = left/middle/right, 3 = no button, 64/65/66/67 = wheel
/// up/down/left/right; bit 32 marks motion (32+0 held left, 32+3 no
/// button).
fn button_code(event: &MouseEvent) -> u16 {
    let base: u16 = match event.kind {
        MouseKind::Press | MouseKind::Release | MouseKind::Motion => {
            event.button.unwrap_or(3) as u16
        }
        MouseKind::WheelUp => 64,
        MouseKind::WheelDown => 65,
        MouseKind::WheelLeft => 66,
        MouseKind::WheelRight => 67,
    };
    let mut code = match event.kind {
        MouseKind::Motion => base + 32,
        _ => base,
    };
    if event.modifiers.shift {
        code += 4;
    }
    if event.modifiers.ctrl {
        code += 8;
    }
    if event.modifiers.alt {
        code += 64;
    }
    code
}
