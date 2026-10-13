//! nora: a fast, efficient, maintainable Linux desktop terminal.
//!
//! CLI (N00): the binary identifies itself and its version.
//! Smoke (N01): `--smoke` runs one automated window smoke cycle and exits
//! with a status code reflecting success (never the launch path).
//! Launch (N05b/N08b2b-2): `nora -- COMMAND ARG...` (and bare `nora`) opens
//! the windowed session and exits with the command's exit status (1 for
//! signal death or a loop death without one); with no command the default
//! shell (`$SHELL`, else `/bin/sh`) is launched and a clean close exits 0.
//! With no display server available the launch falls back to the headless
//! session pump (CI-safe).

mod app;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use nora::session::pty::DEFAULT_CAP;
use nora::session::{LaunchSpec, PtyError, PtySession};

const NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "usage: nora [OPTIONS] [-- COMMAND ARG...]\n\n\
A fast, efficient, maintainable Linux desktop terminal.\n\
\n\
Options:\n\
  -h, --help                   Print this help message\n\
  -V, --version                Print nora's name and version\n\
      --smoke                  Run one automated window smoke cycle and exit\n\
      --working-directory PATH Start the command in PATH\n\
      --hold                  Keep the window open after the child\n\
                             exits (windowed; headless ignores it)\n\
      --                       Remainder is the command argv (no shell\n\
                               interpolation)\n\n\
With no command, the default shell ($SHELL, else /bin/sh) is launched.";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut smoke = false;
    let mut hold = false;
    let mut working_directory: Option<PathBuf> = None;
    let mut command: Vec<String> = Vec::new();
    let mut after_separator = false;

    // Left to right: flags are only recognized before `--`; anything after
    // `--` is command argv, never a flag.
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if after_separator {
            command.push(arg);
            continue;
        }
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("{NAME} {VERSION}");
                return ExitCode::SUCCESS;
            }
            "--smoke" => smoke = true,
            "--hold" => hold = true,
            "--working-directory" => {
                let Some(path) = iter.next() else {
                    eprintln!("{NAME} {VERSION}: --working-directory requires a path");
                    return ExitCode::from(2);
                };
                working_directory = Some(PathBuf::from(path));
            }
            "--" => after_separator = true,
            _ if arg.starts_with('-') => {
                eprintln!("{NAME} {VERSION}: unrecognized option: {arg}");
                eprint!("{USAGE}");
                return ExitCode::from(2);
            }
            _ => {
                eprintln!("{NAME} {VERSION}: unrecognized argument: {arg}");
                eprint!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    if after_separator && command.is_empty() {
        eprintln!("{NAME} {VERSION}: no command after --");
        return ExitCode::from(2);
    }

    if smoke {
        // N01 windowed smoke cycle; never the launch path.
        return app::run(true);
    }

    let explicit = !command.is_empty();
    let spec = if let Some((program, rest)) = command.split_first() {
        LaunchSpec {
            program: program.clone(),
            args: rest.to_vec(),
            cwd: working_directory,
            env: vec![],
        }
    } else {
        // No `-- COMMAND ...`: the default shell ($SHELL, else /bin/sh).
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        LaunchSpec {
            program: shell,
            args: vec![],
            cwd: working_directory,
            env: vec![],
        }
    };
    // Bare `nora` and `nora -- COMMAND ARG...` both open the windowed
    // session (N08b2b-2); `run_windowed` falls back to the headless
    // session pump when no display server is available (CI-safe).
    run_windowed(spec, explicit, hold)
}

