//! nora window/application state (N01).
//!
//! Creates a real window on the available native backend, binds a wgpu
//! surface to it and reports backend/device metadata. No terminal content is
//! rendered yet; the terminal arrives with the N03 engine adapter.

pub mod glyphs;
mod graphics;

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

use self::graphics::{CellFill, DrawCell, Graphics};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::TermMode;
use nora::clipboard::{ClipboardBackend, create_clipboard};
use nora::engine::{KittyKeyboardFlags, Snapshot};
use nora::input::bindings::{Action as UiAction, Bindings, Direction};
use nora::input::focus::encode_focus_event;
use nora::input::mouse::{
    MouseEncoding, MouseEvent, MouseKind, MouseMode, button_number, encode_mouse_event,
    logical_to_cell, mouse_modifiers, physical_to_cell, should_report, track_button,
    wheel_directions,
};
use nora::input::{BindingTable, InputAction, encode};
use nora::layout::{Layout, LayoutNode, PaneId, SplitDirection};
use nora::render::palette::Palette;
use nora::render::policy::snapshot_is_stale;
use nora::render::state::{Action, RenderEvent, RenderState, SurfaceStatus, on_event};
use nora::render::style::cell_style;
use nora::render::term::{TermLayout, cursor_fill, default_bg};
use nora::session::pump::{AppEvent, PaneKey, PumpMsg};
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

