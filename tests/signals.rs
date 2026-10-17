//! Termination signals reach the callback instead of killing the process
//! (its own test binary, so the handler does not affect other tests).
#![cfg(unix)]

use std::sync::mpsc;
use std::time::Duration;

#[test]
fn sighup_runs_the_callback() {
    let (tx, rx) = mpsc::channel();
    nora::signals::on_termination(move |signal| {
        let _ = tx.send(signal);
    })
    .expect("handler installs");
    // SAFETY: raising a signal whose handler was just installed.
    unsafe { libc::raise(libc::SIGHUP) };
    let got = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("callback ran");
    assert_eq!(got, libc::SIGHUP);
}

/// The child side of the tests below: runs only when the parent test
/// re-executes this binary with `NORA_SIGNAL_CHILD` set.
#[test]
fn signal_child() {
    let Ok(mode) = std::env::var("NORA_SIGNAL_CHILD") else {
        return;
    };
    let (tx, rx) = mpsc::channel();
    nora::signals::on_termination(move |signal| {
        let _ = tx.send(signal);
    })
    .expect("handler installs");
    // SAFETY: raising signals whose handler was just installed.
    unsafe {
        libc::raise(libc::SIGTERM);
        if mode == "burst" {
            libc::raise(libc::SIGHUP);
        }
    }
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(libc::SIGTERM));
    if mode == "late" {
        std::thread::sleep(nora::signals::SIGNAL_GRACE + Duration::from_millis(200));
        // SAFETY: as above.
        unsafe { libc::raise(libc::SIGHUP) };
    }
    std::thread::sleep(Duration::from_millis(500));
    assert!(rx.try_recv().is_err(), "the callback runs once");
}

/// Run [`signal_child`] in a child process in `mode`; its exit code.
fn run_child(mode: &str) -> Option<i32> {
    let exe = std::env::current_exe().expect("test binary");
    let status = std::process::Command::new(exe)
        .args(["--exact", "signal_child", "--test-threads=1", "--quiet"])
        .env("NORA_SIGNAL_CHILD", mode)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("child runs");
    status.code()
}

/// SIGTERM and SIGHUP sent back to back (a logout) are one request: the
/// orderly callback runs and the process is not force-exited.
#[test]
fn signals_sent_together_are_one_request() {
    assert_eq!(run_child("burst"), Some(0));
}

/// A second signal after the grace period still forces the exit, in case
/// the orderly path hangs.
#[test]
fn a_later_second_signal_forces_the_exit() {
    assert_eq!(run_child("late"), Some(128 + libc::SIGHUP));
}
