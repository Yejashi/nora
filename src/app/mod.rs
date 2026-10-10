//! nora window/application state (N01).
//!
//! Creates a real window on the available native backend, binds a wgpu
//! surface to it and reports backend/device metadata. No terminal content is
//! rendered yet; the terminal arrives with the N03 engine adapter.

mod graphics;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowAttributes, WindowId},
};

use self::graphics::Graphics;

/// Fixed smoke-test resize target, in logical pixels.
const SMOKE_RESIZE: (f64, f64) = (640.0, 480.0);
/// Settle time after resize+minimize before the smoke run closes the window.
const SMOKE_SETTLE: Duration = Duration::from_millis(250);
/// Watchdog: smoke fails if no progress is made for this long.
const SMOKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Window size requested at creation, in logical pixels.
const INITIAL_SIZE: (f64, f64) = (800.0, 600.0);

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

        let mut graphics = match pollster::block_on(Graphics::new(event_loop, &window)) {
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