/// The grid that fits the window's current *logical* size (N11a): the
/// mono cell metrics in [`glyphs`] are logical pixels, so the derivation
/// converts the physical inner size to logical first — correct at every
/// scale factor, not just 1.0.
pub(crate) fn window_grid(window: &Window, fonts: &glyphs::Fonts) -> (u16, u16) {
    let logical = window.inner_size().to_logical::<f64>(window.scale_factor());
    glyphs::grid_size(
        logical.width as u32,
        logical.height as u32,
        fonts.cell_w,
        fonts.cell_h(),
    )
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

/// The first leaf of `node` in pre-order (N24b): the leftmost/topmost pane.
fn first_leaf(node: &LayoutNode) -> Option<PaneId> {
    match node {
        LayoutNode::Leaf(id) => Some(*id),
        LayoutNode::Split { first, .. } => first_leaf(first),
    }
}

/// Find the pane adjacent to `active` in `dir` (N24b), walking the layout
/// tree. Returns `None` when the active pane is at the edge in that
/// direction (no adjacent pane). For a split whose sibling is itself a
/// split, the sibling's first leaf (pre-order) is chosen.
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
                (true, false) => {
                    if matches!(
                        (dir, direction),
                        (FocusDir::Right, SplitDirection::Horizontal)
                            | (FocusDir::Down, SplitDirection::Vertical)
                    ) {
                        first_leaf(second)
                    } else {
                        adjacent_pane(first, active, dir)
                    }
                }
                (false, true) => {
                    if matches!(
                        (dir, direction),
                        (FocusDir::Left, SplitDirection::Horizontal)
                            | (FocusDir::Up, SplitDirection::Vertical)
                    ) {
                        first_leaf(first)
                    } else {
                        adjacent_pane(second, active, dir)
                    }
                }
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
            let mut fonts = glyphs::Fonts::load();
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
                    1.0,
                    nora::config::ZOOM_DEFAULT,
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
    /// The last cell a motion report was sent for (one report per cell).
    last_motion_cell: Option<(u16, u16)>,
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
    /// Live kitty keyboard protocol flags (N21b): the flag source passed
    /// to `encode_kitty`. Initialized to `NONE` (legacy) to match the
    /// default; the first pump tick publishes the engine's live flags
    /// within ~10 ms.
    kitty_flags: KittyKeyboardFlags,
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
    /// The last grid sent to the pump as `PumpMsg::Resize` (N11a);
    /// `(0, 0)` = never sent. The `Resized`/`ScaleFactorChanged` arms
    /// send only when the derived (clamped) grid differs, so repeated
    /// no-op derivations emit nothing.
    last_grid: (u16, u16),
    /// Selection drag start in grid `(row, col)` (N14b): `Some` while the
    /// primary button is held down dragging a selection; `None` otherwise.
    selection: Option<(usize, usize)>,
    /// Last cursor position in logical pixels (N14b): `winit` reports
    /// `CursorMoved` in physical pixels and `MouseInput` carries no
    /// position, so the drag tracks the last moved cursor.
    cursor: (f64, f64),
    /// Mouse buttons currently pressed, in press order (N16b): motion
    /// events report the first held button; each release removes its
    /// button.
    pressed_buttons: Vec<u8>,
    /// Native clipboard backend (N15): `None` when no display server is
    /// available; clipboard operations are no-ops in that case.
    clipboard: Option<Arc<dyn ClipboardBackend>>,
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
    pane_slots: std::collections::HashMap<PaneKey, Arc<Mutex<Option<Snapshot>>>>,
    /// N24b: the last grid sent to the pump as `PumpMsg::Resize` for that
    /// pane; `(0, 0)` = never sent.
    pane_grid: std::collections::HashMap<PaneKey, (u16, u16)>,
    /// N24b: the pump's `ChildExit` payload once delivered for that pane
    /// (`None` = not yet delivered).
    pane_exit: std::collections::HashMap<PaneKey, Option<Option<i32>>>,
    /// N24b: per-pane render state machine; `DirtyPulse`/`ChildExit` fold
    /// into the pane's own state.
    pane_render: std::collections::HashMap<PaneKey, RenderState>,
    /// N24b: per-pane IME state machine; each pane's preedit/commit is
    /// independent.
    pane_ime: std::collections::HashMap<PaneKey, nora::ime::ImeState>,
    /// N24b: per-pane search UI; each pane's search is independent.
    pane_search: std::collections::HashMap<PaneKey, nora::search::SearchUi>,
    /// N24b: per-pane live terminal `TermMode` (N10b); the mode source
    /// passed to `encode` for that pane.
    pane_modes: std::collections::HashMap<PaneKey, TermMode>,
    /// N24b: per-pane live kitty keyboard flags (N21b); the flag source
    /// passed to `encode_kitty` for that pane.
    pane_kitty: std::collections::HashMap<PaneKey, KittyKeyboardFlags>,
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
    config: &nora::config::NoraConfig,
) -> std::process::ExitCode {
    if debug_enabled() {
        eprintln!(
            "debug: config loaded: history={}, font.size={}",
            config.terminal.history, config.font.size
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
    let mut pane_ime = std::collections::HashMap::new();
    pane_ime.insert(first_pane, nora::ime::ImeState::new());
    let mut pane_search = std::collections::HashMap::new();
    pane_search.insert(first_pane, nora::search::SearchUi::new());
    let mut pane_modes = std::collections::HashMap::new();
    pane_modes.insert(first_pane, TermMode::SHOW_CURSOR);
    let mut pane_kitty = std::collections::HashMap::new();
    pane_kitty.insert(first_pane, KittyKeyboardFlags::NONE);
    let mut pane_selection = std::collections::HashMap::new();
    pane_selection.insert(first_pane, None);
    let mut pane_last_visible = std::collections::HashMap::new();
    pane_last_visible.insert(first_pane, Vec::new());

    let mut app = WinApp {
        slot,
        pump_commands,
        stop,
        state: RenderState::default(),
        mods: ModifiersState::default(),
        modes: TermMode::SHOW_CURSOR,
        kitty_flags: KittyKeyboardFlags::NONE,
        fonts: None,
        graphics: None,
        window: Some(window),
        failed: false,
        frames: 0,
        child_exit: None,
        hold,
        last_grid: (0, 0),
        selection: None,
        cursor: (0.0, 0.0),
        pressed_buttons: Vec::new(),
        clipboard: create_clipboard(),
        zoom: nora::config::ZOOM_DEFAULT,
        palette: Palette::from_theme(&config.theme),
        bindings: Bindings::default(),
        last_motion_cell: None,
        search: nora::search::SearchUi::new(),
        last_visible: Vec::new(),
        ime: nora::ime::ImeState::new(),
        workspace,
        layout,
        tab_layouts,
        pane_slots,
        pane_grid,
        pane_exit,
        pane_render,
        pane_ime,
        pane_search,
        pane_modes,
        pane_kitty,
        pane_selection,
        pane_last_visible,
    };
    match event_loop.run_app(&mut app) {
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
    }
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
        self.pane_ime.insert(pane, nora::ime::ImeState::new());
        self.pane_search.insert(pane, nora::search::SearchUi::new());
        self.pane_modes.insert(pane, TermMode::SHOW_CURSOR);
        self.pane_kitty.insert(pane, KittyKeyboardFlags::NONE);
        self.pane_selection.insert(pane, None);
        self.pane_last_visible.insert(pane, Vec::new());
    }

    /// Drop a pane's per-pane state (N24b); its session is reaped by the
    /// pump.
    fn drop_pane_state(&mut self, pane: PaneKey) {
        self.pane_slots.remove(&pane);
        self.pane_grid.remove(&pane);
        self.pane_exit.remove(&pane);
        self.pane_render.remove(&pane);
        self.pane_ime.remove(&pane);
        self.pane_search.remove(&pane);
        self.pane_modes.remove(&pane);
        self.pane_kitty.remove(&pane);
        self.pane_selection.remove(&pane);
        self.pane_last_visible.remove(&pane);
    }

    /// Save the active pane's mirrored state into its per-pane maps (N24b).
    fn save_active_pane(&mut self, pane: PaneKey) {
        self.pane_slots.insert(pane, self.slot.clone());
        self.pane_grid.insert(pane, self.last_grid);
        self.pane_exit.insert(pane, self.child_exit);
        self.pane_render.insert(pane, self.state.clone());
        self.pane_ime.insert(
            pane,
            std::mem::replace(&mut self.ime, nora::ime::ImeState::new()),
        );
        self.pane_search.insert(
            pane,
            std::mem::replace(&mut self.search, nora::search::SearchUi::new()),
        );
        self.pane_modes.insert(pane, self.modes);
        self.pane_kitty.insert(pane, self.kitty_flags);
        self.pane_selection.insert(pane, self.selection);
        self.pane_last_visible
            .insert(pane, std::mem::take(&mut self.last_visible));
    }

    /// Load `pane`'s per-pane state into the single fields (N24b), making
    /// it the active pane's state.
    fn load_active_pane(&mut self, pane: PaneKey) {
        self.slot = self
            .pane_slots
            .remove(&pane)
            .unwrap_or_else(|| self.slot.clone());
        self.last_grid = self.pane_grid.remove(&pane).unwrap_or((0, 0));
        self.child_exit = self.pane_exit.remove(&pane).unwrap_or(None);
        self.state = self.pane_render.remove(&pane).unwrap_or_default();
        self.ime = self.pane_ime.remove(&pane).unwrap_or_default();
        self.search = self.pane_search.remove(&pane).unwrap_or_default();
        self.modes = self
            .pane_modes
            .remove(&pane)
            .unwrap_or(TermMode::SHOW_CURSOR);
        self.kitty_flags = self
            .pane_kitty
            .remove(&pane)
            .unwrap_or(KittyKeyboardFlags::NONE);
        self.selection = self.pane_selection.remove(&pane).unwrap_or(None);
        self.last_visible = self.pane_last_visible.remove(&pane).unwrap_or_default();
    }

    /// Compute each live pane's grid from the active layout and send a
    /// `PumpMsg::Resize` to every pane whose grid changed (N24b). Called
    /// after a split/close/focus and on window resize, so panes always
    /// match their rectangles.
    fn resize_all_panes(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let Some(fonts) = self.fonts.as_ref() else {
            return;
        };
        let (cols, rows) = window_grid(window, fonts);
        if cols == 0 || rows == 0 {
            return;
        }
        for (pane_id, _x, _y, pw, ph) in self.layout.compute_rectangles(cols, rows) {
            let pane = PaneKey {
                tab: self.active_tab_id().unwrap_or(TabId(0)),
                pane: pane_id,
            };
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
        let new_tab_grid = if cmd == nora::workspace::TabCommand::New {
            let window = self.window.as_ref().expect("the window exists");
            let fonts = self.fonts.as_ref().expect("fonts are created at startup");
            Some(window_grid(window, fonts))
        } else {
            None
        };
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
                    if let Some((cols, rows)) = new_tab_grid {
                        let slot: Arc<Mutex<Option<Snapshot>>> = Arc::new(Mutex::new(None));
                        self.pane_slots.insert(new_pane, slot.clone());
                        self.pane_grid.insert(new_pane, (cols, rows));
                        let _ = self.pump_commands.send(PumpMsg::NewPane {
                            pane: new_pane,
                            rows: rows as usize,
                            cols: cols as usize,
                            slot,
                        });
                    }
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
    }

    /// Split the active pane (N24b): mint a new pane in the active layout,
    /// spawn its session, and make it active. `vertical` = new pane below
    /// (split_vertical); otherwise new pane to the right (split_horizontal).
    fn split_active_pane(&mut self, vertical: bool) {
        let Some(tab) = self.active_tab_id() else {
            return;
        };
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
        // The layout changed: recompute every pane's grid and spawn the new
        // pane's session at its (now known) size.
        self.resize_all_panes();
        let (cols, rows) = self.pane_grid.get(&new_pane).copied().unwrap_or((0, 0));
        if cols > 0 && rows > 0 {
            let slot: Arc<Mutex<Option<Snapshot>>> = Arc::new(Mutex::new(None));
            self.pane_slots.insert(new_pane, slot.clone());
            let _ = self.pump_commands.send(PumpMsg::NewPane {
                pane: new_pane,
                rows: rows as usize,
                cols: cols as usize,
                slot,
            });
        }
        // The new pane becomes active; load its (fresh) state.
        self.load_active_pane(new_pane);
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
        } else {
            // The layout is empty: reset the single fields to a fresh pane.
            self.slot = Arc::new(Mutex::new(None));
            self.last_grid = (0, 0);
            self.child_exit = None;
            self.state = RenderState::default();
            self.ime = nora::ime::ImeState::new();
            self.search = nora::search::SearchUi::new();
            self.modes = TermMode::SHOW_CURSOR;
            self.kitty_flags = KittyKeyboardFlags::NONE;
        }
    }

    /// Focus an adjacent pane (N24b): `dir` is Left/Right/Up/Down. The
    /// target is the sibling (or ancestor's sibling) in that direction,
    /// found by walking the active layout tree.
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
            UiAction::ToggleSearch => self.search.toggle(),
            UiAction::ZoomIn => self.zoom = nora::config::zoom_in(self.zoom),
            UiAction::ZoomOut => self.zoom = nora::config::zoom_out(self.zoom),
            UiAction::ZoomReset => self.zoom = nora::config::zoom_reset(),
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

    /// Paste the clipboard into the active pane (bracketed when the
    /// application enabled DECSET 2004). The pump validates and streams
    /// the text in order, bounded by the writer's app cap.
    fn paste_clipboard(&mut self) {
        let text = self.clipboard.as_ref().and_then(|cb| cb.get_text().ok());
        let Some(text) = text else {
            if debug_enabled() {
                eprintln!("debug: paste: clipboard unavailable or empty");
            }
            return;
        };
        let bracketed = self.modes.contains(TermMode::BRACKETED_PASTE);
        if let Some(pane) = self.active_pane_key() {
            let _ = self.pump_commands.send(PumpMsg::Paste {
                pane,
                text,
                bracketed,
            });
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
            AppEvent::DirtyPulse { pane } => {
                if self.pane_is_live(&pane) {
                    if self.active_pane_key() == Some(pane) {
                        let (state, action) =
                            on_event(self.state.clone(), RenderEvent::Dirty { non_noop: true });
                        self.state = state;
                        match action {
                            Action::Render => {
                                let window = self.window.as_ref().expect("the window exists");
                                window.request_redraw(); // redraw SITE 1
                                self.attempt_render(event_loop);
                            }
                            Action::Skip => {}
                            Action::ExitFail => {
                                self.failed = true;
                                event_loop.exit();
                            }
                            Action::Reconfigure | Action::Close => {
                                // A dirty pulse never reconfigures or
                                // closes; the resize / child-exit arms
                                // own those.
                            }
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
            AppEvent::KittyModes { pane, flags } => {
                if self.pane_is_live(&pane) {
                    if self.active_pane_key() == Some(pane) {
                        self.kitty_flags = flags;
                    } else {
                        self.pane_kitty.insert(pane, flags);
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
    fn grid_cell_at(&self, window: &Window, x: f64, y: f64) -> Option<(usize, usize)> {
        let fonts = self.fonts.as_ref()?;
        let cell_w = fonts.cell_w as f64;
        let cell_h = fonts.cell_h() as f64;
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return None;
        }
        let col = (x / cell_w) as usize;
        let row = (y / cell_h) as usize;
        let (cols, rows) = window_grid(window, fonts);
        if col >= cols as usize || row >= rows as usize {
            return None;
        }
        Some((row, col))
    }

    /// Encode one mouse event against the engine's live mouse mode and
    /// the current window grid, and send the bytes to the PTY (N16b).
    /// No-op when Shift is held (the event belongs to local selection),
    /// the terminal reports no mouse, or the fonts are not loaded yet.
    fn send_mouse_event(&self, event: &MouseEvent, window: &Window) {
        let mode = MouseMode::from_term_mode(self.modes);
        if !should_report(mode, self.mods.shift_key()) {
            return;
        }
        let Some(fonts) = self.fonts.as_ref() else {
            return;
        };
        let (cols, rows) = window_grid(window, fonts);
        let encoding = MouseEncoding::from_term_mode(self.modes);
        if let Some(bytes) = encode_mouse_event(mode, encoding, event, cols, rows)
            && let Some(pane) = self.active_pane_key()
        {
            let _ = self.pump_commands.send(PumpMsg::Input { pane, bytes });
        }
    }

    /// Render the pending snapshot through one acquire → fold → draw →
    /// present pass, following the pure machine's [`Action`].
    fn attempt_render(&mut self, event_loop: &ActiveEventLoop) {
        // N24b: read the active pane's slot first (before the graphics
        // borrow); the pump stores each pane's latest snapshot in its own
        // slot.
        let slot = match self.active_pane_key() {
            Some(pane) => self
                .pane_slots
                .get(&pane)
                .cloned()
                .unwrap_or_else(|| self.slot.clone()),
            None => self.slot.clone(),
        };
        let graphics = self
            .graphics
            .as_mut()
            .expect("graphics are created at startup");
        let window = self.window.as_ref().expect("the window exists");
        let mut frame = graphics.acquire();
        let (state, action) = on_event(
            self.state.clone(),
            RenderEvent::SurfaceStatus(frame.status.clone()),
        );
        self.state = state;
        match action {
            Action::Render => {
                let snap = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
                let Some(mut snap) = snap else {
                    // Coalesced pulse: the pump's single slot held one
                    // snapshot for two rapid pulses (e.g. the
                    // initial-state damage plus the first command's
                    // output); the earlier pulse already presented
                    // the latest content. Nothing to present — never
                    // a blank frame.
                    if debug_enabled() {
                        eprintln!("debug: coalesced pulse, nothing new to render");
                    }
                    return;
                };
                // Resolve against a full table: the engine keeps only the
                // colors an application changed (OSC 4/10/11/12).
                snap.colors = self.palette.overlay(&snap.colors);
                // N11a stale-geometry rejection: the geometry pair is the
                // staleness tag — never present old geometry to the new
                // surface. Fonts are shared-borrowed here, then mutably
                // borrowed below for drawing; never both at once.
                let (grid_cols, grid_rows) = {
                    let fonts = self.fonts.as_ref().expect("fonts are loaded at startup");
                    window_grid(window, fonts)
                };
                if snapshot_is_stale(snap.rows as u16, snap.cols as u16, grid_rows, grid_cols) {
                    if debug_enabled() {
                        eprintln!(
                            "debug: stale snapshot {}x{} != grid {}x{}",
                            snap.rows, snap.cols, grid_rows, grid_cols
                        );
                    }
                    return;
                }
                // N20b: the search is bounded to the visible area —
                // refresh the character snapshot of what was just taken
                // and re-run an active search against it (the content
                // may have scrolled). Field-level calls only: the
                // `graphics`/`window` borrows above are still live, so
                // no `&mut self` methods here.
                let mut visible: Vec<Vec<char>> = Vec::with_capacity(snap.rows);
                for row in 0..snap.rows {
                    let mut line = Vec::with_capacity(snap.cols);
                    for col in 0..snap.cols {
                        line.push(snap.cells[row * snap.cols + col].ch);
                    }
                    visible.push(line);
                }
                self.last_visible = visible;
                if self.search.visible && !self.search.state.query.is_empty() {
                    self.search.research(&self.last_visible);
                }
                // N20b: highlight span of the current match, in viewport
                // (row, col) cells; empty when there is no current match.
                let mut search_cells: Vec<(u32, u32)> = Vec::new();
                if let Some(m) = self.search.current_match() {
                    let rows = self.last_visible.len();
                    let cols = self.last_visible.first().map_or(0, |row| row.len());
                    if m.row < rows {
                        let start = m.col.min(cols);
                        let end = (start + self.search.state.query.chars().count()).min(cols);
                        search_cells.extend((start..end).map(|col| (m.row as u32, col as u32)));
                    }
                }
                let fonts = self.fonts.as_mut().expect("fonts are loaded at startup");
                // Row-major visible cells (N13a): each cell's full cluster is
                // shaped with its resolved style; spacers and concealed cells
                // are skipped, wide chars advance by 2.
                let mut draw_cells: Vec<DrawCell> = Vec::new();
                // Cell backgrounds that differ from the default background
                // (the frame is cleared to it), then the block cursor.
                let default_bg = default_bg(&snap);
                let default_bg = [default_bg.r, default_bg.g, default_bg.b];
                let mut fills: Vec<CellFill> = Vec::new();
                for row in 0..snap.rows {
                    for col in 0..snap.cols {
                        let cell = &snap.cells[row * snap.cols + col];
                        // Spacer cells (wide-char continuation) are not drawn.
                        if cell.width() == 0 {
                            continue;
                        }
                        let style = cell_style(cell.flags, cell.fg, cell.bg, &snap.colors);
                        if style.bg[..3] != default_bg {
                            fills.push(CellFill {
                                row: row as u32,
                                col: col as u32,
                                span: cell.width(),
                                color: style.bg,
                            });
                        }
                        // Spaces carry no content.
                        if cell.ch == ' ' {
                            continue;
                        }
                        // Concealed cells are not drawn.
                        if style.conceal {
                            continue;
                        }
                        // Full cluster: the cell character plus its combining
                        // marks, shaped together so the marks compose onto
                        // the base character.
                        let mut text = String::new();
                        text.push(cell.ch);
                        for &mark in &cell.zerowidth {
                            text.push(mark);
                        }
                        draw_cells.push(DrawCell {
                            row: row as u32,
                            col: col as u32,
                            text,
                            style,
                            width: cell.width(),
                        });
                    }
                }
                // Block cursor: `cursor_fill` applies the engine's
                // `SHOW_CURSOR` mode and range rules and resolves the cursor
                // color; the glyph under it is drawn in the cell background
                // so it stays legible on the block.
                let layout = TermLayout {
                    cell_w: fonts.cell_w as f32,
                    cell_h: glyphs::CELL_H,
                    font_size: glyphs::zoomed_font_size(self.zoom),
                };
                if let Some(cursor) = cursor_fill(&snap, &layout) {
                    let (row, col) = (cursor.row as u32, cursor.col as u32);
                    fills.push(CellFill {
                        row,
                        col,
                        span: cursor.span,
                        color: [cursor.color.r, cursor.color.g, cursor.color.b, 255],
                    });
                    if let Some(cell) = draw_cells
                        .iter_mut()
                        .find(|cell| cell.row == row && cell.col == col)
                    {
                        cell.style.fg = cell.style.bg;
                    }
                }
                // N14b: selection highlight — the grid cells covered by the
                // current selection, drawn as fill rectangles under the text.
                let mut selection_cells: Vec<(u32, u32)> = Vec::new();
                for (i, is_selected) in snap.selected.iter().enumerate() {
                    if *is_selected {
                        let (row, col) = (i / snap.cols, i % snap.cols);
                        selection_cells.push((row as u32, col as u32));
                    }
                }
                let line1 = first_non_space_line(&snap, 40);
                if let Err(message) = graphics.draw_frame(
                    &mut frame,
                    fonts,
                    &draw_cells,
                    [default_bg[0], default_bg[1], default_bg[2], 255],
                    &fills,
                    &selection_cells,
                    &search_cells,
                    window.scale_factor() as f32,
                    self.zoom,
                ) {
                    eprintln!("nora: frame draw failed: {message}");
                }
                graphics.present(frame);
                self.frames += 1;
                if debug_enabled() {
                    eprintln!(
                        "debug: frame rendered {} (rows={} cols={} line1=\"{}\")",
                        self.frames, snap.rows, snap.cols, line1
                    );
                }
            }
            Action::Reconfigure => {
                // Suboptimal: the frame carries a texture, so drop it
                // before `set_size` (Surface::configure panics while a
                // SurfaceTexture is alive). No present: no stale clear
                // frame is needed; the next attempt re-acquires.
                drop(frame);
                let size = window.inner_size();
                if let Err(message) = graphics.set_size(size) {
                    eprintln!("nora: surface reconfiguration failed: {message}");
                }
                window.request_redraw(); // redraw SITE 3
            }
            Action::ExitFail => {
                self.failed = true;
                event_loop.exit();
            }
            Action::Skip => {
                // No retry budget: drop the frame and wait for the next
                // event.
                drop(frame);
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

        let mut graphics =
            match pollster::block_on(Graphics::new(event_loop, &window, |device, surface| {
                eprintln!("nora: device lost: {device:?} {surface}");
            })) {
                Ok(graphics) => graphics,
                Err(message) => {
                    eprintln!("nora: {message}");
                    self.failed = true;
                    event_loop.exit();
                    return;
                }
            };

        let fonts = glyphs::Fonts::load();
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
            "surface: {:?} at {:?}, scale {:.2}",
            graphics.format(),
            window.inner_size(),
            window.scale_factor(),
        );
        println!("fonts: cell {}x{}", fonts.cell_w, fonts.cell_h());

        self.graphics = Some(graphics);
        self.fonts = Some(fonts);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        // N24b: exit the window when the last pane's child exits (and not
        // holding); the pane is still in its layout at this point.
        let last_pane_exit = match &event {
            AppEvent::ChildExit { pane, .. } => {
                self.pane_is_live(pane)
                    && self.workspace.tab_count() <= 1
                    && self
                        .tab_layouts
                        .get(&pane.tab)
                        .map_or(self.layout.panes().len(), |l| l.panes().len())
                        <= 1
            }
            _ => false,
        };
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
            WindowEvent::Resized(size) => {
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
                // GPU, no display).
                let (cols, rows) = match self.fonts.as_ref() {
                    Some(fonts) => window_grid(&window, fonts),
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
                if let Some(pane) = self.active_pane_key() {
                    self.last_grid = self.pane_grid.get(&pane).copied().unwrap_or((0, 0));
                }

                // (e) Coalesce the redraw.
                window.request_redraw(); // redraw SITE 2
                if debug_enabled() {
                    eprintln!("debug: resize {rows}x{cols}");
                }
            }
            // N11a: the window's scale factor changed (monitor move, DPI
            // change). The physical size is unchanged — no `set_size`, no
            // reconfigure; only the logical grid is re-derived, so the
            // same surface now shows a different cell count.
            WindowEvent::ScaleFactorChanged {
                scale_factor: _,
                inner_size_writer: _,
            } => {
                // (a) The scale-change carries a usable window size,
                // exactly like a `Resized` event.
                if self.state.zero_size || self.state.surface_retry >= 3 {
                    return;
                }
                let (cols, rows) = match self.fonts.as_ref() {
                    Some(fonts) => window_grid(&window, fonts),
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
                if let Some(pane) = self.active_pane_key() {
                    self.last_grid = self.pane_grid.get(&pane).copied().unwrap_or((0, 0));
                }
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
                // N09b: act only on real presses; held-key focus-gain
                // presses are synthetic duplicates.
                if event.state != ElementState::Pressed || is_synthetic {
                    return;
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
                        if self.workspace.tab_count() == 0 {
                            // The last pane closed: the window goes too.
                            self.stop.store(true, Ordering::Relaxed);
                            event_loop.exit();
                        }
                        return;
                    }
                }
                // While the search bar is open it owns text input.
                if self.search.visible {
                    match &event.logical_key {
                        Key::Character(text) => {
                            self.search.input(text, &self.last_visible);
                            return;
                        }
                        Key::Named(NamedKey::Enter) => {
                            if self.mods.shift_key() {
                                self.search.prev();
                            } else {
                                self.search.next();
                            }
                            return;
                        }
                        Key::Named(NamedKey::Backspace) => {
                            self.search.backspace(&self.last_visible);
                            return;
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.search.close();
                            return;
                        }
                        _ => {}
                    }
                }
                if let Some(action) = encode(
                    &event.logical_key,
                    &event.physical_key,
                    &self.mods,
                    event.repeat,
                    &BindingTable::default(),
                    self.modes,
                ) {
                    match action {
                        InputAction::Send(bytes) => {
                            if let Some(pane) = self.active_pane_key() {
                                let _ = self.pump_commands.send(PumpMsg::Input { pane, bytes });
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
                let kinds = wheel_directions(delta);
                if kinds.is_empty() {
                    return;
                }
                // N16b: when the terminal reports mouse (and Shift is
                // not held), the application owns the scroll: report
                // the wheel event(s) at the last tracked cursor
                // position. Otherwise the wheel scrolls the local
                // scrollback (N14a).
                if should_report(MouseMode::from_term_mode(self.modes), self.mods.shift_key()) {
                    let (x, y) = self.cursor;
                    if let Some(fonts) = self.fonts.as_ref()
                        && let Some((col, row)) =
                            logical_to_cell(x, y, fonts.cell_w, fonts.cell_h())
                    {
                        for kind in kinds {
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
                // tick.
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, dy) => (dy as i32) * 3,
                    MouseScrollDelta::PixelDelta(dy) => (dy.y / 3.0) as i32,
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
                        let _ = self.pump_commands.send(PumpMsg::Input { pane, bytes });
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
                let scale = window.scale_factor();
                let (x, y) = (position.x / scale, position.y / scale);
                self.cursor = (x, y);
                if let Some((start_row, start_col)) = self.selection
                    && self.pressed_buttons.contains(&0)
                    && let Some((end_row, end_col)) = self.grid_cell_at(&window, x, y)
                    && (end_row, end_col) != (start_row, start_col)
                    && let Some(pane) = self.active_pane_key()
                {
                    let _ = self.pump_commands.send(PumpMsg::Select {
                        pane,
                        start_row,
                        start_col,
                        end_row,
                        end_col,
                    });
                }
                // N16b: report the motion to the terminal (gated by the
                // engine's mouse mode inside `send_mouse_event`; `X10`
                // and `Normal` encode to nothing without a held
                // button).
                if let Some(fonts) = self.fonts.as_ref()
                    && let Some((col, row)) = physical_to_cell(
                        position.x,
                        position.y,
                        scale,
                        fonts.cell_w,
                        fonts.cell_h(),
                    )
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
                        if let Some(pane) = self.active_pane_key() {
                            let _ = self.pump_commands.send(PumpMsg::Input {
                                pane,
                                bytes: committed.into_bytes(),
                            });
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
                // N16b: report the focus change when the engine has
                // focus reporting enabled (`FOCUS_IN_OUT`);
                // `encode_focus_event` returns `None` otherwise. The
                // startup `Focused(true)` arrives before the first
                // `AppEvent::Modes` tick, so it reports nothing.
                if let Some(seq) =
                    encode_focus_event(self.modes.contains(TermMode::FOCUS_IN_OUT), focused)
                    && let Some(pane) = self.active_pane_key()
                {
                    let _ = self.pump_commands.send(PumpMsg::Input {
                        pane,
                        bytes: seq.into_bytes(),
                    });
                }
            }
            _ => {}
        }
    }
}
