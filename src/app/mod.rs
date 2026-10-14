//! nora window/application state (N01).
//!
//! Creates a real window on the available native backend, binds a wgpu
//! surface to it and reports backend/device metadata. No terminal content is
//! rendered yet; the terminal arrives with the N03 engine adapter.

pub mod glyphs;
mod graphics;

use std::{
    sync::{
        Arc, Mutex,
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
    keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey},
    window::{Window, WindowAttributes, WindowId},
};

use self::graphics::{DrawCell, Graphics};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::TermMode;
use nora::clipboard::{ClipboardBackend, create_clipboard};
use nora::engine::{KittyKeyboardFlags, Snapshot};
use nora::input::focus::encode_focus_event;
use nora::input::mouse::{
    MouseEvent, MouseKind, MouseMode, button_number, encode_mouse_event, logical_to_cell,
    mouse_modifiers, physical_to_cell, should_report, track_button, wheel_directions,
};
use nora::input::{BindingTable, InputAction, encode, encode_kitty};
use nora::render::policy::{clamp_grid, snapshot_is_stale};
use nora::render::state::{Action, RenderEvent, RenderState, SurfaceStatus, on_event};
use nora::render::style::cell_style;
use nora::session::pump::{AppEvent, PumpMsg};

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
    eprintln!(
        "debug: config loaded: history={}, font.size={}",
        config.terminal.history, config.font.size
    );
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
        search: nora::search::SearchUi::new(),
        last_visible: Vec::new(),
        ime: nora::ime::ImeState::new(),
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
        if let Some(seq) = encode_mouse_event(mode, event, cols, rows) {
            let _ = self.pump_commands.send(PumpMsg::Input(seq.into_bytes()));
        }
    }

    /// Render the pending snapshot through one acquire → fold → draw →
    /// present pass, following the pure machine's [`Action`].
    fn attempt_render(&mut self, event_loop: &ActiveEventLoop) {
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
                let snap = self.slot.lock().unwrap().take();
                let Some(snap) = snap else {
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
                for row in 0..snap.rows {
                    for col in 0..snap.cols {
                        let cell = &snap.cells[row * snap.cols + col];
                        // Spacer cells (wide-char continuation) are not drawn.
                        if cell.width() == 0 {
                            continue;
                        }
                        // Spaces carry no content.
                        if cell.ch == ' ' {
                            continue;
                        }
                        let style = cell_style(cell.flags, cell.fg, cell.bg, &snap.colors);
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
        match event {
            AppEvent::DirtyPulse => {
                if debug_enabled() {
                    eprintln!("debug: pulse received");
                }
                let (state, action) =
                    on_event(self.state.clone(), RenderEvent::Dirty { non_noop: true });
                self.state = state;
                match action {
                    Action::Render => {
                        let window = self
                            .window
                            .as_ref()
                            .expect("the window exists before user events");
                        window.request_redraw(); // redraw SITE 1
                        self.attempt_render(event_loop);
                    }
                    Action::ExitFail => {
                        self.failed = true;
                        event_loop.exit();
                    }
                    Action::Skip => {
                        // Idle: zero frames until the next event.
                    }
                    _ => {}
                }
            }
            AppEvent::ChildExit(status) => {
                self.child_exit = Some(status);
                if debug_enabled() {
                    eprintln!("debug: child exit status {status:?}");
                }
                let (state, action) = on_event(self.state.clone(), RenderEvent::ChildExited);
                self.state = state;
                if action == Action::Close {
                    // N12b `--hold`: skip the exit; the window keeps its
                    // final frame (pulse-gated rendering — the pump has
                    // exited, so nothing re-renders). Unhold goes through
                    // the existing CloseRequested/Destroyed/Quit arms;
                    // the exit code is already captured in `self.child_exit`.
                    if self.hold {
                        if debug_enabled() {
                            eprintln!("debug: hold active; window stays open");
                        }
                    } else {
                        event_loop.exit();
                    }
                }
            }
            AppEvent::Modes(modes) => {
                // N10b: the engine's live `TermMode` changed; update the
                // mode source for `encode`. Mode changes do not dirty the
                // screen, so no `request_redraw`.
                self.modes = modes;
            }
            AppEvent::KittyModes(flags) => {
                // N21b: the engine's live kitty keyboard flags changed;
                // update the flag source for `encode_kitty`. Flag
                // changes do not dirty the screen, so no
                // `request_redraw`.
                self.kitty_flags = flags;
            }
            AppEvent::Copied(text) => {
                // N14b: the selected text was copied. N15: write it to the
                // native clipboard (arboard-backed); the debug line stays
                // for diagnostics.
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
                    Some(fonts) => window_grid(window, fonts),
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

                // (d) Ask the pump thread to resize the PTY session, only
                // when the clamped grid differs from the last one sent
                // (N11a: zero retention + change-only sends); the next
                // pulse carries the post-resize `Full` snapshot, which
                // the render path presents.
                let (rows, cols) = clamp_grid(rows, cols, self.last_grid.0, self.last_grid.1);
                if (rows, cols) != self.last_grid {
                    self.last_grid = (rows, cols);
                    let _ = self
                        .pump_commands
                        .send(PumpMsg::Resize(rows as usize, cols as usize));
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
                    Some(fonts) => window_grid(window, fonts),
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

                // (d) Send the clamped resize only on change (N11a); the
                // redraw arrives via SITE 1 (DirtyPulse) when the pump
                // takes the post-resize `Full` snapshot — no
                // `request_redraw` in this arm, keeping the pinned
                // exactly-three call sites intact.
                let (rows, cols) = clamp_grid(rows, cols, self.last_grid.0, self.last_grid.1);
                if (rows, cols) != self.last_grid {
                    self.last_grid = (rows, cols);
                    let _ = self
                        .pump_commands
                        .send(PumpMsg::Resize(rows as usize, cols as usize));
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
                // N15: native clipboard UI bindings. These are local UI
                // actions (not PTY escapes), so they are checked before
                // `encode` and return early. Keyed on the physical key +
                // modifier state (layout-independent), mirroring the N09
                // Ctrl+Q idiom; auto-repeat is ignored (one action per key
                // press).
                if self.mods.control_key() && self.mods.shift_key() && !event.repeat {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::KeyC) => {
                            // Ctrl+Shift+C: copy the selection; the pump
                            // emits `AppEvent::Copied`, which writes the
                            // native clipboard.
                            let _ = self.pump_commands.send(PumpMsg::Copy);
                            return;
                        }
                        PhysicalKey::Code(KeyCode::KeyV) => {
                            // Ctrl+Shift+V: paste the clipboard into the
                            // PTY (N17b). The pump validates the text
                            // (size cap, newline normalization, bracketed
                            // paste markers) and streams it to the PTY in
                            // order, bounded by the writer's app cap.
                            // `bracketed` reflects the engine's live
                            // `TermMode::BRACKETED_PASTE` (set by
                            // `CSI ? 2004 h`, cleared by `CSI ? 2004 l`).
                            let text = self.clipboard.as_ref().and_then(|cb| cb.get_text().ok());
                            match text {
                                Some(text) => {
                                    let bracketed = self.modes.contains(TermMode::BRACKETED_PASTE);
                                    let _ =
                                        self.pump_commands.send(PumpMsg::Paste { text, bracketed });
                                }
                                None => {
                                    if debug_enabled() {
                                        eprintln!("debug: paste: clipboard unavailable or empty");
                                    }
                                }
                            }
                            return;
                        }
                        _ => {}
                    }
                }
                // N19b: zoom controls. Local UI actions (not PTY escapes),
                // so they are checked before `encode` and return early,
                // mirroring the N15 idiom. Keyed on the physical key +
                // Ctrl (layout-independent), so the shortcuts work on any
                // layout.
                if self.mods.control_key() {
                    match event.physical_key {
                        // Ctrl+= / Ctrl++: zoom in one step, capped at
                        // 4.0x. The main-row `+` is the `Equal` key with
                        // Shift; the numpad `+` is `NumpadAdd` (winit
                        // 0.30.13 has no `Plus` code).
                        PhysicalKey::Code(KeyCode::Equal)
                        | PhysicalKey::Code(KeyCode::NumpadAdd) => {
                            self.zoom = nora::config::zoom_in(self.zoom);
                            eprintln!("zoom: {:.1}x", self.zoom);
                            return;
                        }
                        PhysicalKey::Code(KeyCode::Minus) => {
                            // Ctrl+-: zoom out one step, floored at 0.5x.
                            self.zoom = nora::config::zoom_out(self.zoom);
                            eprintln!("zoom: {:.1}x", self.zoom);
                            return;
                        }
                        PhysicalKey::Code(KeyCode::Digit0) => {
                            // Ctrl+0: reset zoom to 100%.
                            self.zoom = nora::config::zoom_reset();
                            eprintln!("zoom: reset to {:.1}x", self.zoom);
                            return;
                        }
                        _ => {}
                    }
                }
                // N20b: search bar. Local UI actions (not PTY escapes),
                // checked before `encode` and returning early, mirroring
                // the N15/N19b idiom. While the bar is visible the
                // logical key drives the query (typed text is
                // layout-dependent); the toggle is keyed on the
                // physical `F` + Ctrl (layout-independent).
                if self.search.visible {
                    match &event.logical_key {
                        Key::Character(text) => {
                            self.search.input(text, &self.last_visible);
                            eprintln!(
                                "search: {} match(es) for '{}'",
                                self.search.state.total, self.search.state.query
                            );
                            return;
                        }
                        Key::Named(NamedKey::Enter) => {
                            // Enter = next match, Shift+Enter = previous
                            // (both wrap); the highlight span is
                            // recomputed on the next presented frame.
                            if self.mods.shift_key() {
                                self.search.prev();
                            } else {
                                self.search.next();
                            }
                            if let Some(m) = self.search.current_match() {
                                eprintln!("search highlight: row={}, col={}", m.row, m.col);
                            }
                            return;
                        }
                        Key::Named(NamedKey::Backspace) => {
                            self.search.backspace(&self.last_visible);
                            eprintln!(
                                "search: {} match(es) for '{}'",
                                self.search.state.total, self.search.state.query
                            );
                            return;
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.search.close();
                            eprintln!("search: hidden");
                            return;
                        }
                        _ => {}
                    }
                }
                if self.mods.control_key()
                    && !event.repeat
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyF)
                {
                    self.search.toggle();
                    eprintln!(
                        "search: {}",
                        if self.search.visible {
                            "shown"
                        } else {
                            "hidden"
                        }
                    );
                    return;
                }
                // N21b: kitty keyboard protocol — when the application
                // has negotiated the protocol (the engine's live flags
                // are non-NONE), the key is encoded in the kitty byte
                // format and sent to the PTY; the legacy `encode` is
                // the fallback when the protocol is disabled. The
                // window's initial `NONE` is corrected by the first
                // pump tick's `AppEvent::KittyModes`.
                if let Some(bytes) = encode_kitty(
                    &event.logical_key,
                    &event.physical_key,
                    &self.mods,
                    event.repeat,
                    self.kitty_flags,
                ) {
                    let _ = self.pump_commands.send(PumpMsg::Input(bytes));
                    return;
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
                            let _ = self.pump_commands.send(PumpMsg::Input(bytes));
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
                                window,
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
                if lines != 0 {
                    let _ = self
                        .pump_commands
                        .send(PumpMsg::Scroll(Scroll::Delta(lines)));
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
                    && let Some((end_row, end_col)) = self.grid_cell_at(window, x, y)
                {
                    let _ = self
                        .pump_commands
                        .send(PumpMsg::Select(start_row, start_col, end_row, end_col));
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
                {
                    self.send_mouse_event(
                        &MouseEvent {
                            kind: MouseKind::Motion,
                            button: self.pressed_buttons.first().copied(),
                            col,
                            row,
                            modifiers: mouse_modifiers(&self.mods),
                        },
                        window,
                    );
                }
            }
            WindowEvent::MouseInput { button, state, .. } => {
                // N14b: primary-button drag selects. `MouseInput` carries
                // no position, so use the last tracked cursor.
                if button == MouseButton::Left {
                    let (x, y) = self.cursor;
                    match state {
                        ElementState::Pressed => {
                            if let Some(cell) = self.grid_cell_at(window, x, y) {
                                self.selection = Some(cell);
                                let _ = self
                                    .pump_commands
                                    .send(PumpMsg::Select(cell.0, cell.1, cell.0, cell.1));
                            }
                        }
                        ElementState::Released => {
                            // Finalize: the engine keeps the last
                            // selection; drop the drag state.
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
                            window,
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
                    eprintln!("IME: composition started");
                }
                winit::event::Ime::Preedit(text, _cursor_pos) => {
                    self.ime.update_preedit(&text);
                    eprintln!("IME: preedit = {:?}", text);
                }
                winit::event::Ime::Commit(_text) => {
                    // The state machine is the single source of truth for
                    // what commits; `commit()` returns the tracked preedit
                    // exactly once and deactivates.
                    if let Some(committed) = self.ime.commit() {
                        eprintln!("IME: commit = {:?}", committed);
                        let _ = self
                            .pump_commands
                            .send(PumpMsg::Input(committed.into_bytes()));
                    }
                }
                winit::event::Ime::Disabled => {
                    self.ime.cancel();
                    eprintln!("IME: composition cancelled");
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
                {
                    let _ = self.pump_commands.send(PumpMsg::Input(seq.into_bytes()));
                }
            }
            _ => {}
        }
    }
}
