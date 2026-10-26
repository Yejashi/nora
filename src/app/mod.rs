//! nora window/application state (N01).
//!
//! Creates a real window on the available native backend, binds a wgpu
//! surface to it and reports backend/device metadata. No terminal content is
//! rendered yet; the terminal arrives with the N03 engine adapter.

mod a11y;
pub mod glyphs;
mod graphics;
mod images;
mod scene;

use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Window, WindowAttributes, WindowId},
};

use self::graphics::Graphics;
use self::scene::Scene;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::TermMode;
use nora::clipboard::{ClipboardBackend, ClipboardKind, clipboard_kind, create_clipboard};
use nora::engine::Snapshot;
use nora::input::bindings::{Action as UiAction, Bindings, Direction};
use nora::input::focus::encode_focus_event;
use nora::input::kitty::{self, Kitty};
use nora::input::mouse::{
    MouseEncoding, MouseEvent, MouseKind, MouseMode, ScrollAccumulator, button_number,
    cell_in_pane, encode_mouse_event, logical_to_cell, mouse_modifiers, should_report,
    track_button, wheel_steps,
};
use nora::input::{BindingTable, InputAction, encode};
use nora::layout::{Layout, LayoutNode, PaneId, SplitDirection};
use nora::render::palette::Palette;
use nora::render::policy::snapshot_is_stale;
use nora::render::state::{Action, RenderEvent, RenderState, SurfaceStatus, on_event};
use nora::render::term::{TermLayout, default_bg};
use nora::session::pump::{AppEvent, PaneKey, PasteGate, PasteGateAction, PumpMsg};
use nora::workspace::{TabId, Workspace};

/// Fixed smoke-test resize target, in logical pixels.
const SMOKE_RESIZE: (f64, f64) = (640.0, 480.0);
/// Settle time after resize+minimize before the smoke run closes the window.
const SMOKE_SETTLE: Duration = Duration::from_millis(250);
/// Watchdog: smoke fails if no progress is made for this long.
const SMOKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Window size requested at creation, in logical pixels.
pub const INITIAL_SIZE: (f64, f64) = (800.0, 600.0);

enum Phase {
    /// Interactive run: alive until the window is closed.
    Interactive,
    /// Smoke: requested the fixed resize, waiting for the compositor to apply it.
    WaitingResize,
    /// Smoke: resize applied, minimize exercised; settling before close.
    Settle,
    /// Smoke finished; `true` if the full cycle succeeded.
    Done(bool),
}

pub struct App {
    smoke: bool,
    phase: Phase,
    /// Set when startup fails before any smoke phase could run.
    failed: bool,
    window: Option<Arc<Window>>,
    graphics: Option<Graphics>,
}

/// Run the nora window. `smoke` performs one automated open/resize/minimize/close cycle.
pub fn run(smoke: bool) -> std::process::ExitCode {
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            eprintln!("nora: failed to create the windowing event loop: {err}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut app = App::new(smoke);
    let result = event_loop.run_app(&mut app);

    match result {
        Ok(()) => app.finish_code(),
        Err(err) => {
            eprintln!("nora: event loop failed: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

impl App {
    fn new(smoke: bool) -> Self {
        Self {
            smoke,
            phase: Phase::Interactive,
            failed: false,
            window: None,
            graphics: None,
        }
    }

    fn finish_code(&self) -> std::process::ExitCode {
        if self.failed {
            return std::process::ExitCode::FAILURE;
        }
        // A smoke run only succeeds if the full open/resize/minimize/settle
        // cycle completed before the window closed.
        if self.smoke && !matches!(self.phase, Phase::Done(true)) {
            return std::process::ExitCode::FAILURE;
        }
        std::process::ExitCode::SUCCESS
    }
}

/// Heuristic native-backend hint from the session environment.
///
/// winit does not expose the chosen backend, so this is labeled a heuristic,
/// not proof of the active protocol.
fn platform_hint() -> String {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        "wayland (heuristic: WAYLAND_DISPLAY is set)".to_owned()
    } else if std::env::var_os("DISPLAY").is_some() {
        "x11 (heuristic: DISPLAY is set)".to_owned()
    } else {
        "unknown (neither WAYLAND_DISPLAY nor DISPLAY is set)".to_owned()
    }
}

/// The window's outer and inner rectangles on the screen (x, y, width,
/// height), for accessibility; positions are 0 where the platform does
/// not tell them (Wayland).
fn window_bounds(window: &Window) -> ([f64; 4], [f64; 4]) {
    let rect = |position: Option<winit::dpi::PhysicalPosition<i32>>,
                size: winit::dpi::PhysicalSize<u32>| {
        let position = position.unwrap_or_default();
        [
            f64::from(position.x),
            f64::from(position.y),
            f64::from(size.width),
            f64::from(size.height),
        ]
    };
    (
        rect(window.outer_position().ok(), window.outer_size()),
        rect(window.inner_position().ok(), window.inner_size()),
    )
}

/// Whether `window` is an X11 window (by its raw handle, not by guessing
/// from the environment).
fn is_x11(window: &Window) -> bool {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    matches!(
        window.window_handle().map(|handle| handle.as_raw()),
        Ok(RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_))
    )
}

/// Open a hyperlink with the platform opener on a short-lived thread (the
/// spawn and wait stay off the window thread). Disallowed schemes are
/// ignored.
fn open_hyperlink(uri: &str) {
    let Some(argv) = nora::engine::opener_argv(uri) else {
        if debug_enabled() {
            eprintln!("debug: hyperlink not opened (scheme not allowed): {uri}");
        }
        return;
    };
    let _ = std::thread::Builder::new()
        .name("nora-open".to_string())
        .spawn(move || {
            if let Ok(mut child) = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                let _ = child.wait();
            }
        });
}

/// Terminal options for new sessions from the configuration and the cell
/// size in pixels.
pub(crate) fn session_options(
    config: &nora::config::NoraConfig,
    cell: (u32, u32),
) -> nora::session::SessionOptions {
    use alacritty_terminal::vte::ansi::CursorShape as Shape;
    use nora::config::CursorShape;
    nora::session::SessionOptions {
        history: config.history(),
        cursor_shape: match config.cursor.style {
            CursorShape::Block => Shape::Block,
            CursorShape::Beam => Shape::Beam,
            CursorShape::Underline => Shape::Underline,
        },
        palette: Palette::from_config(config),
        cell_width: u16::try_from(cell.0).unwrap_or(u16::MAX),
        cell_height: u16::try_from(cell.1).unwrap_or(u16::MAX),
        autocomplete: config.autocomplete_enabled(),
    }
}

/// The selection highlight color: `[colors] selection_background`, else
/// the built-in muted blue.
fn selection_color(config: &nora::config::NoraConfig) -> [u8; 4] {
    config
        .colors
        .selection_background
        .as_deref()
        .and_then(nora::render::palette::parse_hex)
        .map_or(graphics::SELECTION_BG, |c| [c.r, c.g, c.b, 255])
}

/// `[window] opacity` as the alpha byte of the default background.
fn opacity_alpha(config: &nora::config::NoraConfig) -> u8 {
    (config.window_opacity().clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Tell the platform whether the window is translucent (Wayland drops its
/// opaque region; X11 fixed the visual at creation) and whether to blur
/// behind it (KDE Wayland).
fn apply_window_translucency(window: &Window, config: &nora::config::NoraConfig) {
    window.set_transparent(opacity_alpha(config) < 255);
    window.set_blur(config.window_blur());
}

/// Poll the config file's modification time once a second and send
/// `AppEvent::ConfigChanged` when it changes (also when it first appears).
fn watch_config(path: std::path::PathBuf, proxy: winit::event_loop::EventLoopProxy<AppEvent>) {
    let modified = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let spawned = std::thread::Builder::new()
        .name("nora-config-watch".to_string())
        .spawn(move || {
            let mut last = modified(&path);
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let now = modified(&path);
                if now != last {
                    last = now;
                    if proxy.send_event(AppEvent::ConfigChanged).is_err() {
                        return;
                    }
                }
            }
        });
    if spawned.is_err() && debug_enabled() {
        eprintln!("debug: config watcher not started");
    }
}

/// The grid that fits the window's physical inner size: the cell metrics
/// in [`glyphs::Fonts`] are physical pixels (they include the scale factor
/// and zoom), so the grid, the renderer and the mouse mapping agree at
/// every scale.
pub(crate) fn window_grid(window: &Window, fonts: &glyphs::Fonts) -> (u16, u16) {
    let size = window.inner_size();
    glyphs::grid_size(size.width, size.height, fonts.cell_w, fonts.cell_h())
}

/// The direction to focus an adjacent pane (N24b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusDir {
    Left,
    Right,
    Up,
    Down,
}

/// Whether `node`'s subtree contains `pane` (N24b).
fn contains_pane(node: &LayoutNode, pane: PaneId) -> bool {
    match node {
        LayoutNode::Leaf(id) => *id == pane,
        LayoutNode::Split { first, second, .. } => {
            contains_pane(first, pane) || contains_pane(second, pane)
        }
    }
}

/// The leaf of `node` on the edge a move in `dir` enters from: moving
/// left enters a subtree from its right edge (so a horizontal split takes
/// its second child), moving up enters from the bottom; a split across the
/// move takes its first (top/left) child.
fn entry_leaf(node: &LayoutNode, dir: FocusDir) -> PaneId {
    match node {
        LayoutNode::Leaf(id) => *id,
        LayoutNode::Split {
            direction,
            first,
            second,
            ..
        } => {
            let from_far_edge = matches!(
                (dir, direction),
                (FocusDir::Left, SplitDirection::Horizontal)
                    | (FocusDir::Up, SplitDirection::Vertical)
            );
            entry_leaf(if from_far_edge { second } else { first }, dir)
        }
    }
}

/// Find the pane adjacent to `active` in `dir` (N24b), walking the layout
/// tree. Returns `None` when the active pane is at the edge in that
/// direction (no adjacent pane). The innermost split holding the active
/// pane is tried first, so a nearer sibling wins over an outer one; a
/// sibling subtree is entered at the leaf on its near edge.
fn adjacent_pane(node: &LayoutNode, active: PaneId, dir: FocusDir) -> Option<PaneId> {
    match node {
        LayoutNode::Leaf(_) => None,
        LayoutNode::Split {
            direction,
            first,
            second,
            ..
        } => {
            let first_has = contains_pane(first, active);
            let second_has = contains_pane(second, active);
            match (first_has, second_has) {
                (true, false) => adjacent_pane(first, active, dir).or_else(|| {
                    matches!(
                        (dir, direction),
                        (FocusDir::Right, SplitDirection::Horizontal)
                            | (FocusDir::Down, SplitDirection::Vertical)
                    )
                    .then(|| entry_leaf(second, dir))
                }),
                (false, true) => adjacent_pane(second, active, dir).or_else(|| {
                    matches!(
                        (dir, direction),
                        (FocusDir::Left, SplitDirection::Horizontal)
                            | (FocusDir::Up, SplitDirection::Vertical)
                    )
                    .then(|| entry_leaf(first, dir))
                }),
                _ => None,
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attributes = WindowAttributes::default()
            .with_title("nora")
            .with_inner_size(LogicalSize::new(INITIAL_SIZE.0, INITIAL_SIZE.1));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                eprintln!("nora: failed to create the window: {err}");
                self.failed = true;
                event_loop.exit();
                return;
            }
        };

        // b-1 passes a no-op device-loss hook; the b-2 window loop signals loss.
        let mut graphics = match pollster::block_on(Graphics::new(event_loop, &window, |_, _| {})) {
            Ok(graphics) => graphics,
            Err(message) => {
                eprintln!("nora: {message}");
                self.failed = true;
                event_loop.exit();
                return;
            }
        };

        let info = graphics.adapter_info();
        println!("nora {}", env!("CARGO_PKG_VERSION"));
        println!("platform: {}", platform_hint());
        println!(
            "adapter: {} | backend {:?} | device_type {:?} | driver {}",
            info.name, info.backend, info.device_type, info.driver
        );
        if let Err(message) = graphics.set_size(window.inner_size()) {
            eprintln!("nora: initial surface configuration failed: {message}");
        }

        println!(
            "surface: {:?} at {:?}, scale {:.2}",
            graphics.format(),
            window.inner_size(),
            window.scale_factor(),
        );

        if self.smoke {
            // N08b2b: live render foundation — load the pinned fonts and
            // drive one full frame through the pinned wgpu 30 surface flow.
            let mut fonts = glyphs::Fonts::load(&nora::config::FontConfig::default());
            fonts.measure(window.scale_factor(), 1.0);
            println!(
                "fonts: cell {}x{} | mono {}",
                fonts.cell_w,
                fonts.cell_h(),
                fonts.mono_path.display()
            );
            let size = window.inner_size();
            if size.width == 0 || size.height == 0 {
                println!("smoke: window zero-sized; skipped live frame");
            } else if let Err(message) = graphics.set_size(size) {
                eprintln!("smoke: live frame surface configuration failed: {message}");
            } else {
                let mut frame = graphics.acquire();
                println!("smoke: acquire status {:?}", frame.status);
                if let Err(message) = graphics.draw_frame(
                    &mut frame,
                    &mut fonts,
                    &[],
                    [0, 0, 0, 255],
                    &[],
                    &[],
                    &[],
                    &[],
                    &[],
                ) {
                    eprintln!("smoke: live frame draw failed: {message}");
                }
                graphics.present(frame);
                println!("smoke: presented 1 frame");
            }

            let _ = window.request_inner_size(LogicalSize::new(SMOKE_RESIZE.0, SMOKE_RESIZE.1));
            self.phase = Phase::WaitingResize;
            println!(
                "smoke: requested resize to {}x{}",
                SMOKE_RESIZE.0, SMOKE_RESIZE.1
            );
        }

        self.window = Some(window);
        self.graphics = Some(graphics);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if !self.smoke || matches!(self.phase, Phase::Done(_)) {
            return;
        }

        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            let succeeded = matches!(self.phase, Phase::Settle);
            if succeeded {
                println!("smoke: settled after resize and minimize; closing window");
            } else {
                eprintln!("smoke: timed out waiting for the requested resize");
            }
            self.phase = Phase::Done(succeeded);
            // winit 0.30 has no Window::close; the window closes with the
            // event loop, so exit here to tear it down.
            event_loop.exit();
        }

        // Keep smoke runs bounded: re-arm the watchdog on every wake-up.
        let timeout = match self.phase {
            Phase::Settle => SMOKE_SETTLE,
            _ => SMOKE_TIMEOUT,
        };
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + timeout));
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.as_ref() else {
            return;
        };

        match event {
            WindowEvent::Resized(size) => {
                if let Some(graphics) = self.graphics.as_mut()
                    && let Err(message) = graphics.set_size(size)
                {
                    eprintln!("nora: surface reconfiguration failed: {message}");
                }

                if self.smoke && matches!(self.phase, Phase::WaitingResize) {
                    let logical = size.to_logical::<f64>(window.scale_factor());
                    let matches_target = (logical.width - SMOKE_RESIZE.0).abs() < 1.0
                        && (logical.height - SMOKE_RESIZE.1).abs() < 1.0;
                    if matches_target {
                        // Exercise minimize once. On Wayland winit ignores the
                        // un-minimize call by design, so this is best-effort.
                        window.set_minimized(true);
                        window.set_minimized(false);
                        println!(
                            "smoke: resize confirmed at {:?}; minimize exercised (best effort)",
                            logical
                        );
                        self.phase = Phase::Settle;
                    }
                }
            }
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Destroyed => event_loop.exit(),
            _ => {}
        }
    }
}

/// Whether `NORA_DEBUG`-gated diagnostic output is enabled.
fn debug_enabled() -> bool {
    std::env::var_os("NORA_DEBUG").is_some()
}

