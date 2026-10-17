//! Termination signals (SIGTERM, SIGHUP, SIGINT) become an orderly
//! shutdown: the window closes as if asked to, so every session is closed
//! and the suggestion history is saved. A second signal more than a
//! second later exits at once (signals sent together count as one), in
//! case the orderly path hangs.
//!
//! The handler only writes one byte to a pipe (async-signal-safe); a
//! thread reads it and runs the callback. Children are unaffected: `exec`
//! resets caught signals to their default action, and the PTY spawn also
//! resets them explicitly.
//!
//! This is the crate's `unsafe` boundary for signals: `pipe`, `sigaction`
//! and the handler's `write`, each with its safety argument below.

/// How long after the first signal further signals count as part of the
/// same request rather than a second one.
pub const SIGNAL_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

/// Run `callback` with the signal number on the first SIGTERM, SIGHUP or
/// SIGINT; exit with `128 + signal` on a later one that arrives more than
/// [`SIGNAL_GRACE`] after the callback returned. Call once.
#[cfg(unix)]
pub fn on_termination(callback: impl FnOnce(i32) + Send + 'static) -> std::io::Result<()> {
    use std::io::Read as _;
    use std::os::fd::FromRawFd as _;
    use std::sync::atomic::Ordering;

    let mut fds = [-1; 2];
    // SAFETY: `fds` is a valid array of two `c_int`s for `pipe` to fill.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    for fd in fds {
        // SAFETY: `fd` was just returned by `pipe` and is open.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // SAFETY: the read end is open and owned by nothing else; the `File`
    // takes ownership and closes it.
    let mut reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let close_writer = || {
        // SAFETY: the write end is open and owned by this function only
        // (the handler no longer sees it once `PIPE_WRITE` is reset);
        // closing it gives the reader thread EOF, so it ends.
        unsafe { libc::close(fds[1]) };
    };

    // The reader thread exists before any handler is installed, so a
    // caught signal always has a reader acting on it.
    let spawned = std::thread::Builder::new()
        .name("nora-signals".to_string())
        .spawn(move || {
            let mut byte = [0u8; 1];
            if reader.read_exact(&mut byte).is_err() {
                return;
            }
            callback(i32::from(byte[0]));
            // Signals that arrive together are one request: a logout sends
            // SIGTERM and SIGHUP back to back. Only one that arrives after
            // the grace period forces the exit.
            let grace = std::time::Instant::now() + SIGNAL_GRACE;
            while reader.read_exact(&mut byte).is_ok() {
                if std::time::Instant::now() >= grace {
                    std::process::exit(128 + i32::from(byte[0]));
                }
            }
        });
    if let Err(error) = spawned {
        close_writer();
        return Err(error);
    }
    imp::PIPE_WRITE.store(fds[1], Ordering::Release);

    let signals = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];
    // SAFETY: an all-zero `sigaction` is a valid value (SIG_DFL, empty
    // mask); a slot is only read back after `sigaction` filled it.
    let mut previous: [libc::sigaction; 3] = unsafe { std::mem::zeroed() };
    for (index, signal) in signals.into_iter().enumerate() {
        // SAFETY: `action` is zero-initialized (a valid `sigaction` with an
        // empty mask), its handler is an `extern "C" fn(c_int)` that only
        // makes async-signal-safe calls, and `previous[index]` is a valid
        // place for the old action.
        let installed = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = imp::on_signal as extern "C" fn(libc::c_int) as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, &mut previous[index])
        };
        if installed != 0 {
            let error = std::io::Error::last_os_error();
            // Put back the actions already replaced: a caught signal with
            // no pipe behind it would otherwise be ignored.
            for (signal, old) in signals.into_iter().zip(&previous).take(index) {
                // SAFETY: `old` is the action `sigaction` reported for this
                // signal above, a valid `sigaction`.
                unsafe { libc::sigaction(signal, old, std::ptr::null_mut()) };
            }
            imp::PIPE_WRITE.store(-1, Ordering::Release);
            close_writer();
            return Err(error);
        }
    }
    Ok(())
}

/// Not supported off Unix yet: the process ends with the platform default.
#[cfg(not(unix))]
pub fn on_termination(_callback: impl FnOnce(i32) + Send + 'static) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
mod imp {
    use std::sync::atomic::{AtomicI32, Ordering};

    /// The pipe's write end, for the handler.
    pub(super) static PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

    pub(super) extern "C" fn on_signal(signal: libc::c_int) {
        let fd = PIPE_WRITE.load(Ordering::Acquire);
        if fd < 0 {
            return;
        }
        let byte = signal as u8;
        let errno = errno_location();
        // SAFETY: `errno_location` points at this thread's errno (or is
        // null), which a handler must leave unchanged; `write` is
        // async-signal-safe and `byte` outlives the call.
        unsafe {
            let saved = errno.as_ref().copied();
            libc::write(fd, (&byte as *const u8).cast(), 1);
            if let Some(saved) = saved {
                *errno = saved;
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn errno_location() -> *mut libc::c_int {
        // SAFETY: always returns this thread's errno.
        unsafe { libc::__errno_location() }
    }

    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    fn errno_location() -> *mut libc::c_int {
        // SAFETY: always returns this thread's errno.
        unsafe { libc::__error() }
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    )))]
    fn errno_location() -> *mut libc::c_int {
        std::ptr::null_mut()
    }
}
