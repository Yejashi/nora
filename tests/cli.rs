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