/// The first non-blank line of a snapshot for NORA_DEBUG frame logs:
/// leading spaces trimmed, truncated to `max_chars`.
fn first_non_space_line(snapshot: &Snapshot, max_chars: usize) -> String {
    for row in 0..snapshot.rows {
        let mut line = String::with_capacity(snapshot.cols);
        for col in 0..snapshot.cols {
            line.push(snapshot.cells[row * snapshot.cols + col].ch);
        }
        let line = line.trim_start();
        if !line.is_empty() {
            return line.chars().take(max_chars).collect();
        }
    }
    String::new()
}

/// The windowed app (N08b2b-2): the event loop owns the window, the render
/// state and the GPU; the session lives only in the pump thread. The
/// window holds only [`RenderState`] and the snapshot slot, and talks to
/// the pump through [`AppEvent`] and [`PumpMsg`].
struct WinApp {
    slot: Arc<Mutex<Option<Snapshot>>>,
    /// Local UI key bindings.
    bindings: Bindings,
    /// Lines per mouse-wheel step (`[scrolling] multiplier`).
    scroll_lines: i32,
    /// Fractional scroll carried between wheel/touchpad events.
    scroll_acc: ScrollAccumulator,
    /// Selection highlight color.
    selection_color: [u8; 4],
    /// Default-background opacity (`[window] opacity`) as an alpha byte.
    bg_alpha: u8,
    /// A transient message on the bottom row (config reload result),
    /// cleared by the next key press.
    notice: Option<String>,
    /// The loaded configuration and where it came from (for reload).
    config_state: nora::config::ConfigState,
    /// Wakes this window from other threads (the font loader).
    proxy: winit::event_loop::EventLoopProxy<AppEvent>,
    /// Screen readers (N37): the active pane's text over AT-SPI2.
    accessibility: a11y::Accessibility,
    /// The active pane's origin and cell size at the last frame, to build
    /// its accessible text when a screen reader starts listening.
    a11y_geometry: Option<((f32, f32), (f32, f32))>,
    /// The full font system being built in the background.
    full_fonts: Option<Arc<Mutex<Option<glyphon::FontSystem>>>>,
    /// Each pane's title (OSC 0/2); the window shows the active pane's.
    pane_titles: std::collections::HashMap<PaneKey, String>,
    /// The last cell a motion report was sent for (one report per cell).
    last_motion_cell: Option<(u16, u16)>,
    /// Each pane's last rendered snapshot, redrawn on redraw requests that
    /// bring no new output (bounded: one snapshot per live pane).
    pane_last: std::collections::HashMap<PaneKey, Snapshot>,
    /// Each pane's inline suggestion (ADR-013): the text after the cursor
    /// the pump computed for the shell's last reported buffer.
    suggestions: std::collections::HashMap<PaneKey, String>,
    /// The number of the last `PumpMsg::Input` sent (to any pane), and
    /// per pane the number of its last one: a suggestion tagged with an
    /// older number answers a buffer the latest input already changed.
    input_seq: u64,
    pane_input_seq: std::collections::HashMap<PaneKey, u64>,
    /// Set by the wgpu device-lost callback; the next render exits.
    device_lost: Arc<AtomicBool>,
    /// `NORA_TRACE_LATENCY`: when the oldest key event not yet followed by
    /// a presented frame arrived (key-to-present measurement).
    trace_key_at: Option<std::time::Instant>,
    /// The same, until a frame with fresh pane content is presented
    /// (key-to-fresh: view changes the pump applies, e.g. scrollback).
    trace_fresh_key_at: Option<std::time::Instant>,
    /// The same, until a frame shows a new inline suggestion
    /// (key-to-suggestion, ADR-013), and whether one is waiting to show.
    trace_suggest_key_at: Option<std::time::Instant>,
    trace_suggest_pending: bool,
    /// Keys whose press went to the application, each with whether kitty
    /// event types were on then and the pane it went to: only such a key's
    /// release is reported, to that pane, so the application never sees a
    /// release without its press (bounded by the number of physical keys).
    sent_keys: std::collections::HashMap<winit::keyboard::PhysicalKey, (PaneKey, bool)>,
    trace_latency: bool,
    /// `NORA_TRACE_FRAMES`: log a per-phase timing line for every frame.
    trace_frames: bool,
    /// Base color table (built-in xterm palette + `[theme]`); each
    /// snapshot's runtime OSC overrides are overlaid on it before drawing.
    palette: Palette,
    pump_commands: mpsc::Sender<PumpMsg>,
    stop: Arc<AtomicBool>,
    state: RenderState,
    /// Keyboard modifier state at the last `ModifiersChanged` (N09b).
    mods: ModifiersState,
    /// Live terminal `TermMode` (N10b): the mode source passed to `encode`.
    /// Initialized to `SHOW_CURSOR` to match N09b's hard-coded behavior; the
    /// first pump tick corrects it to the engine's live mode within ~10 ms.
    modes: TermMode,
    fonts: Option<glyphs::Fonts>,
    graphics: Option<Graphics>,
    window: Option<Arc<Window>>,
    failed: bool,
    frames: u64,
    /// The pump's [`AppEvent::ChildExit`] payload once delivered:
    /// `Some(code)` = the child's exit status, `None` = the pump loop
    /// died without one.
    child_exit: Option<Option<i32>>,
    /// `--hold` (N12b): when set, the `ChildExit` fold skips
    /// `event_loop.exit()`; the window keeps its final frame.
    hold: bool,
    /// Selection drag start in grid `(row, col)` (N14b): `Some` while the
    /// primary button is held down dragging a selection; `None` otherwise.
    selection: Option<(usize, usize)>,
    /// The drag's selection was sent: later motion only moves its end, so
    /// the start stays on the text it was anchored to if the view scrolls.
    selection_sent: bool,
    /// Last cursor position in logical pixels (N14b): `winit` reports
    /// `CursorMoved` in physical pixels and `MouseInput` carries no
    /// position, so the drag tracks the last moved cursor.
    cursor: (f64, f64),
    /// Mouse buttons currently pressed, in press order (N16b): motion
    /// events report the first held button; each release removes its
    /// button.
    pressed_buttons: Vec<u8>,
    /// The pane a reported mouse press landed in, until every button is
    /// released: its drag motion and releases are reported clamped into
    /// it even when the pointer leaves it.
    mouse_grab: Option<PaneKey>,
    /// The last render attempt gathered content but the surface skipped the
    /// frame (acquire `Timeout`/`Occluded`): the gathered snapshots were
    /// retained but never presented, so the next attempt must draw them
    /// even when no fresh snapshot arrived.
    frame_skipped: bool,
    /// Native clipboard backend (N15): `None` when no display server is
    /// available; clipboard operations are no-ops in that case.
    clipboard: Option<Arc<dyn ClipboardBackend>>,
    /// Paste order: the clipboard read in flight (at most one), pastes
    /// queued behind it and the input held until its text was sent.
    paste_gate: PasteGate,
    /// Current zoom level (N19b): 1.0 = 100%; scales the shaping font
    /// size passed to `draw_frame`.
    zoom: f32,
    /// Search bar UI (N20b): the query, matches and visibility, driven
    /// by the keyboard before PTY encoding.
    search: nora::search::SearchUi,
    /// Character snapshot of the last presented visible area (N20b):
    /// the search is bounded to the visible area for now; scrollback
    /// search is a later slice. Row 0 = top visible row.
    last_visible: Vec<Vec<char>>,
    /// The IME state machine (N22b): driven by the platform `Ime` events;
    /// the preedit is UI-only state and each commit is sent to the PTY
    /// exactly once.
    ime: nora::ime::ImeState,
    /// The workspace (N23b): owns all tabs and focus.
    workspace: Workspace,
    /// N24b: the active tab's layout (owns that tab's panes and their
    /// arrangement); one layout per tab, mirrored into `tab_layouts`.
    layout: nora::layout::Layout,
    /// N24b: one layout per tab; switching tabs swaps the active `layout`
    /// without touching the pump.
    tab_layouts: std::collections::HashMap<TabId, nora::layout::Layout>,
    /// N24b: one snapshot slot per pane; the pump stores each pane's latest
    /// snapshot here; `attempt_render` reads the active pane's slot.
    /// Every pane, the active one included, has its own entry.
    pane_slots: std::collections::HashMap<PaneKey, Arc<Mutex<Option<Snapshot>>>>,
    /// Panes whose session the pump has been asked to spawn. A pane laid
    /// out with no room (a split of a one-row pane, a new tab while
    /// minimized) spawns once it has a non-zero size (audit RV6).
    pane_spawned: std::collections::HashSet<PaneKey>,
    /// N24b: the last grid sent to the pump as `PumpMsg::Resize` (or
    /// `NewPane`) for that pane; `(0, 0)` = never sent. The single record
    /// for every pane, the active one included: it is never mirrored into
    /// a window field, so a layout change that resizes the active pane
    /// cannot leave a stale copy that suppresses a later resize.
    pane_grid: std::collections::HashMap<PaneKey, (u16, u16)>,
    /// N24b: the pump's `ChildExit` payload once delivered for that pane
    /// (`None` = not yet delivered).
    pane_exit: std::collections::HashMap<PaneKey, Option<Option<i32>>>,
    /// N24b: per-pane render state machine; `DirtyPulse`/`ChildExit` fold
    /// into the pane's own state.
    pane_render: std::collections::HashMap<PaneKey, RenderState>,
    /// N24b: per-pane search UI; each pane's search is independent.
    pane_search: std::collections::HashMap<PaneKey, nora::search::SearchUi>,
    /// N24b: per-pane live terminal `TermMode` (N10b); the mode source
    /// passed to `encode` for that pane.
    pane_modes: std::collections::HashMap<PaneKey, TermMode>,
    /// N24b: per-pane selection drag start (N14b); each pane's selection is
    /// independent.
    pane_selection: std::collections::HashMap<PaneKey, Option<(usize, usize)>>,
    /// N24b: per-pane visible-area character snapshot (N20b); each pane's
    /// search is bounded to its own visible area.
    pane_last_visible: std::collections::HashMap<PaneKey, Vec<Vec<char>>>,
}

/// Run the windowed terminal (N08b2b-2). `event_loop` carries
/// [`AppEvent`]s from the pump thread; `slot` carries the latest snapshot;
/// `pump_commands` carries window→pump messages; `stop` signals the pump
/// thread when the window closes. `hold` (`--hold`, N12b) defers the
/// `ChildExit` close: the window keeps its final frame until unhold.
/// `config` (N18b) is the loaded configuration; its values are accepted
/// and logged now, and consumed where the windowed path supports them.
#[allow(clippy::too_many_arguments)] // fixed wiring: loop + window + pump + state
pub fn run_windowed(
    event_loop: EventLoop<AppEvent>,
    window: Arc<Window>,
    slot: Arc<Mutex<Option<Snapshot>>>,
    pump_commands: mpsc::Sender<PumpMsg>,
    stop: Arc<AtomicBool>,
    explicit: bool,
    hold: bool,
    config_state: nora::config::ConfigState,
    fonts: glyphs::Fonts,
) -> std::process::ExitCode {
    let config = &config_state.current;
    if debug_enabled() {
        eprintln!(
            "debug: config loaded: history={}, font.size={}",
            config.history(),
            config.font.size
        );
    }
    // N23b/N24b: the workspace owns all tabs; the window's first tab is
    // `TabId(0)` with a single-pane layout, matching the pump's first
    // session (`PaneKey::first()`). Its slot is the shared `slot` Arc, so
    // the pump's writes are visible to `attempt_render`.
    let mut workspace = Workspace::new();
    let (first_tab, _first_pane) = workspace.add_tab();
    let first_pane = PaneKey::first();
    // The active tab's layout: one pane, matching `first_pane`.
    let layout = Layout::new();
    let mut tab_layouts = std::collections::HashMap::new();
    tab_layouts.insert(first_tab, layout.clone());
    let mut pane_slots = std::collections::HashMap::new();
    pane_slots.insert(first_pane, slot.clone());
    let mut pane_grid = std::collections::HashMap::new();
    pane_grid.insert(first_pane, (0, 0));
    let mut pane_exit = std::collections::HashMap::new();
    pane_exit.insert(first_pane, None);
    let mut pane_render = std::collections::HashMap::new();
    pane_render.insert(first_pane, RenderState::default());
    let mut pane_search = std::collections::HashMap::new();
    pane_search.insert(first_pane, nora::search::SearchUi::new());
    let mut pane_modes = std::collections::HashMap::new();
    pane_modes.insert(first_pane, TermMode::SHOW_CURSOR);
    let mut pane_selection = std::collections::HashMap::new();
    pane_selection.insert(first_pane, None);
    let mut pane_last_visible = std::collections::HashMap::new();
    pane_last_visible.insert(first_pane, Vec::new());

    // ADR-016: the event loop owns winit's Wayland connection, which
    // disconnects the `wl_display` when its last clone is dropped (the event
    // loop and each window hold one). A Wayland clipboard's threads use that
    // display for as long as the backend lives, and a paste thread stuck on
    // a selection owner keeps the backend alive past the window. So the
    // event loop is never dropped while a Wayland clipboard exists, not even
    // during an unwind: it is `ManuallyDrop` from here on and is leaked
    // below, which leaves the display connected until the process exits.
    let mut event_loop = std::mem::ManuallyDrop::new(event_loop);
    let display = winit::raw_window_handle::HasDisplayHandle::display_handle(&*window)
        .map(|handle| handle.as_raw())
        .ok();
    let keeps_display = display
        .as_ref()
        .is_some_and(|display| clipboard_kind(display) == ClipboardKind::Wayland);
    // SAFETY: a Wayland `display` is the `wl_display` of `event_loop`'s
    // connection; `event_loop` is never dropped when `keeps_display` (see
    // above), so that display stays connected for the rest of the process,
    // which outlives the backend and every clone of it.
    let clipboard = display.and_then(|display| unsafe { create_clipboard(display) });

    let mut app = WinApp {
        slot,
        pump_commands,
        stop,
        state: RenderState::default(),
        mods: ModifiersState::default(),
        modes: TermMode::SHOW_CURSOR,
        fonts: Some(fonts),
        graphics: None,
        window: Some(window),
        failed: false,
        frames: 0,
        child_exit: None,
        hold,
        selection: None,
        selection_sent: false,
        cursor: (0.0, 0.0),
        pressed_buttons: Vec::new(),
        mouse_grab: None,
        frame_skipped: false,
        clipboard,
        paste_gate: PasteGate::new(),
        zoom: nora::config::ZOOM_DEFAULT,
        palette: Palette::from_config(config),
        bindings: Bindings::with_config(&config.binding_entries()),
        scroll_lines: i32::from(config.scroll_multiplier()),
        scroll_acc: ScrollAccumulator::default(),
        selection_color: selection_color(config),
        bg_alpha: opacity_alpha(config),
        notice: None,
        config_state: config_state.clone(),
        last_motion_cell: None,
        pane_titles: std::collections::HashMap::new(),
        proxy: event_loop.create_proxy(),
        accessibility: a11y::Accessibility::new(event_loop.create_proxy()),
        a11y_geometry: None,
        full_fonts: None,
        pane_last: std::collections::HashMap::new(),
        suggestions: std::collections::HashMap::new(),
        input_seq: 0,
        pane_input_seq: std::collections::HashMap::new(),
        device_lost: Arc::new(AtomicBool::new(false)),
        trace_key_at: None,
        trace_fresh_key_at: None,
        trace_suggest_key_at: None,
        trace_suggest_pending: false,
        sent_keys: std::collections::HashMap::new(),
        trace_latency: std::env::var_os("NORA_TRACE_LATENCY").is_some(),
        trace_frames: std::env::var_os("NORA_TRACE_FRAMES").is_some(),
        search: nora::search::SearchUi::new(),
        last_visible: Vec::new(),
        ime: nora::ime::ImeState::new(),
        workspace,
        layout,
        tab_layouts,
        pane_slots,
        pane_spawned: std::collections::HashSet::from([first_pane]),
        pane_grid,
        pane_exit,
        pane_render,
        pane_search,
        pane_modes,
        pane_selection,
        pane_last_visible,
    };
    // `run_app_on_demand` borrows the loop (`run_app` would drop it on
    // return); on Linux `run_app` is exactly this call.
    let code = match winit::platform::run_on_demand::EventLoopExtRunOnDemand::run_app_on_demand(
        &mut *event_loop,
        &mut app,
    ) {
        Ok(()) => {
            if debug_enabled() {
                eprintln!(
                    "debug: windowed loop ended (failed={} child_exit={:?})",
                    app.failed, app.child_exit
                );
            }
            if app.failed {
                std::process::ExitCode::FAILURE
            } else if explicit {
                // Propagate the child's exit status (N05b 02:52); a loop
                // death without one becomes 1.
                match app.child_exit {
                    Some(Some(code)) => std::process::ExitCode::from(code as u8),
                    _ => std::process::ExitCode::from(1),
                }
            } else {
                // Default shell: 0 on a clean close (N05b 02:29).
                std::process::ExitCode::SUCCESS
            }
        }
        Err(err) => {
            eprintln!("nora: event loop failed: {err}");
            std::process::ExitCode::FAILURE
        }
    };
    drop(app);
    if !keeps_display {
        drop(std::mem::ManuallyDrop::into_inner(event_loop));
    }
    code
}

