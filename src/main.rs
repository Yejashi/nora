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
//! Config (N18b): `--config PATH` selects an explicit config file and
//! `--check-config` validates the loaded config and exits (no display or
//! GPU needed). Precedence: CLI flags > config file > built-in defaults.
//! Reload (N19a): `--reload` re-reads the config from disk, classifies the
//! changes and swaps in the new state transactionally; a failed reload keeps
//! the last valid config.

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
      --config PATH            Use an explicit config file\n\
      --check-config           Validate the config and exit (no display\n\
                               or GPU needed)\n\
      --reload                 Re-read the config from disk before launch;\n\
                               a failed reload keeps the last valid config\n\
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
    let mut config: Option<String> = None;
    let mut check_config = false;
    let mut reload = false;
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
            "--config" => {
                let Some(path) = iter.next() else {
                    eprintln!("{NAME} {VERSION}: --config requires a path");
                    return ExitCode::from(2);
                };
                config = Some(path);
            }
            "--check-config" => check_config = true,
            "--reload" => reload = true,
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

    // N18b: the config is loaded before the app starts, so it is available
    // for the smoke, windowed and headless paths alike.
    let loaded_config = match nora::config::load_config(config.as_deref()) {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("{NAME} {VERSION}: {err}");
            return ExitCode::from(1);
        }
    };

    // `--check-config` is headless: validate and exit, no display or GPU.
    if check_config {
        let errors = loaded_config.config.validate();
        if errors.is_empty() {
            println!(
                "config OK: {}",
                loaded_config
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "defaults".to_string())
            );
            return ExitCode::SUCCESS;
        }
        for error in &errors {
            eprintln!("{error}");
        }
        return ExitCode::FAILURE;
    }

    // N19a: an explicit `--reload` re-reads the config from disk and swaps in
    // the new state transactionally. A failed reload (I/O, parse or
    // validation) keeps the last valid config. Headless-safe: this runs
    // before any display/GPU work, so it is exercised in CI too.
    let config_state = nora::config::ConfigState::new(loaded_config);
    let config_state = if reload {
        match config_state.reload() {
            Ok(new_state) => {
                let changes =
                    nora::config::classify_change(&config_state.current, &new_state.current);
                for (field, change_type) in &changes {
                    eprintln!("config reload: {field} = {change_type:?}");
                    // N19b: staged font/geometry changes apply on the next
                    // frame (font re-initialization is a later slice); log
                    // them as they are staged.
                    if *change_type == nora::config::ConfigChange::Staged {
                        eprintln!("applying staged font change: {field}");
                    }
                }
                eprintln!("config reloaded (epoch {})", new_state.epoch);
                new_state
            }
            Err(err) => {
                eprintln!("config reload failed: {err}");
                eprintln!("keeping last valid config");
                config_state
            }
        }
    } else {
        config_state
    };

    if smoke {
        // N01 windowed smoke cycle; never the launch path.
        return app::run(true);
    }

    let explicit = !command.is_empty();
    // N29: advertise only validated capabilities. Set `TERM` to the
    // dedicated `nora` terminfo entry when it is resolvable, else fall
    // back to the ubiquitous `xterm-256color` (the `nora` entry is a
    // strict subset of xterm-256color, so the fallback is always safe).
    // `COLORTERM=truecolor` signals validated 24-bit color support.
    let term = resolve_term();
    let child_env = vec![
        ("TERM".to_string(), term),
        ("COLORTERM".to_string(), "truecolor".to_string()),
    ];
    let spec = if let Some((program, rest)) = command.split_first() {
        LaunchSpec {
            program: program.clone(),
            args: rest.to_vec(),
            cwd: working_directory,
            env: child_env,
        }
    } else {
        // No `-- COMMAND ...`: the default shell ($SHELL, else /bin/sh).
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        LaunchSpec {
            program: shell,
            args: vec![],
            cwd: working_directory,
            env: child_env,
        }
    };
    // Bare `nora` and `nora -- COMMAND ARG...` both open the windowed
    // session (N08b2b-2); `run_windowed` falls back to the headless
    // session pump when no display server is available (CI-safe).
    // N18b precedence: the CLI `--working-directory` (already in
    // `spec.cwd`) overrides any config value; the config overrides the
    // built-in defaults.
    run_windowed(spec, explicit, hold, &config_state.current)
}

