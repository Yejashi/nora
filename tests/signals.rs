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