impl WinApp {
    /// The active tab id (N23b).
    fn active_tab_id(&self) -> Option<TabId> {
        self.workspace.active_tab()
    }

    /// The active pane's key (N24b): the active tab's active pane. `None`
    /// when the active tab has no live panes (its layout is empty).
    fn active_pane_key(&self) -> Option<PaneKey> {
        let tab = self.active_tab_id()?;
        let pane = self.layout.active_pane();
        Some(PaneKey { tab, pane })
    }

    /// Initialize a new pane's per-pane state (N24b).
    fn init_pane_state(&mut self, pane: PaneKey) {
        self.pane_grid.insert(pane, (0, 0));
        self.pane_exit.insert(pane, None);
        self.pane_render.insert(pane, RenderState::default());
        self.pane_search.insert(pane, nora::search::SearchUi::new());
        self.pane_modes.insert(pane, TermMode::SHOW_CURSOR);
        self.pane_selection.insert(pane, None);
        self.pane_last_visible.insert(pane, Vec::new());
    }

    /// Drop a pane's per-pane state (N24b); its session is reaped by the
    /// pump.
    fn drop_pane_state(&mut self, pane: PaneKey) {
        self.pane_last.remove(&pane);
        self.suggestions.remove(&pane);
        self.pane_input_seq.remove(&pane);
        self.pane_titles.remove(&pane);
        self.pane_slots.remove(&pane);
        self.pane_spawned.remove(&pane);
        self.pane_grid.remove(&pane);
        self.pane_exit.remove(&pane);
        self.pane_render.remove(&pane);
        self.pane_search.remove(&pane);
        self.pane_modes.remove(&pane);
        self.pane_selection.remove(&pane);
        self.pane_last_visible.remove(&pane);
    }

    /// Save the active pane's mirrored state into its per-pane maps (N24b).
    fn save_active_pane(&mut self, pane: PaneKey) {
        self.pane_slots.insert(pane, self.slot.clone());
        self.pane_exit.insert(pane, self.child_exit);
        self.pane_render.insert(pane, self.state.clone());
        // The platform composition belongs to the window, not the pane: a
        // preedit moved with the pane would be stranded there (and block
        // accepting its suggestion), so focus leaving drops it instead.
        self.ime.drop_composition();
        self.pane_search.insert(
            pane,
            std::mem::replace(&mut self.search, nora::search::SearchUi::new()),
        );
        self.pane_modes.insert(pane, self.modes);
        self.pane_selection.insert(pane, self.selection);
        self.pane_last_visible
            .insert(pane, std::mem::take(&mut self.last_visible));
    }

    /// Load `pane`'s per-pane state into the single fields (N24b), making
    /// it the active pane's state.
    /// Set the window title to the active pane's title, or "nora".
    fn show_title(&self) {
        let title = self
            .active_pane_key()
            .and_then(|pane| self.pane_titles.get(&pane))
            .map_or("nora", String::as_str);
        if let Some(window) = self.window.as_ref() {
            window.set_title(title);
        }
    }

    fn load_active_pane(&mut self, pane: PaneKey) {
        // The pane's own slot, kept in the map; never another pane's
        // (a pane without one used to share the previous pane's, audit
        // RV6).
        self.slot = self
            .pane_slots
            .entry(pane)
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone();
        self.child_exit = self.pane_exit.remove(&pane).unwrap_or(None);
        self.state = self.pane_render.remove(&pane).unwrap_or_default();
        // Every way in (focus, split, tab switch, closing the active pane)
        // starts without the previous pane's preedit.
        self.ime.drop_composition();
        self.search = self.pane_search.remove(&pane).unwrap_or_default();
        self.modes = self
            .pane_modes
            .remove(&pane)
            .unwrap_or(TermMode::SHOW_CURSOR);
        self.selection = self.pane_selection.remove(&pane).unwrap_or(None);
        self.selection_sent = false;
        self.last_visible = self.pane_last_visible.remove(&pane).unwrap_or_default();
        self.show_title();
    }

    /// Compute each live pane's grid from the active layout and send a
    /// `PumpMsg::Resize` to every pane whose grid changed (N24b). Called
    /// after a split/close/focus and on window resize, so panes always
    /// match their rectangles.
    /// The grid available to panes: the window grid minus the tab bar row
    /// when more than one tab exists.
    fn pane_area(&self) -> Option<(u16, u16)> {
        let window = self.window.as_ref()?;
        let fonts = self.fonts.as_ref()?;
        let (cols, rows) = window_grid(window, fonts);
        let bar = u16::from(self.workspace.tab_count() > 1);
        let rows = rows.saturating_sub(bar);
        (cols > 0 && rows > 0).then_some((cols, rows))
    }

    /// Resize every pane of every tab to its share of the pane area
    /// (change-only per pane). Tabs share the area, so a size change, a
    /// zoom, or the tab bar appearing or disappearing affects them all.
    fn resize_all_panes(&mut self) {
        let Some((cols, rows)) = self.pane_area() else {
            return;
        };
        // Report the cell size in pixels with every geometry change (the
        // pump ignores an unchanged size).
        if let Some(fonts) = self.fonts.as_ref() {
            let _ = self.pump_commands.send(PumpMsg::CellSize {
                width: u16::try_from(fonts.cell_w).unwrap_or(u16::MAX),
                height: u16::try_from(fonts.cell_h()).unwrap_or(u16::MAX),
            });
        }
        let mut layouts: Vec<(TabId, Layout)> = Vec::new();
        if let Some(active) = self.active_tab_id() {
            layouts.push((active, self.layout.clone()));
        }
        for (tab, layout) in &self.tab_layouts {
            if Some(*tab) != self.active_tab_id() {
                layouts.push((*tab, layout.clone()));
            }
        }
        for (tab, layout) in layouts {
            for (pane_id, _x, _y, pw, ph) in layout.compute_rectangles(cols, rows) {
                let pane = PaneKey { tab, pane: pane_id };
                if !self.pane_spawned.contains(&pane) {
                    if pw > 0 && ph > 0 {
                        self.spawn_pane(pane, pw, ph);
                    }
                    continue;
                }
                let prev = self.pane_grid.get(&pane).copied().unwrap_or((0, 0));
                if prev != (pw, ph) {
                    self.pane_grid.insert(pane, (pw, ph));
                    let _ = self.pump_commands.send(PumpMsg::Resize {
                        pane,
                        rows: ph as usize,
                        cols: pw as usize,
                    });
                }
            }
        }
    }

    /// Ask the pump to spawn `pane`'s session at `cols` x `rows`, into the
    /// pane's own snapshot slot.
    fn spawn_pane(&mut self, pane: PaneKey, cols: u16, rows: u16) {
        let slot = self
            .pane_slots
            .entry(pane)
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone();
        self.pane_spawned.insert(pane);
        self.pane_grid.insert(pane, (cols, rows));
        let _ = self.pump_commands.send(PumpMsg::NewPane {
            pane,
            rows: rows as usize,
            cols: cols as usize,
            slot,
        });
    }

    /// Apply a tab command (N23b): the keyboard handler maps Ctrl+T /
    /// Ctrl+W / Ctrl+Tab / Ctrl+Shift+Tab to a [`nora::workspace::TabCommand`]
    /// and calls this. Spawns/reaps the tab's session and swaps the
    /// active tab's mirrored state.
    /// Switch the active layout to `tab`'s and load its active pane's
    /// mirrored state into the single fields (N24b).
    fn switch_to_tab(&mut self, tab: TabId) {
        self.layout = self
            .tab_layouts
            .get(&tab)
            .cloned()
            .unwrap_or_else(Layout::new);
        let pane = self.layout.active_pane();
        self.load_active_pane(PaneKey { tab, pane });
    }

    /// Apply a tab command (N23b/N24b): the keyboard handler maps Ctrl+T /
    /// Ctrl+W / Ctrl+Tab / Ctrl+Shift+Tab to a [`nora::workspace::TabCommand`]
    /// and calls this. Spawns/reaps the tab's pane sessions and swaps the
    /// active tab's layout and the active pane's mirrored state.
    fn on_tab_command(&mut self, cmd: nora::workspace::TabCommand) {
        // Capture the old active pane before the command changes focus.
        let prev_active_pane = self.active_pane_key();
        let prev_active = self.workspace.active_tab();
        let new_active = self.workspace.apply_tab_command(cmd);
        // Compute the new tab's full grid size first (immutable borrows
        // of `self.window` and `self.fonts` end before the mutable
        // operations below).
        match cmd {
            nora::workspace::TabCommand::New => {
                if let Some(tab) = new_active {
                    if let (Some(prev), Some(prev_pane)) = (prev_active, prev_active_pane) {
                        self.save_active_pane(prev_pane);
                        self.tab_layouts.insert(prev, self.layout.clone());
                    }
                    // The new tab starts with a single-pane layout.
                    let new_pane = PaneKey {
                        tab,
                        pane: PaneId(0),
                    };
                    self.layout = Layout::new();
                    self.tab_layouts.insert(tab, self.layout.clone());
                    self.init_pane_state(new_pane);
                    // Its session spawns in `resize_all_panes` below, once
                    // the pane area is known and non-zero.
                    self.load_active_pane(new_pane);
                }
            }
            nora::workspace::TabCommand::Close => {
                if let Some(tab) = prev_active {
                    // Reap every pane of the closed tab; its layout is still
                    // the active one, so `self.layout.panes()` is exact.
                    for pane_id in self.layout.panes() {
                        let pane = PaneKey { tab, pane: pane_id };
                        let _ = self.pump_commands.send(PumpMsg::ClosePane { pane });
                        self.drop_pane_state(pane);
                    }
                    self.tab_layouts.remove(&tab);
                    match self.workspace.active_tab() {
                        Some(new_active) => self.switch_to_tab(new_active),
                        None => self.layout = Layout::new(),
                    }
                }
            }
            nora::workspace::TabCommand::Next | nora::workspace::TabCommand::Prev => {
                if let Some(tab) = new_active {
                    if let (Some(prev), Some(prev_pane)) = (prev_active, prev_active_pane) {
                        self.save_active_pane(prev_pane);
                        self.tab_layouts.insert(prev, self.layout.clone());
                    }
                    self.switch_to_tab(tab);
                }
            }
        }
        // Opening or closing a tab can show or hide the tab bar, which
        // changes every tab's pane area.
        if matches!(
            cmd,
            nora::workspace::TabCommand::New | nora::workspace::TabCommand::Close
        ) {
            self.resize_all_panes();
        }
        self.report_pane_focus(prev_active_pane);
    }

    /// Split the active pane (N24b): mint a new pane in the active layout,
    /// spawn its session, and make it active. `vertical` = new pane below
    /// (split_vertical); otherwise new pane to the right (split_horizontal).
    fn split_active_pane(&mut self, vertical: bool) {
        let Some(tab) = self.active_tab_id() else {
            return;
        };
        // A pane too small to halve is not split (no-op): the new pane
        // would get no room for a session and swallow the keystrokes
        // typed into it. With no known area yet (minimized), the split
        // goes ahead and the new pane spawns once it has room.
        let direction = if vertical {
            SplitDirection::Vertical
        } else {
            SplitDirection::Horizontal
        };
        if let Some((cols, rows)) = self.pane_area()
            && !self.layout.can_split(direction, cols, rows)
        {
            if debug_enabled() {
                eprintln!("debug: split refused: the active pane is too small");
            }
            return;
        }
        // The new pane becomes active: park the current pane's per-pane
        // state (its snapshot slot above all) first, as a focus change
        // does; otherwise the old pane's slot is lost and it stops
        // updating.
        let prev = self.active_pane_key();
        if let Some(prev) = prev {
            self.save_active_pane(prev);
        }
        let new_pane_id = if vertical {
            self.layout.split_vertical()
        } else {
            self.layout.split_horizontal()
        };
        let new_pane = PaneKey {
            tab,
            pane: new_pane_id,
        };
        self.init_pane_state(new_pane);
        // The layout changed: recompute every pane's grid; the new pane's
        // session spawns at its size, or later once it has room.
        self.resize_all_panes();
        // The new pane becomes active; load its (fresh) state.
        self.load_active_pane(new_pane);
        self.report_pane_focus(prev);
    }

    /// Close the active pane (N24b): collapse the layout, reap its session,
    /// and make the layout's remaining active pane the active one.
    fn close_active_pane(&mut self) {
        let Some(tab) = self.active_tab_id() else {
            return;
        };
        let active = self.layout.active_pane();
        if !self.layout.has_pane(active) {
            return;
        }
        let closed = PaneKey { tab, pane: active };
        self.layout.close_pane(active);
        let _ = self.pump_commands.send(PumpMsg::ClosePane { pane: closed });
        self.drop_pane_state(closed);
        // The layout's active pane is now the first remaining pane (or the
        // empty root if none remain).
        let new_active = self.layout.active_pane();
        // The layout changed: recompute every pane's grid.
        self.resize_all_panes();
        let new_pane = PaneKey {
            tab,
            pane: new_active,
        };
        if self.layout.has_pane(new_active) {
            self.load_active_pane(new_pane);
            // The closed pane is gone; only the new one hears focus in.
            self.report_pane_focus(Some(closed));
        } else {
            // The layout is empty: reset the single fields to a fresh pane.
            self.slot = Arc::new(Mutex::new(None));
            self.child_exit = None;
            self.state = RenderState::default();
            self.ime.drop_composition();
            self.search = nora::search::SearchUi::new();
            self.modes = TermMode::SHOW_CURSOR;
        }
    }

    /// Focus an adjacent pane (N24b): `dir` is Left/Right/Up/Down. The
    /// target is the sibling (or ancestor's sibling) in that direction,
    /// found by walking the active layout tree.
    /// Re-measure the cells for the current scale factor and zoom, then
    /// resize every pane's grid (and PTY) to the new metrics. Called on
    /// zoom and on a display scale-factor change.
    fn apply_metrics(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let Some(fonts) = self.fonts.as_mut() else {
            return;
        };
        let metrics = fonts.measure(window.scale_factor(), self.zoom);
        if debug_enabled() {
            eprintln!(
                "debug: cell metrics {}x{} at {:.1}px (zoom {:.1})",
                metrics.cell_w, metrics.cell_h, metrics.font_px, self.zoom
            );
        }
        self.resize_all_panes();
    }

