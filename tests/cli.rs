//! N05b CLI regression suite for `nora -- COMMAND ARG...`.
//!
//! Every process run is deadline-bounded at 30 s: we poll `try_wait` on a
//! 10 ms tick and kill the process on overrun, then drain the captured
//! stdout/stderr once the process has exited (small outputs only, so the
//! pipe buffers cannot fill).

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Hard deadline for each CLI process run; the process is killed on overrun.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);
/// One poll tick while waiting for a CLI process to exit.
const TICK: Duration = Duration::from_millis(10);

/// The nora bin path, auto-discovered by cargo for integration tests.
fn nora_bin() -> String {
    std::env::var("CARGO_BIN_EXE_nora")
        .expect("CARGO_BIN_EXE_nora must be set by cargo for integration tests")
}

/// The helper bin path, auto-discovered by cargo for integration tests.
fn helper_bin() -> String {
    std::env::var("CARGO_BIN_EXE_nora_child")
        .expect("CARGO_BIN_EXE_nora_child must be set by cargo for integration tests")
}

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run a process to completion under the 30 s deadline, then drain its
/// captured output. Panics (after killing the process) on overrun.
fn run_bounded(cmd: &mut Command) -> Output {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI test process must spawn");
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(handle) = child.stdout.as_mut() {
                    handle
                        .read_to_string(&mut stdout)
                        .expect("draining stdout after exit");
                }
                if let Some(handle) = child.stderr.as_mut() {
                    handle
                        .read_to_string(&mut stderr)
                        .expect("draining stderr after exit");
                }
                return Output {
                    code: status.code(),
                    stdout,
                    stderr,
                };
            }
            Ok(None) => {
                if start.elapsed() >= RUN_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("CLI process did not exit within {RUN_TIMEOUT:?}");
                }
                thread::sleep(TICK);
            }
            Err(err) => panic!("failed to wait on CLI process: {err}"),
        }
    }
}

