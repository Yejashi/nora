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
    event::{ElementState, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::ModifiersState,
    window::{Window, WindowAttributes, WindowId},
};

use self::graphics::{DrawCell, Graphics};
use alacritty_terminal::term::TermMode;
use nora::engine::Snapshot;
use nora::input::{BindingTable, InputAction, encode};
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
                if let Err(message) = graphics.draw_frame(&mut frame, &mut fonts, &[]) {
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
}

/// Run the windowed terminal (N08b2b-2). `event_loop` carries
/// [`AppEvent`]s from the pump thread; `slot` carries the latest snapshot;
/// `pump_commands` carries window→pump messages; `stop` signals the pump
/// thread when the window closes. `hold` (`--hold`, N12b) defers the
/// `ChildExit` close: the window keeps its final frame until unhold.
pub fn run_windowed(
    event_loop: EventLoop<AppEvent>,
    window: Arc<Window>,
    slot: Arc<Mutex<Option<Snapshot>>>,
    pump_commands: mpsc::Sender<PumpMsg>,
    stop: Arc<AtomicBool>,
    explicit: bool,
    hold: bool,
) -> std::process::ExitCode {
    let mut app = WinApp {
        slot,
        pump_commands,
        stop,
        state: RenderState::default(),
        mods: ModifiersState::default(),
        modes: TermMode::SHOW_CURSOR,
        fonts: None,
        graphics: None,
        window: Some(window),
        failed: false,
        frames: 0,
        child_exit: None,
        hold,
        last_grid: (0, 0),
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
                let line1 = first_non_space_line(&snap, 40);
                if let Err(message) = graphics.draw_frame(&mut frame, fonts, &draw_cells) {
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

        // N09b: no IME in v0.1; `Ime` events are ignored in `window_event`.
        window.set_ime_allowed(false);

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
            WindowEvent::Ime(_) => {}     // N09b: no IME in v0.1.
            WindowEvent::Focused(_) => {} // N09b: synthetic filtering covers duplicates.
            _ => {}
        }
    }
}