    /// Close the search bar and end the pane's search (its highlight is
    /// cleared and the view returns to the bottom).
    fn close_search(&mut self) {
        self.search.close();
        if let Some(pane) = self.active_pane_key() {
            let _ = self.pump_commands.send(PumpMsg::SearchEnd { pane });
        }
    }

    /// Re-read the configuration and apply it. Colors, bindings, the wheel
    /// multiplier, the selection color and the cursor shape apply at once;
    /// a font change reloads the fonts and resizes every pane; scrollback
    /// applies to new panes; the window size only at startup. A config that
    /// fails to load or validate leaves the current one in place.
    fn reload_config(&mut self) {
        let new_state = match self.config_state.reload() {
            Ok(state) => state,
            Err(err) => {
                eprintln!("nora: config not reloaded, keeping the current one: {err}");
                // The specific problem first: the first validation error,
                // or the parse/read error line.
                let detail = match &err {
                    nora::config::ConfigError::Validation { errors, .. } => {
                        errors.first().cloned().unwrap_or_default()
                    }
                    other => other.to_string().lines().next().unwrap_or("").to_string(),
                };
                self.notice = Some(format!("config error (not applied): {detail}"));
                return;
            }
        };
        for warning in &new_state.warnings {
            eprintln!("nora: warning: {warning}");
        }
        let old = std::mem::replace(&mut self.config_state, new_state);
        let config = self.config_state.current.clone();
        self.palette = Palette::from_config(&config);
        self.bindings = Bindings::with_config(&config.binding_entries());
        self.scroll_lines = i32::from(config.scroll_multiplier());
        self.selection_color = selection_color(&config);
        self.bg_alpha = opacity_alpha(&config);
        if let Some(window) = self.window.as_ref() {
            apply_window_translucency(window, &config);
        }
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.set_selection_color(self.selection_color);
        }
        let cell = self
            .fonts
            .as_ref()
            .map_or((8, 16), |f| (f.cell_w, f.cell_h()));
        let _ = self.pump_commands.send(PumpMsg::Configure {
            options: Box::new(session_options(&config, cell)),
        });
        if old.current.font != config.font
            && let Some(window) = self.window.clone()
        {
            let mut fonts = glyphs::Fonts::load(&config.font);
            fonts.measure(window.scale_factor(), self.zoom);
            let proxy = self.proxy.clone();
            self.full_fonts = Some(fonts.spawn_full_system(move || {
                let _ = proxy.send_event(AppEvent::FontsReady);
            }));
            self.fonts = Some(fonts);
            if let Some(graphics) = self.graphics.as_mut() {
                graphics.clear_glyph_cache();
            }
            self.resize_all_panes();
        }
        let path = self
            .config_state
            .path
            .as_ref()
            .map_or("built-in defaults".to_string(), |p| p.display().to_string());
        eprintln!("nora: config reloaded from {path}");
        self.notice = Some(match self.config_state.warnings.first() {
            Some(first) => format!("config reloaded with a repair: {first}"),
            None => format!("config reloaded from {path}"),
        });
    }

    /// Perform a bound UI action. `false` when the action had nothing to
    /// act on, so the key goes to the application instead.
    fn perform(&mut self, action: UiAction) -> bool {
        use nora::workspace::TabCommand;
        match action {
            UiAction::Copy => {
                if let Some(pane) = self.active_pane_key() {
                    let _ = self.pump_commands.send(PumpMsg::Copy { pane });
                }
            }
            UiAction::Paste => self.paste_clipboard(),
            UiAction::NewTab => self.on_tab_command(TabCommand::New),
            UiAction::NextTab | UiAction::PrevTab => {
                if self.workspace.tab_count() < 2 {
                    return false;
                }
                let cmd = if action == UiAction::NextTab {
                    TabCommand::Next
                } else {
                    TabCommand::Prev
                };
                self.on_tab_command(cmd);
            }
            UiAction::ClosePane => {
                if self.layout.pane_count() <= 1 {
                    self.on_tab_command(TabCommand::Close);
                } else {
                    self.close_active_pane();
                }
            }
            UiAction::SplitRight => self.split_active_pane(false),
            UiAction::SplitDown => self.split_active_pane(true),
            UiAction::FocusPane(direction) => {
                let dir = match direction {
                    Direction::Left => FocusDir::Left,
                    Direction::Right => FocusDir::Right,
                    Direction::Up => FocusDir::Up,
                    Direction::Down => FocusDir::Down,
                };
                return self.focus_pane_direction(dir);
            }
            UiAction::ToggleSearch => {
                if self.search.visible {
                    self.close_search();
                } else {
                    self.search.toggle();
                }
            }
            UiAction::ReloadConfig => self.reload_config(),
            UiAction::AcceptSuggestion => return self.accept_suggestion(false),
            UiAction::AcceptSuggestionWord => return self.accept_suggestion(true),
            UiAction::PreviousPrompt | UiAction::SelectPreviousCommand => {
                if let Some(pane) = self.active_pane_key() {
                    let msg = if action == UiAction::PreviousPrompt {
                        PumpMsg::PreviousPrompt { pane }
                    } else {
                        PumpMsg::SelectPreviousCommand { pane }
                    };
                    let _ = self.pump_commands.send(msg);
                }
            }
            UiAction::ZoomIn | UiAction::ZoomOut | UiAction::ZoomReset => {
                self.zoom = match action {
                    UiAction::ZoomIn => nora::config::zoom_in(self.zoom),
                    UiAction::ZoomOut => nora::config::zoom_out(self.zoom),
                    _ => nora::config::zoom_reset(),
                };
                self.apply_metrics();
            }
            UiAction::ScrollPageUp
            | UiAction::ScrollPageDown
            | UiAction::ScrollToTop
            | UiAction::ScrollToBottom => {
                // The alternate screen has no scrollback: the keys belong
                // to the full-screen application.
                if self.modes.contains(TermMode::ALT_SCREEN) {
                    return false;
                }
                let scroll = match action {
                    UiAction::ScrollPageUp => Scroll::PageUp,
                    UiAction::ScrollPageDown => Scroll::PageDown,
                    UiAction::ScrollToTop => Scroll::Top,
                    _ => Scroll::Bottom,
                };
                if let Some(pane) = self.active_pane_key() {
                    let _ = self.pump_commands.send(PumpMsg::Scroll { pane, scroll });
                }
            }
        }
        true
    }

    /// Type the active pane's inline suggestion (ADR-013), all of it or
    /// its next word, as ordinary input: the shell's editor inserts it.
    /// `false` (the key goes to the application) when there is none to
    /// accept.
    /// Encode a key event under the active kitty keyboard flags.
    /// `pane`'s live terminal modes: the mirrored field for the active
    /// pane, its parked entry otherwise.
    fn modes_of(&self, pane: PaneKey) -> TermMode {
        if self.active_pane_key() == Some(pane) {
            self.modes
        } else {
            self.pane_modes
                .get(&pane)
                .copied()
                .unwrap_or(TermMode::SHOW_CURSOR)
        }
    }

    fn encode_kitty(&self, event: &winit::event::KeyEvent, modes: TermMode) -> Kitty {
        use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
        let unshifted = event.key_without_modifiers();
        let kind = match (event.state, event.repeat) {
            (ElementState::Released, _) => kitty::KeyEventKind::Release,
            (ElementState::Pressed, true) => kitty::KeyEventKind::Repeat,
            (ElementState::Pressed, false) => kitty::KeyEventKind::Press,
        };
        kitty::encode(
            &kitty::KeyEvent {
                logical: &event.logical_key,
                unshifted: &unshifted,
                physical: event.physical_key,
                location: event.location,
                mods: self.mods,
                kind,
                text: event.text.as_deref(),
            },
            modes,
        )
    }

    fn accept_suggestion(&mut self, word: bool) -> bool {
        if self.search.visible || !self.ime.preedit().is_empty() {
            return false;
        }
        let Some(pane) = self.active_pane_key() else {
            return false;
        };
        let Some(suffix) = self.suggestions.remove(&pane) else {
            return false;
        };
        let text = if word { next_word(&suffix) } else { &suffix };
        let bytes = text.as_bytes().to_vec();
        self.send_input(pane, bytes);
        true
    }

    /// Send input bytes to `pane`, numbered (ADR-013): a suggestion is
    /// kept only when tagged with the number of the pane's latest input.
    /// Input behind a paste into the same pane waits for the clipboard
    /// text ([`PasteGate`]).
    fn send_input(&mut self, pane: PaneKey, bytes: Vec<u8>) {
        let actions = self
            .paste_gate
            .input(pane, bytes, std::time::Instant::now());
        self.run_paste_actions(actions);
    }

    /// Run [`PasteGate`] actions in order.
    fn run_paste_actions(&mut self, actions: Vec<PasteGateAction>) {
        let mut actions = std::collections::VecDeque::from(actions);
        while let Some(action) = actions.pop_front() {
            match action {
                PasteGateAction::Input { pane, bytes } => self.send_input_now(pane, bytes),
                PasteGateAction::Paste { pane, text } => self.paste_text(pane, text),
                PasteGateAction::Read { pane, read } => {
                    if !self.read_clipboard(pane, read) {
                        let now = std::time::Instant::now();
                        actions.extend(self.paste_gate.read_done(read, None, now));
                    }
                }
            }
        }
    }

    fn send_input_now(&mut self, pane: PaneKey, bytes: Vec<u8>) {
        self.input_seq += 1;
        let seq = self.input_seq;
        self.pane_input_seq.insert(pane, seq);
        let _ = self.pump_commands.send(PumpMsg::Input { pane, bytes, seq });
    }

    /// Paste the clipboard into the active pane (bracketed when the
    /// application enabled DECSET 2004), through [`WinApp::paste_text`].
    ///
    /// The clipboard is read on a short-lived thread: a slow or stuck
    /// selection owner must not freeze the window, and text over the paste
    /// cap is dropped there instead of being carried to the pump. One read
    /// is in flight at a time (later requests queue behind it); the text
    /// arrives as [`AppEvent::ClipboardText`] for the pane active at the
    /// request, and input to that pane typed meanwhile follows it.
    fn paste_clipboard(&mut self) {
        let (Some(_), Some(pane)) = (self.clipboard.as_ref(), self.active_pane_key()) else {
            if debug_enabled() {
                eprintln!("debug: paste: clipboard unavailable");
            }
            return;
        };
        let actions = self.paste_gate.request(pane, std::time::Instant::now());
        self.run_paste_actions(actions);
    }

    /// Start clipboard read `read` for a paste into `pane` on its thread;
    /// `false` when it could not start.
    fn read_clipboard(&mut self, pane: PaneKey, read: u64) -> bool {
        let Some(clipboard) = self.clipboard.clone() else {
            return false;
        };
        let proxy = self.proxy.clone();
        std::thread::Builder::new()
            .name("nora-paste".to_string())
            .spawn(move || {
                let text = clipboard.get_text().ok().filter(|text| pasteable(text));
                let _ = proxy.send_event(AppEvent::ClipboardText { pane, read, text });
            })
            .is_ok()
    }

    /// Deliver clipboard text read for a paste into `pane` (bracketed when
    /// its application enabled DECSET 2004). The pump validates and
    /// streams the text in order, bounded by the writer's app cap.
    fn paste_text(&mut self, pane: PaneKey, text: String) {
        if !self.pane_is_live(&pane) {
            return;
        }
        // While searching, a paste is query text, never shell input
        // (audit RV8).
        if self.search.visible && self.active_pane_key() == Some(pane) {
            if self.search.state.push_text(&text) {
                let query = self.search.state.query.clone();
                let _ = self
                    .pump_commands
                    .send(PumpMsg::SearchQuery { pane, query });
            }
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
            return;
        }
        let bracketed = self.modes_of(pane).contains(TermMode::BRACKETED_PASTE);
        self.suggestions.remove(&pane);
        // Numbered like keys, so a suggestion computed before the paste
        // is recognised as stale.
        self.input_seq += 1;
        let seq = self.input_seq;
        self.pane_input_seq.insert(pane, seq);
        let _ = self.pump_commands.send(PumpMsg::Paste {
            pane,
            text,
            bracketed,
            seq,
        });
    }

    /// Live panes across all tabs.
    fn total_panes(&self) -> usize {
        let active = self.active_tab_id();
        self.layout.pane_count()
            + self
                .tab_layouts
                .iter()
                .filter(|(tab, _)| Some(**tab) != active)
                .map(|(_, layout)| layout.pane_count())
                .sum::<usize>()
    }

    /// Close one pane of any tab; a tab whose last pane closes is closed.
    fn close_pane(&mut self, pane: PaneKey) {
        if Some(pane.tab) == self.active_tab_id() {
            if self.layout.pane_count() <= 1 {
                self.on_tab_command(nora::workspace::TabCommand::Close);
                return;
            }
            if self.active_pane_key() == Some(pane) {
                self.close_active_pane();
                return;
            }
            self.layout.close_pane(pane.pane);
        } else if let Some(layout) = self.tab_layouts.get_mut(&pane.tab) {
            layout.close_pane(pane.pane);
            if layout.pane_count() == 0 {
                self.tab_layouts.remove(&pane.tab);
                self.workspace.close_tab(pane.tab);
            }
        } else {
            return;
        }
        let _ = self.pump_commands.send(PumpMsg::ClosePane { pane });
        self.drop_pane_state(pane);
        self.resize_all_panes();
    }

    /// The OSC 8 hyperlink URI in the active pane's cell under the mouse,
    /// from the last drawn snapshot.
    fn hyperlink_under_cursor(&self, window: &Window) -> Option<String> {
        let (row, col) = self.grid_cell_at(window, self.cursor.0, self.cursor.1)?;
        let snap = self.pane_last.get(&self.active_pane_key()?)?;
        let cell = snap.cells.get(row * snap.cols + col)?;
        cell.hyperlink.as_ref().map(|(_, uri)| uri.clone())
    }

    /// Make `target` (a pane of the active tab) the active pane.
    fn focus_pane(&mut self, target: PaneId) {
        let Some(tab) = self.active_tab_id() else {
            return;
        };
        let active = self.layout.active_pane();
        if target == active || !self.layout.has_pane(target) {
            return;
        }
        self.layout.focus_pane(target);
        self.save_active_pane(PaneKey { tab, pane: active });
        self.load_active_pane(PaneKey { tab, pane: target });
        // In an area too small for every pane the active one takes the
        // cells, so a focus change can resize panes.
        self.resize_all_panes();
        self.report_pane_focus(Some(PaneKey { tab, pane: active }));
    }

    /// After the active pane changed from `prev`, report focus out to
    /// `prev` and focus in to the new active pane, each only when its
    /// application enabled focus reporting (`FOCUS_IN_OUT`) and only while
    /// the window itself has focus (otherwise neither pane had it).
    fn report_pane_focus(&mut self, prev: Option<PaneKey>) {
        let window_focused = self
            .window
            .as_ref()
            .is_some_and(|window| window.has_focus());
        let with_modes = |pane: Option<PaneKey>| {
            pane.filter(|pane| self.pane_is_live(pane))
                .map(|pane| (pane, self.modes_of(pane)))
        };
        let reports = focus_switch_reports(
            window_focused,
            with_modes(prev),
            with_modes(self.active_pane_key()),
        );
        for (pane, bytes) in reports {
            self.send_input(pane, bytes);
        }
    }

    fn focus_pane_direction(&mut self, dir: FocusDir) -> bool {
        let Some(tab) = self.active_tab_id() else {
            return false;
        };
        let active = self.layout.active_pane();
        let Some(target) = adjacent_pane(self.layout.root(), active, dir) else {
            return false;
        };
        if target == active {
            return false;
        }
        let prev_pane = PaneKey { tab, pane: active };
        self.layout.focus_pane(target);
        let next_pane = PaneKey { tab, pane: target };
        self.save_active_pane(prev_pane);
        self.load_active_pane(next_pane);
        self.resize_all_panes();
        self.report_pane_focus(Some(prev_pane));
        true
    }

    /// Whether `pane` is live: its tab exists and its pane is in that
    /// tab's current layout (N24b). Stale events (for a pane that no
    /// longer exists) are ignored by the caller.
    fn pane_is_live(&self, pane: &PaneKey) -> bool {
        if !self.workspace.has_tab(pane.tab) {
            return false;
        }
        let live = if self.active_tab_id() == Some(pane.tab) {
            &self.layout
        } else {
            match self.tab_layouts.get(&pane.tab) {
                Some(l) => l,
                None => return false,
            }
        };
        live.has_pane(pane.pane)
    }

    /// Route a pump [`AppEvent`] to its pane (N24b): stale events (for a
    /// pane that no longer exists) are ignored; the active pane's state is
    /// updated in place, other panes' state in their per-pane maps.
    /// Returns the render [`Action`] for the fold (used by the caller to
    /// decide whether to exit).
    fn on_pane_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) -> Action {
        match event {
            AppEvent::Accessibility => {
                // A screen reader started listening: the full tree of what is
                // on screen now (there may be no new frame for a while).
                let built = self.active_pane_key().zip(self.a11y_geometry).and_then(
                    |(active, (origin, cell))| {
                        let snap = self.pane_last.get(&active)?;
                        let title = self.window.as_ref().map(|w| w.title()).unwrap_or_default();
                        Some(a11y::TextModel::of(snap, origin, cell, &title))
                    },
                );
                match built {
                    Some(model) => self.accessibility.update(model),
                    None => self.accessibility.resend(),
                }
            }
            AppEvent::DirtyPulse { pane } => {
                if self.pane_is_live(&pane) {
                    if self.active_pane_key() == Some(pane) {
                        let (state, action) =
                            on_event(self.state.clone(), RenderEvent::Dirty { non_noop: true });
                        self.state = state;
                        match action {
                            Action::Render => self.attempt_render(event_loop, false),
                            Action::Skip => {}
                            Action::ExitFail => {
                                self.failed = true;
                                event_loop.exit();
                            }
                            Action::Reconfigure | Action::Recreate | Action::Close => {
                                // A dirty pulse never reconfigures or
                                // closes; the resize / child-exit arms
                                // own those.
                            }
                        }
                        return action;
                    } else if Some(pane.tab) == self.active_tab_id() {
                        // Another pane of the visible tab: the frame shows
                        // it too, so render (the window's surface state
                        // decides, as for the active pane).
                        let (state, action) =
                            on_event(self.state.clone(), RenderEvent::Dirty { non_noop: true });
                        self.state = state;
                        if action == Action::Render {
                            self.attempt_render(event_loop, false);
                        }
                        return action;
                    } else if let Some(state) = self.pane_render.get_mut(&pane) {
                        let (new_state, action) =
                            on_event(state.clone(), RenderEvent::Dirty { non_noop: true });
                        *state = new_state;
                        if action == Action::ExitFail {
                            self.failed = true;
                        }
                        return action;
                    }
                }
            }
            AppEvent::ChildExit { pane, status } => {
                if self.pane_is_live(&pane) {
                    if self.active_pane_key() == Some(pane) {
                        self.child_exit = Some(status);
                        let (state, action) =
                            on_event(self.state.clone(), RenderEvent::ChildExited);
                        self.state = state;
                        if action == Action::ExitFail {
                            self.failed = true;
                        }
                    } else {
                        self.pane_exit.insert(pane, Some(status));
                        if let Some(state) = self.pane_render.get_mut(&pane) {
                            let (new_state, action) =
                                on_event(state.clone(), RenderEvent::ChildExited);
                            *state = new_state;
                            if action == Action::ExitFail {
                                self.failed = true;
                            }
                        }
                    }
                    let window = self.window.as_ref().expect("the window exists");
                    window.request_redraw();
                }
            }
            AppEvent::Modes { pane, modes } => {
                if self.pane_is_live(&pane) {
                    if self.active_pane_key() == Some(pane) {
                        self.modes = modes;
                    } else {
                        self.pane_modes.insert(pane, modes);
                    }
                }
            }
            AppEvent::Copied { pane, text } => {
                if self.pane_is_live(&pane) {
                    if debug_enabled() {
                        eprintln!("debug: copied {} byte(s): {:?}", text.len(), text);
                    }
                    if let Some(cb) = &self.clipboard
                        && let Err(err) = cb.set_text(text.as_str())
                    {
                        eprintln!("nora: {err}");
                    }
                }
            }
            AppEvent::PaneReady { pane } => {
                // The new pane's session is ready; input is already routed
                // to it via the active pane.
                let _ = pane;
            }
            // Handled in `user_event` before routing.
            AppEvent::FontsReady | AppEvent::ConfigChanged | AppEvent::Terminate => {}
            AppEvent::ClipboardText { read, text, .. } => {
                if text.is_none() && debug_enabled() {
                    eprintln!("debug: paste: clipboard unavailable, empty or too large");
                }
                let actions = self
                    .paste_gate
                    .read_done(read, text, std::time::Instant::now());
                self.run_paste_actions(actions);
            }
            AppEvent::Suggestion { pane, suffix, seq } => {
                // Computed before the pane's latest input reached the
                // shell: it answers an older buffer ("git s" -> "tatus"
                // after "t" was typed), so it must not be shown or
                // accepted.
                if !suggestion_is_current(seq, self.pane_input_seq.get(&pane).copied()) {
                    return Action::Skip;
                }
                let shown = suffix.is_some();
                let changed = match suffix {
                    Some(suffix) => self.suggestions.insert(pane, suffix.clone()) != Some(suffix),
                    None => self.suggestions.remove(&pane).is_some(),
                };
                if self.trace_latency && shown && changed {
                    self.trace_suggest_pending = true;
                }
                if changed
                    && self.active_pane_key() == Some(pane)
                    && let Some(window) = self.window.as_ref()
                {
                    window.request_redraw();
                }
            }
            AppEvent::SearchStatus {
                pane,
                current,
                total,
            } => {
                if self.active_pane_key() == Some(pane) && self.search.visible {
                    self.search.state.current = current.saturating_sub(1);
                    self.search.state.total = total;
                    if let Some(window) = self.window.as_ref() {
                        window.request_redraw();
                    }
                }
            }
            AppEvent::Title { pane, title } => {
                let changed = match title {
                    Some(title) => {
                        let changed = self.pane_titles.get(&pane) != Some(&title);
                        self.pane_titles.insert(pane, title);
                        changed
                    }
                    None => self.pane_titles.remove(&pane).is_some(),
                };
                if self.active_pane_key() == Some(pane) {
                    self.show_title();
                }
                // The tab bar (shown with more than one tab) labels each
                // tab with its active pane's title.
                if changed
                    && self.workspace.tab_count() > 1
                    && let Some(window) = self.window.as_ref()
                {
                    window.request_redraw();
                }
            }
            AppEvent::Bell { pane } => {
                // Ask for attention when the window is in the background
                // (taskbar flash / urgency hint); a focused window needs
                // no signal for its own output.
                if self.pane_is_live(&pane)
                    && let Some(window) = self.window.as_ref()
                    && !window.has_focus()
                {
                    window.request_user_attention(Some(
                        winit::window::UserAttentionType::Informational,
                    ));
                }
            }
            AppEvent::PaneGone { pane } => {
                // The pane's session was reaped (or fully drained). Drop
                // its state only if the pane is no longer in the layout
                // (stale); a drained-but-open pane keeps its final frame.
                if !self.pane_is_live(&pane) {
                    self.drop_pane_state(pane);
                }
            }
        }
        Action::Skip
    }

    /// Map a logical-pixel position to a grid `(row, col)`, or `None` if
    /// the position falls outside the current grid (N14b).
    /// The pane of the active tab under physical pixel (`x`, `y`): its id
    /// and its grid rectangle (col, row, width, height) in window cells,
    /// tab bar included.
    fn pane_at(&self, x: f64, y: f64) -> Option<(PaneId, (u16, u16, u16, u16))> {
        let fonts = self.fonts.as_ref()?;
        let (cols, rows) = self.pane_area()?;
        let bar = u16::from(self.workspace.tab_count() > 1);
        let col = (x / f64::from(fonts.cell_w)) as u16;
        let row = (y / f64::from(fonts.cell_h())) as u16;
        self.layout
            .compute_rectangles(cols, rows)
            .into_iter()
            .map(|(id, px, py, w, h)| (id, (px, py + bar, w, h)))
            .find(|&(_, (px, py, w, h))| col >= px && col < px + w && row >= py && row < py + h)
    }

    /// The active pane's rectangle (col, row, width, height) in window
    /// cells.
    fn active_pane_rect(&self) -> Option<(u16, u16, u16, u16)> {
        let (cols, rows) = self.pane_area()?;
        let bar = u16::from(self.workspace.tab_count() > 1);
        let active = self.layout.active_pane();
        self.layout
            .compute_rectangles(cols, rows)
            .into_iter()
            .find(|r| r.0 == active)
            .map(|(_, x, y, w, h)| (x, y + bar, w, h))
    }

    /// The active pane's cell (row, col) under physical pixel (`x`, `y`),
    /// or `None` outside it.
    fn grid_cell_at(&self, _window: &Window, x: f64, y: f64) -> Option<(usize, usize)> {
        let fonts = self.fonts.as_ref()?;
        let (px, py, w, h) = self.active_pane_rect()?;
        let col = (x / f64::from(fonts.cell_w)) as u16;
        let row = (y / f64::from(fonts.cell_h())) as u16;
        (col >= px && col < px + w && row >= py && row < py + h)
            .then(|| (usize::from(row - py), usize::from(col - px)))
    }

    /// Encode one mouse event against the engine's live mouse mode and
    /// the current window grid, and send the bytes to the PTY (N16b).
    /// No-op when Shift is held (the event belongs to local selection),
    /// the terminal reports no mouse, or the fonts are not loaded yet.
    fn send_mouse_event(&mut self, event: &MouseEvent, window: &Window) {
        let _ = window;
        let pane = self.active_pane_key();
        // The grab started by a press ends with the last release, whatever
        // happens to this event.
        let grabbed = pane.is_some() && self.mouse_grab == pane;
        if event.kind == MouseKind::Release && self.pressed_buttons.is_empty() {
            self.mouse_grab = None;
        }
        let mode = MouseMode::from_term_mode(self.modes);
        if !should_report(mode, self.mods.shift_key()) {
            return;
        }
        let (Some(pane), Some(rect)) = (pane, self.active_pane_rect()) else {
            return;
        };
        // Window cells to the active pane's own grid. Events outside the
        // pane are not reported, except the drag motion and release of a
        // press made inside it, which are clamped to its edge.
        let clamp = grabbed
            && (event.kind == MouseKind::Release
                || (event.kind == MouseKind::Motion && event.button.is_some()));
        let Some((col, row)) = cell_in_pane(event.col, event.row, rect, clamp) else {
            return;
        };
        if event.kind == MouseKind::Press {
            self.mouse_grab = Some(pane);
        }
        let event = MouseEvent { col, row, ..*event };
        let encoding = MouseEncoding::from_term_mode(self.modes);
        if let Some(bytes) = encode_mouse_event(mode, encoding, &event, rect.2, rect.3) {
            self.send_input(pane, bytes);
        }
    }

    /// Render the pending snapshot through one acquire → fold → draw →
    /// present pass, following the pure machine's [`Action`].
    /// Render the active pane. A fresh snapshot from the pump wins; with
    /// `retained` (a redraw request: expose, zoom, search, tab or pane
    /// switch, surface reconfigure) the pane's last rendered snapshot is
    /// redrawn when nothing new arrived.
    fn attempt_render(&mut self, event_loop: &ActiveEventLoop, retained: bool) {
        if self.device_lost.load(Ordering::Relaxed) {
            eprintln!("nora: the GPU device was lost; exiting");
            self.failed = true;
            event_loop.exit();
            return;
        }
        let Some(active) = self.active_pane_key() else {
            return;
        };
        let t_start = std::time::Instant::now();
        let allocs_start = nora::trace::thread_allocations();
        let slot = self
            .pane_slots
            .get(&active)
            .cloned()
            .unwrap_or_else(|| self.slot.clone());
        let window = self.window.as_ref().expect("the window exists");
        // Gather before acquiring a surface image: an image acquired and
        // dropped unpresented (nothing new to draw) is not reliably
        // returned to the swapchain by every window system, and once all
        // images are held every acquire times out.
        let fonts = self.fonts.as_ref().expect("fonts are loaded at startup");
        let (grid_cols, grid_rows) = window_grid(window, fonts);
        // With more than one tab, row 0 is the tab bar.
        let bar = u16::from(self.workspace.tab_count() > 1);
        let rects = self
            .layout
            .compute_rectangles(grid_cols, grid_rows.saturating_sub(bar));
        // Each pane of the active tab: its fresh snapshot, else the
        // retained one; a snapshot that does not fit its pane (from
        // before a resize) is fitted to it until the pump sends a new one.
        let mut fresh_any = false;
        let mut panes: Vec<(PaneKey, u16, u16, u16, u16, Option<Snapshot>)> = Vec::new();
        for &(pane_id, x, y, w, h) in &rects {
            let key = PaneKey {
                tab: active.tab,
                pane: pane_id,
            };
            let pane_slot = if key == active {
                Some(slot.clone())
            } else {
                self.pane_slots.get(&key).cloned()
            };
            let fresh =
                pane_slot.and_then(|s| s.lock().unwrap_or_else(PoisonError::into_inner).take());
            fresh_any |= fresh.is_some();
            // One that does not fit (taken before a resize) is drawn cut
            // or padded to the pane until the pump's next snapshot, so a
            // live resize does not flash the bare background.
            let snap = fresh
                .or_else(|| self.pane_last.remove(&key))
                .and_then(|s| {
                    if !snapshot_is_stale(s.rows as u16, s.cols as u16, h, w) {
                        Some(s)
                    } else {
                        (h > 0 && w > 0)
                            .then(|| scene::fit_snapshot(&s, usize::from(h), usize::from(w)))
                    }
                })
                .map(|mut s| {
                    s.colors = self.palette.overlay(&s.colors);
                    s
                });
            panes.push((key, x, y + bar, w, h, snap));
        }
        if fresh_any {
            // A slot was emptied: the pump may build the next snapshot.
            let _ = self.pump_commands.send(PumpMsg::Wake);
        }
        if !fresh_any && !retained && !self.frame_skipped {
            // Coalesced pulse: the earlier pulse already presented
            // the latest content (unless its frame was skipped).
            for (key, .., snap) in panes {
                if let Some(snap) = snap {
                    self.pane_last.insert(key, snap);
                }
            }
            if debug_enabled() {
                eprintln!("debug: coalesced pulse, nothing new to render");
            }
            return;
        }
        let t_gathered = std::time::Instant::now();
        let allocs_gathered = nora::trace::thread_allocations();
        let graphics = self
            .graphics
            .as_mut()
            .expect("graphics are created at startup");
        let mut frame = graphics.acquire();
        let t_acquired = std::time::Instant::now();
        let allocs_acquired = nora::trace::thread_allocations();
        let (state, action) = on_event(
            self.state.clone(),
            RenderEvent::SurfaceStatus(frame.status.clone()),
        );
        self.state = state;
        if action != Action::Render {
            // Keep what was gathered for the next frame.
            for (key, .., snap) in panes.drain(..) {
                if let Some(snap) = snap {
                    self.pane_last.insert(key, snap);
                }
            }
        }
        match action {
            Action::Render => {
                self.frame_skipped = false;
                let fonts = self.fonts.as_mut().expect("fonts are loaded at startup");
                let active_entry = panes.iter().find(|p| p.0 == active);
                let frame_bg = match active_entry.and_then(|p| p.5.as_ref()) {
                    Some(snap) => default_bg(snap),
                    None => self.palette.background(),
                };
                let frame_bg = [frame_bg.r, frame_bg.g, frame_bg.b, self.bg_alpha];
                // The selection belongs to the active pane; its cells are
                // offset to its origin. (Search matches are marked with the
                // selection by the pump, so no separate search cells.)
                let search_cells: Vec<(u32, u32)> = Vec::new();
                let mut selection_cells: Vec<(u32, u32)> = Vec::new();
                let mut line1 = String::new();
                if let Some(&(_, x, y, _, _, Some(ref snap))) = active_entry {
                    let (x, y) = (u32::from(x), u32::from(y));
                    self.last_visible = (0..snap.rows)
                        .map(|row| {
                            (0..snap.cols)
                                .map(|col| snap.cells[row * snap.cols + col].ch)
                                .collect()
                        })
                        .collect();
                    for (i, selected) in snap.selected.iter().enumerate() {
                        if *selected {
                            let (row, col) = (i / snap.cols, i % snap.cols);
                            selection_cells.push((y + row as u32, x + col as u32));
                        }
                    }
                    line1 = first_non_space_line(snap, 40);
                }
                let layout = TermLayout {
                    cell_w: fonts.cell_w as f32,
                    cell_h: fonts.cell_h() as f32,
                    font_size: fonts.metrics.font_px,
                };
                let mut scene = Scene::default();
                for (key, x, y, _, _, snap) in &panes {
                    if let Some(snap) = snap {
                        let cursor = (*key == active).then(|| window.has_focus());
                        scene.add_pane(
                            snap,
                            u32::from(*x),
                            u32::from(*y),
                            frame_bg,
                            cursor,
                            &layout,
                        );
                    }
                }
                if bar == 1 {
                    let tabs = self.workspace.tabs().to_vec();
                    let labels: Vec<String> = tabs
                        .iter()
                        .enumerate()
                        .map(|(i, tab)| {
                            let pane = if *tab == active.tab {
                                Some(active.pane)
                            } else {
                                self.tab_layouts.get(tab).map(|l| l.active_pane())
                            };
                            let title = pane.and_then(|pane| {
                                self.pane_titles.get(&PaneKey { tab: *tab, pane })
                            });
                            match title {
                                Some(t) => {
                                    format!("{} {}", i + 1, t.chars().take(24).collect::<String>())
                                }
                                None => format!("{}", i + 1),
                            }
                        })
                        .collect();
                    let index = tabs.iter().position(|t| *t == active.tab).unwrap_or(0);
                    scene.add_tab_bar(&labels, index, u32::from(grid_cols));
                }
                // IME composition: the preedit is drawn at the active pane's
                // cursor, and the input method is told where that is so its
                // candidate window opens next to it.
                if let Some(&(_, x, y, w, _, Some(ref snap))) = panes.iter().find(|p| p.0 == active)
                    && let Some((row, col)) = snap.cursor
                {
                    let (row, col) = (u32::from(y) + row as u32, u32::from(x) + col as u32);
                    let preedit = self.ime.preedit();
                    let mut covered = 1;
                    let bg = frame_bg;
                    let right = u32::from(x) + u32::from(w);
                    if !preedit.is_empty() {
                        let fg = self.palette.foreground();
                        let fg = [fg.r, fg.g, fg.b, 255];
                        covered = scene.add_preedit(preedit, row, col, right, fg, bg).max(1);
                    } else if let Some(ghost) = self.suggestions.get(&active)
                        && !self.search.visible
                        && window.has_focus()
                    {
                        // Dim: the foreground two fifths of the way from
                        // the background.
                        let fg = self.palette.foreground();
                        let mix = |f: u8, b: u8| ((u16::from(f) * 2 + u16::from(b) * 3) / 5) as u8;
                        let fg = [mix(fg.r, bg[0]), mix(fg.g, bg[1]), mix(fg.b, bg[2]), 255];
                        scene.add_ghost_text(ghost, row, col, right, fg, bg);
                    }
                    // X11's XIM takes only a point, the spot where text is
                    // inserted, and input methods open their candidate
                    // window below it (pinned winit drops the size there):
                    // give the cell's bottom-left, or the candidates cover
                    // the line being edited. Wayland takes the rectangle.
                    let spot_row = if is_x11(window) { row + 1 } else { row };
                    window.set_ime_cursor_area(
                        winit::dpi::PhysicalPosition::new(
                            col as f32 * layout.cell_w,
                            spot_row as f32 * layout.cell_h,
                        ),
                        winit::dpi::PhysicalSize::new(
                            covered as f32 * layout.cell_w,
                            layout.cell_h,
                        ),
                    );
                }
                if panes.len() > 1 {
                    let tiles: Vec<(u32, u32, u32, u32)> = panes
                        .iter()
                        .map(|p| {
                            (
                                u32::from(p.1),
                                u32::from(p.2),
                                u32::from(p.3),
                                u32::from(p.4),
                            )
                        })
                        .collect();
                    scene.add_separators(&tiles, u32::from(bar), layout.cell_w, layout.cell_h);
                }
                // The search bar (or a notice) covers the window's bottom
                // row: added last, so nothing drawn earlier shows over it.
                let bar_text = if self.search.visible {
                    let state = &self.search.state;
                    let status = if state.query.is_empty() {
                        String::new()
                    } else if state.total == 0 {
                        "  no matches".to_string()
                    } else {
                        format!("  {}/{}", state.current + 1, state.total)
                    };
                    Some(format!(
                        " Find: {}{status}   (Enter next, Shift+Enter previous, Esc close)",
                        state.query
                    ))
                } else {
                    self.notice.as_deref().map(|notice| format!(" {notice}"))
                };
                if let Some(text) = bar_text {
                    let row = u32::from(grid_rows.saturating_sub(1));
                    scene.add_search_bar(&text, row, u32::from(grid_cols), layout.cell_h);
                    selection_cells.retain(|&(cell_row, _)| cell_row != row);
                }
                let t_built = std::time::Instant::now();
                let allocs_built = nora::trace::thread_allocations();
                if let Err(message) = graphics.draw_frame(
                    &mut frame,
                    fonts,
                    &scene.cells,
                    frame_bg,
                    &scene.fills,
                    &scene.rects,
                    &selection_cells,
                    &search_cells,
                    &scene.images,
                ) {
                    eprintln!("nora: frame draw failed: {message}");
                }
                let suboptimal = frame.status == SurfaceStatus::Suboptimal;
                let t_drawn = std::time::Instant::now();
                let allocs_drawn = nora::trace::thread_allocations();
                graphics.present(frame);
                if self.frames == 0 {
                    nora::trace::stage("first frame presented");
                }
                if self.trace_frames {
                    let ms = |a: std::time::Instant, b: std::time::Instant| {
                        (b - a).as_secs_f64() * 1000.0
                    };
                    let done = std::time::Instant::now();
                    // Only in `alloc-count` builds; before `cells` so the grid
                    // stays the last field (scripts/perf.sh).
                    let allocs = match (
                        allocs_start,
                        allocs_gathered,
                        allocs_acquired,
                        allocs_built,
                        allocs_drawn,
                        nora::trace::thread_allocations(),
                    ) {
                        (Some(a), Some(g), Some(q), Some(b), Some(d), Some(e)) => format!(
                            " allocs {} gather_allocs {} acquire_allocs {} scene_allocs {} encode_allocs {} present_allocs {}",
                            e - a,
                            g - a,
                            q - g,
                            b - q,
                            d - b,
                            e - d
                        ),
                        _ => String::new(),
                    };
                    eprintln!(
                        "frame: total {:.3} gather {:.3} acquire {:.3} scene {:.3} encode {:.3} present {:.3} ms{allocs} cells {} grid {}x{}",
                        ms(t_start, done),
                        ms(t_start, t_gathered),
                        ms(t_gathered, t_acquired),
                        ms(t_acquired, t_built),
                        ms(t_built, t_drawn),
                        ms(t_drawn, done),
                        scene.cells.len(),
                        grid_cols,
                        grid_rows
                    );
                }
                if let Some(at) = self.trace_key_at.take() {
                    eprintln!(
                        "latency: key-to-present {:.3} ms",
                        at.elapsed().as_secs_f64() * 1000.0
                    );
                }
                if std::mem::take(&mut self.trace_suggest_pending)
                    && let Some(at) = self.trace_suggest_key_at.take()
                {
                    eprintln!(
                        "latency: key-to-suggestion {:.3} ms",
                        at.elapsed().as_secs_f64() * 1000.0
                    );
                }
                if fresh_any && let Some(at) = self.trace_fresh_key_at.take() {
                    eprintln!(
                        "latency: key-to-fresh {:.3} ms",
                        at.elapsed().as_secs_f64() * 1000.0
                    );
                }
                if suboptimal && let Err(message) = graphics.reconfigure() {
                    eprintln!("nora: surface reconfiguration failed: {message}");
                }
                self.frames += 1;
                if debug_enabled() {
                    eprintln!(
                        "debug: frame rendered {} (panes={} line1=\"{}\")",
                        self.frames,
                        panes.len(),
                        line1
                    );
                }
                // Screen readers (N37): the active pane's text, while one listens.
                if let Some((_, x, y, ..)) = panes.iter().find(|p| p.0 == active) {
                    self.a11y_geometry = Some((
                        (f32::from(*x) * layout.cell_w, f32::from(*y) * layout.cell_h),
                        (layout.cell_w, layout.cell_h),
                    ));
                }
                if self.accessibility.is_active()
                    && let Some((.., Some(snap))) = panes.iter().find(|p| p.0 == active)
                    && let Some((origin, cell)) = self.a11y_geometry
                {
                    let model = a11y::TextModel::of(snap, origin, cell, &window.title());
                    self.accessibility.update(model);
                }
                for (key, .., snap) in panes {
                    if let Some(snap) = snap {
                        self.pane_last.insert(key, snap);
                    }
                }
            }
            Action::Reconfigure => {
                // Suboptimal/Outdated: the frame may carry a texture, so
                // drop it before reconfiguring (Surface::configure panics
                // while a SurfaceTexture is alive). Forced: Suboptimal
                // keeps the size, which `set_size` would skip (audit R7).
                drop(frame);
                if let Err(message) = graphics.reconfigure() {
                    eprintln!("nora: surface reconfiguration failed: {message}");
                }
                window.request_redraw();
            }
            Action::Recreate => {
                drop(frame);
                if let Err(message) = graphics.recreate_surface() {
                    eprintln!("nora: surface recreation failed: {message}");
                }
                window.request_redraw();
            }
            Action::ExitFail => {
                self.failed = true;
                event_loop.exit();
            }
            Action::Skip => {
                // No retry budget: drop the frame and wait for the next
                // event. What was gathered is retained but unpresented:
                // the next pulse or redraw must draw it. (No redraw is
                // requested here: an occluded or stalled surface would
                // turn that into a retry loop; X11 exposes and later
                // pulses retry.)
                drop(frame);
                self.frame_skipped = true;
            }
            Action::Close => {
                // `Close` folds only from `ChildExited`, which is folded
                // in `user_event`, never from a surface status.
            }
        }
    }
}