/// Headless launch (N05b). 24×80 is the documented headless placeholder
/// until the UI cards.
fn run_session(spec: LaunchSpec, explicit: bool) -> ExitCode {
    let mut session = match PtySession::spawn(spec, 24, 80, DEFAULT_CAP) {
        Ok(session) => session,
        Err(err) => {
            let message = match &err {
                PtyError::BadCwd(path) => format!(
                    "working directory '{}' does not exist or is not a directory",
                    path.display()
                ),
                PtyError::Spawn(source) => format!("session could not be started: {source}"),
            };
            eprintln!("{NAME} {VERSION}: {message}");
            return ExitCode::from(1);
        }
    };
    let status = session.join();
    // Surface the last title set by the child, if any (N06b), on stderr
    // so it never mixes with terminal output.
    if let Some(title) = session.title() {
        eprintln!("title: {title}");
    }
    session.close();
    let code = if explicit {
        // Propagate the child's exit status; a signal death (or a loop
        // death without one) becomes 1 (02:52).
        status.map(|status| status.code().unwrap_or(1)).unwrap_or(1)
    } else {
        // Default shell: no status to propagate; 0 on clean close.
        0
    };
    ExitCode::from(code as u8)
}

/// Windowed interactive launch (N08b2b-2). The pump thread owns the
/// session; the event loop owns the window and the render state. A
/// headless environment (no display server) falls back to the N05b
/// headless session pump, so `nora -- CMD` stays CI-safe. Exits with
/// the child's exit status when explicit (1 on a loop death without
/// one), 0 on a clean default-shell close, or 1 on a startup failure.
fn run_windowed(spec: LaunchSpec, explicit: bool, hold: bool) -> ExitCode {
    let event_loop =
        match winit::event_loop::EventLoop::<nora::session::pump::AppEvent>::with_user_event()
            .build()
        {
            Ok(event_loop) => event_loop,
            Err(err) => {
                eprintln!("{NAME} {VERSION}: no display server ({err}); headless session");
                return run_session(spec, explicit);
            }
        };

    // N11a: the window exists before the session spawn, so the child
    // sees the window's *logical* grid from the first byte (the font
    // metrics are pure — no display needed — and the initial
    // `WindowEvent::Resized` asks the pump for the same grid: a no-op
    // engine resize with no damage, no pulse, no extra frame).
    //
    // Pinned winit 0.30.13 moved window creation to
    // `ActiveEventLoop::create_window` (handler context); N11a's spawn
    // ordering needs the window before `run_app`, so the deprecated
    // `EventLoop::create_window` wrapper is used here — it builds the
    // identical `platform_impl::Window`.
    #[allow(deprecated)]
    let window = std::sync::Arc::new(
        event_loop
            .create_window(
                winit::window::WindowAttributes::default()
                    .with_title("nora")
                    .with_inner_size(winit::dpi::LogicalSize::new(
                        app::INITIAL_SIZE.0,
                        app::INITIAL_SIZE.1,
                    )),
            )
            .expect("creating the nora window"),
    );
    let fonts = app::glyphs::Fonts::load();
    let (cols, rows) = app::window_grid(&window, &fonts);
    let (rows, cols) = if rows == 0 || cols == 0 {
        eprintln!("debug: degenerate window grid at spawn; falling back to 24x80");
        (24, 80)
    } else {
        (rows, cols)
    };
    let session = match PtySession::spawn(spec, rows as usize, cols as usize, DEFAULT_CAP) {
        Ok(session) => session,
        Err(err) => {
            let message = match &err {
                PtyError::BadCwd(path) => format!(
                    "working directory '{}' does not exist or is not a directory",
                    path.display()
                ),
                PtyError::Spawn(source) => format!("session could not be started: {source}"),
            };
            eprintln!("{NAME} {VERSION}: {message}");
            return ExitCode::from(1);
        }
    };

    let slot: std::sync::Arc<std::sync::Mutex<Option<nora::engine::Snapshot>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let (pump_commands, pump_commands_rx) =
        std::sync::mpsc::channel::<nora::session::pump::PumpMsg>();
    let proxy = event_loop.create_proxy();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // The pump thread gets its own Arcs; the window loop keeps these.
    let pump_slot = slot.clone();
    let pump_stop = stop.clone();
    let pump = std::thread::spawn(move || {
        nora::session::pump::pump_thread(session, pump_slot, proxy, pump_commands_rx, pump_stop)
    });
    let code = app::run_windowed(
        event_loop,
        window,
        slot,
        pump_commands,
        stop.clone(),
        explicit,
        hold,
    );
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = pump.join();
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_reports_name_and_version() {
        assert_eq!(format!("{NAME} {VERSION}"), format!("nora {VERSION}"));
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