/// Headless wait granularity: how often parser replies are forwarded
/// while the child runs.
const HEADLESS_TICK: std::time::Duration = std::time::Duration::from_millis(20);

/// Headless launch (N05b). 24×80 is the documented headless placeholder
/// until the UI cards. `config` (N18b) is the loaded configuration; the
/// values are accepted and logged now, and consumed where the headless
/// path supports them.
fn run_session(spec: LaunchSpec, explicit: bool, config: &nora::config::NoraConfig) -> ExitCode {
    if std::env::var_os("NORA_DEBUG").is_some() {
        eprintln!(
            "debug: config loaded: history={}, font.size={}",
            config.terminal.history, config.font.size
        );
    }
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
    // Wait for the child for as long as it runs (no drain timeout here:
    // that bound is for an explicit close), forwarding parser replies to
    // it so a child that queries the terminal (DA, DSR, size) never hangs.
    while !session.wait_drain(HEADLESS_TICK) {
        if session.flush(usize::MAX).is_err() {
            break;
        }
    }
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
/// `config` (N18b) is the loaded configuration, passed to the windowed
/// app and the headless fallback alike.
fn run_windowed(
    spec: LaunchSpec,
    explicit: bool,
    hold: bool,
    config: &nora::config::NoraConfig,
) -> ExitCode {
    let event_loop =
        match winit::event_loop::EventLoop::<nora::session::pump::AppEvent>::with_user_event()
            .build()
        {
            Ok(event_loop) => event_loop,
            Err(err) => {
                eprintln!("{NAME} {VERSION}: no display server ({err}); headless session");
                return run_session(spec, explicit, config);
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
    // Loaded once here (the first PTY needs the grid) and handed to the
    // window loop.
    let mut fonts = app::glyphs::Fonts::load(&config.font);
    fonts.measure(window.scale_factor(), nora::config::ZOOM_DEFAULT);
    let (cols, rows) = app::window_grid(&window, &fonts);
    let (rows, cols) = if rows == 0 || cols == 0 {
        eprintln!("debug: degenerate window grid at spawn; falling back to 24x80");
        (24, 80)
    } else {
        (rows, cols)
    };
    // N23b: the pump spawns new-tab sessions from the same launch spec.
    let spec_for_pump = spec.clone();
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
    // N24b: the pump owns one session per pane; the window's first pane is
    // tab 0's first pane, matching the workspace's first `add_tab()` and
    // the layout's first `Layout::new()` pane.
    let first_pane = nora::session::pump::PaneKey::first();
    let pump = std::thread::spawn(move || {
        nora::session::pump::pump_thread(
            first_pane,
            session,
            pump_slot,
            spec_for_pump,
            proxy,
            pump_commands_rx,
            pump_stop,
        )
    });
    let code = app::run_windowed(
        event_loop,
        window,
        slot,
        pump_commands,
        stop.clone(),
        explicit,
        hold,
        config,
        fonts,
    );
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = pump.join();
    code
}

/// N29: resolve the `TERM` value to advertise to children. Returns
/// `"nora"` when the dedicated `nora` terminfo entry is resolvable in one
/// of the standard search paths, else falls back to `"xterm-256color"`.
/// The `nora` entry is a strict subset of xterm-256color, so the fallback
/// is always safe and no child ever sees an unknown or missing `TERM`.
fn resolve_term() -> String {
    // Search order: `$TERMINFO/n/nora`, `~/.terminfo/n/nora`,
    // `/etc/terminfo/n/nora`, `/usr/local/share/terminfo/n/nora`,
    // `/usr/share/terminfo/n/nora`.
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(terminfo) = env::var_os("TERMINFO") {
        candidates.push(std::path::Path::new(&terminfo).join("n").join("nora"));
    }
    if let Some(home) = env::var_os("HOME") {
        candidates.push(
            std::path::Path::new(&home)
                .join(".terminfo")
                .join("n")
                .join("nora"),
        );
    }
    candidates.push(std::path::PathBuf::from("/etc/terminfo/n/nora"));
    candidates.push(std::path::PathBuf::from("/usr/local/share/terminfo/n/nora"));
    candidates.push(std::path::PathBuf::from("/usr/share/terminfo/n/nora"));

    if candidates.iter().any(|path| path.is_file()) {
        "nora".to_string()
    } else {
        "xterm-256color".to_string()
    }
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