impl ApplicationHandler<AppEvent> for WinApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.graphics.is_some() {
            return;
        }

        // N11a: the window is created in `main` before the session spawn,
        // so the child sees the window's grid from the first byte; this
        // handler only binds the graphics and loads the fonts.
        let window = self
            .window
            .clone()
            .expect("the window is created before the loop runs");

        // N22b: enable the platform IME; `Ime` events are folded into
        // `self.ime` in `window_event` (preedit is UI state, commits are
        // sent to the PTY exactly once).
        window.set_ime_allowed(true);

        nora::trace::stage("event loop running (PTY spawned)");
        let mut graphics = match pollster::block_on(Graphics::new(event_loop, &window, {
            let device_lost = self.device_lost.clone();
            move |reason, message| {
                eprintln!("nora: device lost: {reason:?} {message}");
                device_lost.store(true, Ordering::Relaxed);
            }
        })) {
            Ok(graphics) => graphics,
            Err(message) => {
                eprintln!("nora: {message}");
                self.failed = true;
                event_loop.exit();
                return;
            }
        };

        nora::trace::stage("GPU device and surface ready");
        let mut fonts = match self.fonts.take() {
            Some(fonts) => fonts,
            None => glyphs::Fonts::load(&nora::config::FontConfig::default()),
        };
        fonts.measure(window.scale_factor(), self.zoom);
        let info = graphics.adapter_info();
        println!("nora {}", env!("CARGO_PKG_VERSION"));
        println!("platform: {}", platform_hint());
        println!(
            "adapter: {} | backend {:?} | device_type {:?} | driver {}",
            info.name, info.backend, info.device_type, info.driver
        );
        if let Err(message) = graphics.set_size(window.inner_size()) {
            eprintln!("nora: initial surface configuration failed: {message}");
            self.failed = true;
            event_loop.exit();
            return;
        }
        println!(
            "surface: {:?} at {:?}, scale {:.2}, alpha {:?}",
            graphics.format(),
            window.inner_size(),
            window.scale_factor(),
            graphics.alpha_mode(),
        );
        println!("fonts: cell {}x{}", fonts.cell_w, fonts.cell_h());

        // Fallback fonts for glyphs the configured faces lack are loaded
        // off the startup path and swapped in when ready.
        let proxy = self.proxy.clone();
        self.full_fonts = Some(fonts.spawn_full_system(move || {
            let _ = proxy.send_event(AppEvent::FontsReady);
        }));
        apply_window_translucency(&window, &self.config_state.current);
        if self.bg_alpha < 255 && !graphics.transparent() {
            eprintln!(
                "nora: window.opacity: the surface only presents opaque frames ({:?}); the background stays opaque",
                graphics.alpha_mode()
            );
        }
        graphics.set_selection_color(self.selection_color);
        // Live reload when the config file changes (or first appears at
        // the default location).
        let watched = self.config_state.path.clone().or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config"))
                })
                .map(|dir| dir.join("nora").join("config.toml"))
        });
        if let Some(path) = watched {
            watch_config(path, self.proxy.clone());
        }
        self.graphics = Some(graphics);
        self.fonts = Some(fonts);
    }

    /// A clipboard read that never answers is abandoned at its deadline
    /// (input held behind it goes out), not only at the next key: the
    /// loop wakes then while a read is in flight.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let actions = self.paste_gate.poll(Instant::now());
        self.run_paste_actions(actions);
        event_loop.set_control_flow(match self.paste_gate.deadline() {
            Some(deadline) => ControlFlow::WaitUntil(deadline),
            None => ControlFlow::Wait,
        });
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        if let AppEvent::Terminate = event {
            eprintln!("nora: termination signal, closing");
            self.stop.store(true, Ordering::Relaxed);
            event_loop.exit();
            return;
        }
        if let AppEvent::ConfigChanged = event {
            self.reload_config();
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
            return;
        }
        if let AppEvent::FontsReady = event {
            let full = glyphs::take_loaded(&mut self.full_fonts);
            if let (Some(fs), Some(fonts)) = (full, self.fonts.as_mut()) {
                fonts.install_full_system(fs);
                if let Some(graphics) = self.graphics.as_mut() {
                    graphics.clear_glyph_cache();
                }
                nora::trace::stage("system fallback fonts installed");
                self.resize_all_panes();
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            return;
        }
        // N24b: exit the window when the last pane's child exits (and not
        // holding); the pane is still in its layout at this point.
        // The window closes only when the last pane of the last tab
        // exits. (This read the active tab's stale `tab_layouts` copy, so a
        // child exiting in one half of a split closed the window, audit F6.)
        let exited_pane = match &event {
            AppEvent::ChildExit { pane, .. } if self.pane_is_live(pane) => Some(*pane),
            _ => None,
        };
        let last_pane_exit = exited_pane.is_some() && self.total_panes() <= 1;
        // N24b: extract the exit status before routing (the event is moved
        // into `on_pane_event`).
        let child_exit_status = match &event {
            AppEvent::ChildExit { status, .. } => Some(*status),
            _ => None,
        };
        // N24b: route the event to its pane (stale events are ignored).
        self.on_pane_event(event_loop, event);
        if last_pane_exit && let Some(status) = child_exit_status {
            if debug_enabled() {
                eprintln!("debug: child exit status {status:?}");
            }
            if self.hold {
                // N12b `--hold`: skip the exit; the window keeps its
                // final frame (pulse-gated rendering — the pump has
                // exited, so nothing re-renders).
                if debug_enabled() {
                    eprintln!("debug: hold active; window stays open");
                }
            } else {
                event_loop.exit();
            }
        } else if let Some(pane) = exited_pane
            && !self.hold
        {
            // Another pane remains: close the one whose child exited, as
            // tmux and kitty do (with --hold it stays, showing its output).
            self.close_pane(pane);
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        // N24b: clone the window handle (a cheap `Arc` handle) so the
        // owned `window` does not hold an immutable borrow of `self`
        // across the `&mut self` `resize_all_panes()` call below.
        let window = match &self.window {
            Some(window) => window.clone(),
            None => return,
        };

        match event {
            // Expose, and every redraw this window requests (zoom, search,
            // tab or pane switch, surface reconfigure): draw the newest
            // snapshot, or redraw the retained one.
            WindowEvent::RedrawRequested if !self.state.zero_size => {
                self.state.dirty = true;
                self.attempt_render(event_loop, true);
            }
            WindowEvent::Resized(size) => {
                let (outer, inner) = window_bounds(&window);
                self.accessibility.set_bounds(outer, inner);
                if size.width == 0 || size.height == 0 {
                    // No set_size, no PumpMsg, no redraw: just record.
                    let (state, _action) =
                        on_event(self.state.clone(), RenderEvent::ZeroSize { zero: true });
                    self.state = state;
                    return;
                }

                // (a) Reconfigure the surface: the `Resize` fold's
                // `Reconfigure` action IS this call.
                let mut validation: Option<String> = None;
                if let Some(graphics) = self.graphics.as_mut()
                    && let Err(message) = graphics.set_size(size)
                {
                    validation = Some(message);
                }

                // (b) The grid that fits the new *logical* size from the
                // pinned mono metrics (N11a: logical derivation; pure: no
                // GPU, no display). A scale-factor change that arrived
                // while the window was zero-sized (or never arrived as an
                // event) is measured here, before the grid is derived.
                let (cols, rows) = match self.fonts.as_mut() {
                    Some(fonts) => {
                        if fonts.measured_scale() != window.scale_factor() {
                            fonts.measure(window.scale_factor(), self.zoom);
                        }
                        window_grid(&window, fonts)
                    }
                    None => (0, 0),
                };

                // (c) Fold the resize (or the surface failure, which
                // consumes the retry budget).
                let (state, action) = if let Some(message) = validation {
                    on_event(
                        self.state.clone(),
                        RenderEvent::SurfaceStatus(SurfaceStatus::Validation(message)),
                    )
                } else {
                    on_event(self.state.clone(), RenderEvent::Resize)
                };
                self.state = state;
                if action == Action::ExitFail {
                    self.failed = true;
                    event_loop.exit();
                    return;
                }

                // (d) N24b: resize every pane to its new fraction of the
                // window. `resize_all_panes` sends a `PumpMsg::Resize` to
                // each pane whose grid changed (change-only, so no no-op
                // resizes reach the pump); the next pulse carries each
                // pane's post-resize `Full` snapshot.
                self.resize_all_panes();

                // (e) Coalesce the redraw.
                window.request_redraw(); // redraw SITE 2
                if debug_enabled() {
                    eprintln!("debug: resize {rows}x{cols}");
                }
            }
            // N11a: the window's scale factor changed (monitor move, DPI
            // change). The cells are re-measured for the new scale (font
            // pixels scale with it), then the grid is re-derived.
            WindowEvent::ScaleFactorChanged {
                scale_factor: _,
                inner_size_writer: _,
            } => {
                // (a) The scale-change carries a usable window size,
                // exactly like a `Resized` event.
                if self.state.zero_size || self.state.surface_retry >= 3 {
                    return;
                }
                let (cols, rows) = match self.fonts.as_mut() {
                    Some(fonts) => {
                        fonts.measure(window.scale_factor(), self.zoom);
                        window_grid(&window, fonts)
                    }
                    None => (0, 0),
                };
                // (c) Fold the scale change through the machine with the
                // same guard order as the `Resized` arm: `Resize` forces
                // `Damage::Full` and reconfigures; the following render
                // attempt presents the post-resize snapshot.
                let (state, action) = on_event(self.state.clone(), RenderEvent::Resize);
                self.state = state;
                if action == Action::ExitFail {
                    self.failed = true;
                    event_loop.exit();
                    return;
                }

                // (d) N24b: resize every pane to its new fraction of the
                // window (change-only per pane). The redraw arrives via
                // SITE 1 (DirtyPulse) when the pump takes the post-resize
                // `Full` snapshot — no `request_redraw` in this arm,
                // keeping the pinned exactly-three call sites intact.
                self.resize_all_panes();
                if debug_enabled() {
                    eprintln!("debug: scale factor changed, grid {rows}x{cols}");
                }
            }
            WindowEvent::CloseRequested => {
                if debug_enabled() {
                    eprintln!("debug: window close requested");
                }
                self.stop.store(true, Ordering::Relaxed);
                event_loop.exit();
            }
            WindowEvent::Destroyed => {
                if debug_enabled() {
                    eprintln!("debug: window destroyed");
                }
                self.stop.store(true, Ordering::Relaxed);
                event_loop.exit();
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                // N09b: held-key focus-gain presses are synthetic
                // duplicates. A synthetic release (focus loss with the key
                // held) still ends a press the application saw, so it
                // goes through to the release path below.
                if is_synthetic && event.state == ElementState::Pressed {
                    return;
                }
                // A release is reported only under the kitty protocol's
                // event types, and only for a key whose press reached the
                // application with them on (not one a local binding
                // consumed, nor one pressed before the flags changed). It
                // goes to the pane that got the press, with that pane's
                // modes, even if focus moved to another pane since.
                if event.state == ElementState::Released {
                    if let Some((pane, true)) = self.sent_keys.remove(&event.physical_key)
                        && self.pane_is_live(&pane)
                    {
                        let modes = self.modes_of(pane);
                        if kitty::active(modes)
                            && let Kitty::Send(bytes) = self.encode_kitty(&event, modes)
                        {
                            self.send_input(pane, bytes);
                        }
                    }
                    return;
                }
                // A notice (config reload result) is dismissed by any key.
                if self.notice.take().is_some() {
                    window.request_redraw();
                }
                if self.trace_latency {
                    let now = std::time::Instant::now();
                    self.trace_key_at.get_or_insert(now);
                    self.trace_fresh_key_at.get_or_insert(now);
                    // The latest key: a suggestion answers the keystroke
                    // that produced its buffer.
                    self.trace_suggest_key_at = Some(now);
                }
                // Local UI bindings (input::bindings). A bound chord whose
                // action has nothing to act on falls through to the
                // application; a held non-repeating chord is swallowed so
                // it never leaks control bytes.
                if let Some(action) = self.bindings.lookup(&event.physical_key, self.mods) {
                    if event.repeat && !action.repeats() {
                        return;
                    }
                    if self.perform(action) {
                        window.request_redraw();
                        if self.workspace.tab_count() == 0 {
                            // The last pane closed: the window goes too.
                            self.stop.store(true, Ordering::Relaxed);
                            event_loop.exit();
                        }
                        return;
                    }
                }
                // While the search bar is open it owns text input. The pump
                // searches the pane's scrollback and screen and marks the
                // match with the selection.
                if self.search.visible {
                    let pane = self.active_pane_key();
                    // A Ctrl/Alt/Super chord that is not a binding is not
                    // text: it must not add its letter (audit RV8).
                    let chord =
                        self.mods.control_key() || self.mods.alt_key() || self.mods.super_key();
                    let query_changed = match &event.logical_key {
                        Key::Character(text) if !chord => self.search.state.push_text(text),
                        Key::Named(NamedKey::Space) if !chord => self.search.state.push_text(" "),
                        Key::Named(NamedKey::Backspace) => {
                            self.search.state.query.pop();
                            true
                        }
                        Key::Named(NamedKey::Enter) => {
                            if let Some(pane) = pane {
                                let backwards = !self.mods.shift_key();
                                let _ = self
                                    .pump_commands
                                    .send(PumpMsg::SearchStep { pane, backwards });
                            }
                            false
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.close_search();
                            false
                        }
                        // Every other key is swallowed while searching: it
                        // must not reach the application.
                        _ => false,
                    };
                    if query_changed && let Some(pane) = pane {
                        let query = self.search.state.query.clone();
                        let _ = self
                            .pump_commands
                            .send(PumpMsg::SearchQuery { pane, query });
                    }
                    window.request_redraw();
                    return;
                }
                // Under the kitty keyboard protocol the key is encoded per
                // its flags; where it keeps the legacy bytes, the legacy
                // encoder below runs.
                let kitty_bytes = if kitty::active(self.modes) {
                    match self.encode_kitty(&event, self.modes) {
                        Kitty::Send(bytes) => Some(bytes),
                        Kitty::Legacy => None,
                        Kitty::Nothing => return,
                    }
                } else {
                    None
                };
                let action = match kitty_bytes {
                    Some(bytes) => Some(InputAction::Send(bytes)),
                    None => encode(
                        &event.logical_key,
                        &event.physical_key,
                        &self.mods,
                        event.repeat,
                        &BindingTable::default(),
                        self.modes,
                    ),
                };
                if let Some(action) = action {
                    match action {
                        InputAction::Send(bytes) => {
                            if let Some(pane) = self.active_pane_key() {
                                let types = self.modes.contains(TermMode::REPORT_EVENT_TYPES);
                                self.sent_keys.insert(event.physical_key, (pane, types));
                                // The buffer is about to change: a stale
                                // suggestion must not be accepted before
                                // the shell reports the new one. A key
                                // that cannot edit the line keeps it: the
                                // shell reports no new buffer for it, so
                                // the pump would send no replacement.
                                if key_may_edit_line(&event.logical_key, &bytes) {
                                    self.suggestions.remove(&pane);
                                }
                                self.send_input(pane, bytes);
                            }
                        }
                        InputAction::Quit => {
                            // Local quit (Ctrl+Q): the session drops on
                            // unwind, the master closes and the child
                            // gets SIGHUP.
                            event_loop.exit();
                        }
                    }
                }
            }
            WindowEvent::ModifiersChanged(mods) => self.mods = mods.state(),
            WindowEvent::MouseWheel { delta, .. } => {
                // Touchpad pixel deltas and fractional line deltas add up
                // to whole steps (lines / wheel reports) across events.
                let Some(fonts) = self.fonts.as_ref() else {
                    return;
                };
                let (cell_w, cell_h) = (fonts.cell_w, fonts.cell_h());
                let steps = self.scroll_acc.add(delta, cell_w, cell_h);
                if steps == (0, 0) {
                    return;
                }
                // N16b: when the terminal reports mouse (and Shift is
                // not held), the application owns the scroll: report
                // one wheel event per whole step at the last tracked
                // cursor position. Otherwise the wheel scrolls the local
                // scrollback (N14a).
                if should_report(MouseMode::from_term_mode(self.modes), self.mods.shift_key()) {
                    let (x, y) = self.cursor;
                    if let Some((col, row)) = logical_to_cell(x, y, cell_w, cell_h) {
                        for kind in wheel_steps(steps) {
                            self.send_mouse_event(
                                &MouseEvent {
                                    kind,
                                    button: None,
                                    col,
                                    row,
                                    modifiers: mouse_modifiers(&self.mods),
                                },
                                &window,
                            );
                        }
                    }
                    return;
                }
                // N14a: the wheel scrolls the scrollback. winit reports
                // positive y as "content moves down" (scroll up) for
                // both delta kinds, so positive lines = into history;
                // the pump applies the delta to the engine on its next
                // tick. The configured multiplier scales wheel notches;
                // touchpad pixels already move one line per cell height.
                let lines = match delta {
                    MouseScrollDelta::LineDelta(..) => steps.1 * self.scroll_lines,
                    MouseScrollDelta::PixelDelta(_) => steps.1,
                };
                // Alternate scroll (DECSET 1007): the alternate screen has
                // no scrollback, so the wheel becomes cursor keys for the
                // full-screen application (less, man, vim).
                if self.modes.contains(TermMode::ALT_SCREEN)
                    && self.modes.contains(TermMode::ALTERNATE_SCROLL)
                {
                    if lines != 0
                        && let Some(pane) = self.active_pane_key()
                    {
                        let key = if lines > 0 { b'A' } else { b'B' };
                        let one: &[u8] = if self.modes.contains(TermMode::APP_CURSOR) {
                            &[0x1b, b'O', key]
                        } else {
                            &[0x1b, b'[', key]
                        };
                        let bytes = one.repeat(lines.unsigned_abs() as usize);
                        self.send_input(pane, bytes);
                    }
                    return;
                }
                if lines != 0
                    && let Some(pane) = self.active_pane_key()
                {
                    let _ = self.pump_commands.send(PumpMsg::Scroll {
                        pane,
                        scroll: Scroll::Delta(lines),
                    });
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // N14b: track the cursor in logical pixels (`winit`
                // reports physical). While a selection drag is in
                // progress, extend the selection to the cell under the
                // cursor.
                // Physical pixels, like the cell metrics and the grid.
                let (x, y) = (position.x, position.y);
                self.cursor = (x, y);
                if let Some((start_row, start_col)) = self.selection
                    && self.pressed_buttons.contains(&0)
                    && let Some((end_row, end_col)) = self.grid_cell_at(&window, x, y)
                    && (self.selection_sent || (end_row, end_col) != (start_row, start_col))
                    && let Some(pane) = self.active_pane_key()
                {
                    let msg = if self.selection_sent {
                        PumpMsg::ExtendSelection {
                            pane,
                            end_row,
                            end_col,
                        }
                    } else {
                        PumpMsg::Select {
                            pane,
                            start_row,
                            start_col,
                            end_row,
                            end_col,
                        }
                    };
                    self.selection_sent = true;
                    let _ = self.pump_commands.send(msg);
                }
                // N16b: report the motion to the terminal (gated by the
                // engine's mouse mode inside `send_mouse_event`; `X10`
                // and `Normal` encode to nothing without a held
                // button).
                if let Some(fonts) = self.fonts.as_ref()
                    && let Some((col, row)) =
                        logical_to_cell(position.x, position.y, fonts.cell_w, fonts.cell_h())
                    // Motion is reported once per cell, not per pixel.
                    && self.last_motion_cell != Some((col, row))
                {
                    self.last_motion_cell = Some((col, row));
                    self.send_mouse_event(
                        &MouseEvent {
                            kind: MouseKind::Motion,
                            button: self.pressed_buttons.first().copied(),
                            col,
                            row,
                            modifiers: mouse_modifiers(&self.mods),
                        },
                        &window,
                    );
                }
            }
            WindowEvent::MouseInput { button, state, .. } => {
                // A press in another pane of the visible tab focuses it
                // first; the click then belongs to that pane. Not while
                // another button is held or a press holds the grab: the
                // pane that took that press keeps the events up to its
                // release (a right-click over B during a left-drag in A
                // must not strand A's release).
                if state == ElementState::Pressed
                    && self.pressed_buttons.is_empty()
                    && self.mouse_grab.is_none()
                    && let Some((target, _)) = self.pane_at(self.cursor.0, self.cursor.1)
                    && target != self.layout.active_pane()
                {
                    self.focus_pane(target);
                    window.request_redraw();
                }
                // Ctrl+click on an OSC 8 hyperlink opens it (only allowed
                // schemes, through the platform opener, URL as one argv
                // element); the click does nothing else.
                if button == MouseButton::Left
                    && state == ElementState::Pressed
                    && self.mods.control_key()
                    && let Some(uri) = self.hyperlink_under_cursor(&window)
                {
                    open_hyperlink(&uri);
                    return;
                }
                // Local selection: a left press clears the old selection and
                // anchors a new one that starts on drag (`MouseInput` has no
                // position, so the last tracked cursor is used). When the
                // application reports the mouse, the click is its own; Shift
                // overrides, as in xterm.
                let reporting =
                    should_report(MouseMode::from_term_mode(self.modes), self.mods.shift_key());
                if button == MouseButton::Left && !reporting {
                    let (x, y) = self.cursor;
                    match state {
                        ElementState::Pressed => {
                            if let Some(pane) = self.active_pane_key() {
                                let _ = self.pump_commands.send(PumpMsg::ClearSelection { pane });
                            }
                            self.selection = self.grid_cell_at(&window, x, y);
                            self.selection_sent = false;
                        }
                        ElementState::Released => {
                            self.selection = None;
                        }
                    }
                }
                // N16b: report the press/release to the terminal at the
                // last tracked cursor position (logical pixels).
                let button_num = button_number(button);
                if let Some(kind) = track_button(&mut self.pressed_buttons, button_num, state) {
                    let (x, y) = self.cursor;
                    if let Some(fonts) = self.fonts.as_ref()
                        && let Some((col, row)) =
                            logical_to_cell(x, y, fonts.cell_w, fonts.cell_h())
                    {
                        self.send_mouse_event(
                            &MouseEvent {
                                kind,
                                button: Some(button_num),
                                col,
                                row,
                                modifiers: mouse_modifiers(&self.mods),
                            },
                            &window,
                        );
                    }
                }
            }
            // N22b: fold the platform IME events into the state machine.
            // The preedit is UI-only state (displayed, never sent); the
            // commit is sent to the PTY exactly once through the ordered
            // writer; cancel/transfer clear all state without sending.
            WindowEvent::Ime(ime_event) => match ime_event {
                winit::event::Ime::Enabled => {
                    self.ime.start_composition();
                    if debug_enabled() {
                        eprintln!("IME: composition started");
                    }
                }
                winit::event::Ime::Preedit(text, _cursor_pos) => {
                    self.ime.update_preedit(&text);
                    if debug_enabled() {
                        eprintln!("IME: preedit = {text:?}");
                    }
                    // The preedit is drawn by the window, not the PTY: no
                    // output will trigger the frame.
                    window.request_redraw();
                }
                winit::event::Ime::Commit(text) => {
                    // The platform text is authoritative: under Wayland
                    // text-input (fcitx5, ibus) every ordinary keystroke is
                    // delivered as a commit with no preceding preedit, and
                    // the key itself never reaches `KeyboardInput`. The
                    // tracked preedit is only a fallback for a platform that
                    // commits an empty string; either way the state machine
                    // deactivates exactly once.
                    let tracked = self.ime.commit();
                    let committed = if text.is_empty() { tracked } else { Some(text) };
                    if let Some(committed) = committed.filter(|c| !c.is_empty()) {
                        if debug_enabled() {
                            eprintln!("IME: commit = {committed:?}");
                        }
                        // While searching, committed text is search input
                        // (on Wayland text-input every letter arrives this
                        // way); it must not reach the shell.
                        if self.search.visible {
                            if self.search.state.push_text(&committed)
                                && let Some(pane) = self.active_pane_key()
                            {
                                let query = self.search.state.query.clone();
                                let _ = self
                                    .pump_commands
                                    .send(PumpMsg::SearchQuery { pane, query });
                            }
                            window.request_redraw();
                            return;
                        }
                        if let Some(pane) = self.active_pane_key() {
                            self.suggestions.remove(&pane);
                            self.send_input(pane, committed.into_bytes());
                        }
                    }
                }
                winit::event::Ime::Disabled => {
                    self.ime.cancel();
                    if debug_enabled() {
                        eprintln!("IME: composition cancelled");
                    }
                }
            },
            WindowEvent::Focused(focused) => {
                self.accessibility.set_focused(focused);
                let (outer, inner) = window_bounds(&window);
                self.accessibility.set_bounds(outer, inner);
                // N16b: report the focus change when the engine has
                // focus reporting enabled (`FOCUS_IN_OUT`);
                // `encode_focus_event` returns `None` otherwise. The
                // startup `Focused(true)` arrives before the first
                // `AppEvent::Modes` tick, so it reports nothing.
                if let Some(seq) =
                    encode_focus_event(self.modes.contains(TermMode::FOCUS_IN_OUT), focused)
                    && let Some(pane) = self.active_pane_key()
                {
                    self.send_input(pane, seq.into_bytes());
                }
                // The cursor style (hollow when unfocused) and the ghost
                // text depend on focus.
                window.request_redraw();
            }
            _ => {}
        }
    }
}

/// Whether sending `bytes` for `key` may change the shell's line buffer.
/// Function keys and modifier or lock keys (reported alone under the kitty
/// protocol) do not edit it, so the shell sends no new buffer and the
/// shown suggestion stays valid; every other key may. Escape, and any key
/// whose bytes end in a bare ESC, may too: readline and zle take a
/// trailing ESC as a Meta prefix, so a suggestion accepted next would
/// have its first byte read as Meta+char. A key bound in the shell to an
/// edit still gets a fresh suggestion once the shell reports its buffer.
fn key_may_edit_line(key: &Key, bytes: &[u8]) -> bool {
    use NamedKey::*;
    bytes.last() == Some(&0x1b)
        || !matches!(
            key,
            Key::Named(
                F1 | F2
                    | F3
                    | F4
                    | F5
                    | F6
                    | F7
                    | F8
                    | F9
                    | F10
                    | F11
                    | F12
                    | F13
                    | F14
                    | F15
                    | F16
                    | F17
                    | F18
                    | F19
                    | F20
                    | F21
                    | F22
                    | F23
                    | F24
                    | Shift
                    | Control
                    | Alt
                    | AltGraph
                    | Super
                    | Meta
                    | Hyper
                    | Fn
                    | FnLock
                    | CapsLock
                    | NumLock
                    | ScrollLock
                    | Symbol
                    | SymbolLock
            )
        )
}

/// The focus reports for moving the active pane from `prev` to `next`
/// (each with its modes): `CSI O` to the old pane, then `CSI I` to the new
/// one, each only when its application enabled `FOCUS_IN_OUT`. Nothing
/// while the window is unfocused (neither pane had focus) or when the
/// pane did not change.
fn focus_switch_reports(
    window_focused: bool,
    prev: Option<(PaneKey, TermMode)>,
    next: Option<(PaneKey, TermMode)>,
) -> Vec<(PaneKey, Vec<u8>)> {
    if !window_focused || prev.map(|(pane, _)| pane) == next.map(|(pane, _)| pane) {
        return Vec::new();
    }
    [(prev, false), (next, true)]
        .into_iter()
        .filter_map(|(pane, focused)| {
            let (pane, modes) = pane?;
            let seq = encode_focus_event(modes.contains(TermMode::FOCUS_IN_OUT), focused)?;
            Some((pane, seq.into_bytes()))
        })
        .collect()
}

/// Whether clipboard text read for a paste is worth sending to the pump:
/// not empty and within the paste cap (normalizing newlines never grows
/// it; the pump checks the exact size with bracket markers).
fn pasteable(text: &str) -> bool {
    let max = nora::input::paste::MAX_PASTE_SIZE;
    if text.len() > max && debug_enabled() {
        eprintln!(
            "debug: paste rejected: {} byte(s) exceeds max {max}",
            text.len()
        );
    }
    !text.is_empty() && text.len() <= max
}

/// Whether a suggestion tagged `seq` (the last input the pump had fed to
/// the pane's PTY when it read the buffer) answers the pane's latest input
/// (`last_sent`; `None` = the window never sent the pane input).
fn suggestion_is_current(seq: u64, last_sent: Option<u64>) -> bool {
    seq == last_sent.unwrap_or(0)
}

/// The next word of an inline suggestion: leading spaces, then up to the
/// next space (`"atus --short"` -> `"atus"`, `" --short"` -> `" --short"`).
fn next_word(suffix: &str) -> &str {
    let start = suffix.len() - suffix.trim_start().len();
    let end = suffix[start..]
        .find(char::is_whitespace)
        .map_or(suffix.len(), |i| start + i);
    &suffix[..end]
}

#[cfg(test)]
mod suggestion_tests {
    #[test]
    fn next_word_takes_leading_space_and_one_word() {
        assert_eq!(super::next_word("atus --short"), "atus");
        assert_eq!(super::next_word(" --short"), " --short");
        assert_eq!(super::next_word("  a b"), "  a");
        assert_eq!(super::next_word("x"), "x");
    }

    #[test]
    fn oversized_or_empty_clipboard_text_is_not_pasted() {
        let max = nora::input::paste::MAX_PASTE_SIZE;
        assert!(super::pasteable("ls"));
        assert!(super::pasteable(&"x".repeat(max)));
        assert!(!super::pasteable(&"x".repeat(max + 1)));
        assert!(!super::pasteable(""));
    }

    #[test]
    fn suggestion_for_an_older_input_is_stale() {
        use super::suggestion_is_current;
        // A pane the window never typed into: the pump's tag is 0.
        assert!(suggestion_is_current(0, None));
        // "git s" was input 4; "t" (input 5) is sent; the report for
        // "git s" arrives tagged 4 and must be dropped.
        assert!(!suggestion_is_current(4, Some(5)));
        // The echo of "t" is read after input 5 was fed.
        assert!(suggestion_is_current(5, Some(5)));
    }
}

#[cfg(test)]
mod focus_tests {
    use super::{FocusDir, LayoutNode, PaneId, SplitDirection, adjacent_pane};

    fn leaf(id: u32) -> LayoutNode {
        LayoutNode::Leaf(PaneId(id))
    }

    fn split(direction: SplitDirection, first: LayoutNode, second: LayoutNode) -> LayoutNode {
        LayoutNode::Split {
            direction,
            ratio: 0.5,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    #[test]
    fn nearer_sibling_wins_over_outer_split() {
        // H{A, H{B, C}}: left of C is B, left of B is A.
        let tree = split(
            SplitDirection::Horizontal,
            leaf(0),
            split(SplitDirection::Horizontal, leaf(1), leaf(2)),
        );
        assert_eq!(
            adjacent_pane(&tree, PaneId(2), FocusDir::Left),
            Some(PaneId(1))
        );
        assert_eq!(
            adjacent_pane(&tree, PaneId(1), FocusDir::Left),
            Some(PaneId(0))
        );
        assert_eq!(
            adjacent_pane(&tree, PaneId(0), FocusDir::Right),
            Some(PaneId(1))
        );
        assert_eq!(
            adjacent_pane(&tree, PaneId(1), FocusDir::Right),
            Some(PaneId(2))
        );
        assert_eq!(adjacent_pane(&tree, PaneId(0), FocusDir::Left), None);
        assert_eq!(adjacent_pane(&tree, PaneId(2), FocusDir::Right), None);
    }

    #[test]
    fn subtree_is_entered_at_its_near_edge() {
        // H{H{0, 2}, 1}: left of 1 is 2 (the near edge), not 0.
        let tree = split(
            SplitDirection::Horizontal,
            split(SplitDirection::Horizontal, leaf(0), leaf(2)),
            leaf(1),
        );
        assert_eq!(
            adjacent_pane(&tree, PaneId(1), FocusDir::Left),
            Some(PaneId(2))
        );
        assert_eq!(
            adjacent_pane(&tree, PaneId(2), FocusDir::Right),
            Some(PaneId(1))
        );
        // V{V{0, 2}, 1}: up from 1 is 2.
        let tree = split(
            SplitDirection::Vertical,
            split(SplitDirection::Vertical, leaf(0), leaf(2)),
            leaf(1),
        );
        assert_eq!(
            adjacent_pane(&tree, PaneId(1), FocusDir::Up),
            Some(PaneId(2))
        );
        assert_eq!(
            adjacent_pane(&tree, PaneId(0), FocusDir::Down),
            Some(PaneId(2))
        );
    }

    #[test]
    fn perpendicular_split_is_crossed_to_the_outer_sibling() {
        // H{A, V{B, C}}: left of C is A; up from C is B.
        let tree = split(
            SplitDirection::Horizontal,
            leaf(0),
            split(SplitDirection::Vertical, leaf(1), leaf(2)),
        );
        assert_eq!(
            adjacent_pane(&tree, PaneId(2), FocusDir::Left),
            Some(PaneId(0))
        );
        assert_eq!(
            adjacent_pane(&tree, PaneId(2), FocusDir::Up),
            Some(PaneId(1))
        );
        assert_eq!(adjacent_pane(&tree, PaneId(1), FocusDir::Up), None);
    }
}

#[cfg(test)]
mod focus_report_tests {
    use super::{PaneId, PaneKey, TabId, TermMode, focus_switch_reports};

    fn pane(tab: u32, pane: u32) -> PaneKey {
        PaneKey {
            tab: TabId(tab),
            pane: PaneId(pane),
        }
    }

    const ON: TermMode = TermMode::FOCUS_IN_OUT;
    const OFF: TermMode = TermMode::SHOW_CURSOR;

    #[test]
    fn a_pane_switch_reports_out_then_in() {
        let (a, b) = (pane(0, 0), pane(0, 1));
        assert_eq!(
            focus_switch_reports(true, Some((a, ON)), Some((b, ON))),
            vec![(a, b"\x1b[O".to_vec()), (b, b"\x1b[I".to_vec())]
        );
        // Only panes whose application asked for reports hear them.
        assert_eq!(
            focus_switch_reports(true, Some((a, OFF)), Some((b, ON))),
            vec![(b, b"\x1b[I".to_vec())]
        );
        assert_eq!(
            focus_switch_reports(true, Some((a, ON)), None),
            vec![(a, b"\x1b[O".to_vec())],
            "a switch to an empty layout still reports focus out"
        );
    }

    #[test]
    fn only_keys_that_may_edit_drop_the_suggestion() {
        use super::{Key, NamedKey, key_may_edit_line};
        for (key, bytes) in [
            (NamedKey::F2, &b"\x1bOQ"[..]),
            (NamedKey::F5, b"\x1b[15~"),
            (NamedKey::Shift, b"\x1b[57441u"),
            (NamedKey::Control, b""),
        ] {
            assert!(!key_may_edit_line(&Key::Named(key), bytes), "{key:?}");
        }
        for (key, bytes) in [
            (Key::Character("a".into()), &b"a"[..]),
            (Key::Named(NamedKey::Backspace), b"\x7f"),
            (Key::Named(NamedKey::Enter), b"\r"),
            (Key::Named(NamedKey::ArrowLeft), b"\x1b[D"),
            (Key::Named(NamedKey::Space), b" "),
            // The shell takes ESC as a Meta prefix: an accepted
            // suggestion's first byte would become Meta+char.
            (Key::Named(NamedKey::Escape), b"\x1b"),
            (Key::Named(NamedKey::Escape), b"\x1b\x1b"),
        ] {
            assert!(key_may_edit_line(&key, bytes), "{key:?}");
        }
        // Whatever the key, bytes ending in a bare ESC leave a pending
        // Meta prefix in the shell.
        for key in [NamedKey::Alt, NamedKey::F1] {
            assert!(key_may_edit_line(&Key::Named(key), b"\x1b"), "{key:?}");
        }
    }

    #[test]
    fn nothing_without_window_focus_or_a_change() {
        let (a, b) = (pane(0, 0), pane(1, 0));
        assert!(focus_switch_reports(false, Some((a, ON)), Some((b, ON))).is_empty());
        assert!(focus_switch_reports(true, Some((a, ON)), Some((a, ON))).is_empty());
    }
}
