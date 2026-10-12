//! nora: a fast, efficient, maintainable Linux desktop terminal.
//!
//! CLI (N00): the binary identifies itself and its version.
//! Smoke (N01): `--smoke` runs one automated window smoke cycle and exits
//! with a status code reflecting success (never the launch path).
//! Launch (N05b): `nora -- COMMAND ARG...` runs the command headless on a
//! PTY session and exits with the command's exit status (1 for signal
//! death or a loop death without one); with no command the default shell
//! (`$SHELL`, else `/bin/sh`) is launched and a clean close exits 0.

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
      --                       Remainder is the command argv (no shell\n\
                               interpolation)\n\n\
With no command, the default shell ($SHELL, else /bin/sh) is launched.";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut smoke = false;
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
    run_session(spec, explicit)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_reports_name_and_version() {
        assert_eq!(format!("{NAME} {VERSION}"), format!("nora {VERSION}"));
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