#[test]
fn explicit_command_exit_status_propagates() {
    let helper = helper_bin();
    let mut cmd = Command::new(nora_bin());
    cmd.args(["--", helper.as_str(), "status", "7"]);
    let out = run_bounded(&mut cmd);
    assert_eq!(
        out.code,
        Some(7),
        "nora must propagate the child's exit status; stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );
}

#[test]
fn hold_headless_exit_status_propagates() {
    let helper = helper_bin();
    let mut cmd = Command::new(nora_bin());
    cmd.args(["--hold", "--", helper.as_str(), "status", "7"])
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY");
    let out = run_bounded(&mut cmd);
    assert_eq!(
        out.code,
        Some(7),
        "headless --hold is ignored; status must still propagate; stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );
}

#[test]
fn no_command_after_dash_dash_is_an_error() {
    let mut cmd = Command::new(nora_bin());
    cmd.arg("--");
    let out = run_bounded(&mut cmd);
    assert_eq!(
        out.code,
        Some(2),
        "`nora --` with no command must fail as a usage error; stderr={:?}",
        out.stderr
    );
    assert!(
        !out.stderr.trim().is_empty(),
        "stderr must explain the error"
    );
}

#[test]
fn unknown_flag_is_an_error() {
    let mut cmd = Command::new(nora_bin());
    cmd.arg("--bogus");
    let out = run_bounded(&mut cmd);
    assert_eq!(
        out.code,
        Some(2),
        "an unknown flag must fail as a usage error; stderr={:?}",
        out.stderr
    );
    assert!(
        !out.stderr.trim().is_empty(),
        "stderr must explain the error"
    );
}

#[test]
fn help_and_version_exit_zero() {
    let mut cmd = Command::new(nora_bin());
    cmd.arg("--help");
    let out = run_bounded(&mut cmd);
    assert_eq!(
        out.code,
        Some(0),
        "--help must exit 0; stderr={:?}",
        out.stderr
    );
    assert!(!out.stdout.trim().is_empty(), "--help must print usage");
    // The option table keeps its indentation and alignment.
    assert!(
        out.stdout
            .contains("\n  -h, --help                   Print this help message\n"),
        "--help options are indented: {:?}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("\n      --hold                   Keep the window open"),
        "--hold is aligned with the other options: {:?}",
        out.stdout
    );

    let mut cmd = Command::new(nora_bin());
    cmd.arg("--version");
    let out = run_bounded(&mut cmd);
    assert_eq!(
        out.code,
        Some(0),
        "--version must exit 0; stderr={:?}",
        out.stderr
    );
    assert!(
        !out.stdout.trim().is_empty(),
        "--version must print the version"
    );
}

/// Write a config file named `name` under a unique temp dir and return
/// its path.
fn write_config(name: &str, contents: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nora-config-test-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock must not precede the epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("unique temp dir must be creatable");
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("config file must be writable");
    path
}

/// Deterministic config discovery: with no `HOME` and no
/// `XDG_CONFIG_HOME`, `load_config(None)` has no XDG candidates and falls
/// back to the built-in defaults.
fn no_config_env(cmd: &mut Command) -> &mut Command {
    cmd.env_remove("HOME").env_remove("XDG_CONFIG_HOME")
}

#[test]
fn working_directory_flag() {
    let dir = std::env::temp_dir().join(format!(
        "nora-cli-test-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock must not precede the epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("unique temp dir must be creatable");

    let helper = helper_bin();
    let mut cmd = Command::new(nora_bin());
    cmd.arg("--working-directory")
        .arg(&dir)
        .arg("--")
        .arg(&helper)
        .arg("pwd");
    let out = run_bounded(&mut cmd);
    assert_eq!(
        out.code,
        Some(0),
        "nora must run the child in the requested cwd; stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );

    let mut cmd = Command::new(nora_bin());
    cmd.arg("--working-directory")
        .arg("/nonexistent-nora-cwd")
        .arg("--")
        .arg(&helper)
        .arg("pwd");
    let out = run_bounded(&mut cmd);
    assert_eq!(
        out.code,
        Some(1),
        "a bad cwd must exit 1 (PtyError::BadCwd); stderr={:?}",
        out.stderr
    );
    assert!(
        !out.stderr.trim().is_empty(),
        "stderr must report the bad cwd"
    );
}

// --- N18b: CLI config integration ---------------------------------------

/// `nora --check-config --config <valid>` exits 0 and reports the path.
#[test]
fn check_config_valid() {
    let path = write_config("valid.toml", "[terminal]\nhistory = 2000\n");
    let mut cmd = Command::new(nora_bin());
    no_config_env(&mut cmd)
        .arg("--check-config")
        .arg("--config")
        .arg(&path);
    let out = run_bounded(&mut cmd);
    assert_eq!(
        out.code,
        Some(0),
        "--check-config with a valid config must exit 0; stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("config OK"),
        "stdout must report the config as OK; stdout={:?}",
        out.stdout
    );
}

/// `nora --check-config --config <invalid>` exits 1 and reports the
/// validation error(s).
#[test]
fn check_config_invalid() {
    let path = write_config("invalid.toml", "[terminal]\nhistory = 999999999\n");
    let mut cmd = Command::new(nora_bin());
    no_config_env(&mut cmd)
        .arg("--check-config")
        .arg("--config")
        .arg(&path);
    let out = run_bounded(&mut cmd);
    assert_eq!(
        out.code,
        Some(1),
        "--check-config with an invalid config must exit 1; stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("terminal.history"),
        "stderr must name the failing setting; stderr={:?}",
        out.stderr
    );
}

/// `nora --check-config` with no discoverable config file exits 0 on the
/// built-in defaults (no display or GPU needed).
#[test]
fn check_config_defaults() {
    let mut cmd = Command::new(nora_bin());
    no_config_env(&mut cmd).arg("--check-config");
    let out = run_bounded(&mut cmd);
    assert_eq!(
        out.code,
        Some(0),
        "--check-config with no config file must exit 0 on the built-in defaults; stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("config OK: defaults"),
        "stdout must report that the built-in defaults were used; stdout={:?}",
        out.stdout
    );
}

/// `nora --config <valid> -- COMMAND` loads the config and still runs the
/// session headless, propagating the child's exit status.
#[test]
fn config_flag() {
    let path = write_config("valid.toml", "[theme]\nbackground = \"#101010\"\n");
    let helper = helper_bin();
    let mut cmd = Command::new(nora_bin());
    cmd.arg("--config")
        .arg(&path)
        .arg("--")
        .arg(&helper)
        .arg("status")
        .arg("7")
        // The config-loaded line is diagnostic output, shown only with
        // NORA_DEBUG so it never clutters the user's terminal.
        .env("NORA_DEBUG", "1")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY");
    let out = run_bounded(&mut cmd);
    assert_eq!(
        out.code,
        Some(7),
        "the child's exit status must still propagate with --config; stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("debug: config loaded"),
        "the loaded config must reach the session; stderr={:?}",
        out.stderr
    );
}

/// `nora --reload --config <valid> -- COMMAND` re-reads the config before
/// launch (reporting the new epoch) and still runs the session headless,
/// propagating the child's exit status.
#[test]
fn reload_flag() {
    let path = write_config("valid.toml", "[terminal]\nhistory = 2000\n");
    let helper = helper_bin();
    let mut cmd = Command::new(nora_bin());
    cmd.arg("--reload")
        .arg("--config")
        .arg(&path)
        .arg("--")
        .arg(&helper)
        .arg("status")
        .arg("7")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY");
    let out = run_bounded(&mut cmd);
    assert_eq!(
        out.code,
        Some(7),
        "the child's exit status must still propagate with --reload; stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("config reloaded (epoch 1)"),
        "--reload must report the reloaded epoch; stderr={:?}",
        out.stderr
    );
}
