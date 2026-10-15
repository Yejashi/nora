//! Tagged-I/O child helper for the PTY session tests (N05).
//!
//! Behavior (every line flushed as it is printed):
//! - no args: print `READY`, then echo each stdin line as `E:<line>`;
//! - `argv a1 a2 ...`: print `A:<i> <arg>` for i = 1.., exit 0;
//! - `pwd`: print `PWD:<absolute cwd>`, exit 0;
//! - `status <n>`: print `READY`, exit `<n>` (missing `<n>` → 0);
//! - `stop <n>`: read stdin until >= `n` bytes received and a newline has
//!   been seen, print `STOPPED`, then read until EOF, exit 0;
//! - `ws <rows> <cols>`: print `WS:<rows>;<cols>` from the actual terminal
//!   winsize on stdin (arguments nominal, ignored), exit 0;
//! - `dying <q-hex>`: print `READY`, wait 100 ms, write the hex-decoded
//!   query bytes to stdout, exit 0;
//! - `descendant <hold_ms>`: print `READY`, spawn `/bin/sleep <hold_ms
//!   as seconds>` with inherited stdin (it holds the slave PTY fd),
//!   wait for it, print `DESCENDANT-DONE`, exit 0;
//! - `title <t>`: print `READY`, wait 100 ms, write `\x1b]0;<t>\x07` to
//!   stdout, exit 0;
//! - `check <marker> <q-hex> <expect-hex>`: print `READY`, write the
//!   hex-decoded query bytes to stdout, then read stdin in chunks until the
//!   contiguous hex-decoded expect bytes are seen (print `CHECK:<marker>`,
//!   exit 0) or stdin reaches EOF first (print `CHECKFAIL:<marker>`, exit 0);
//! - `flood <total>`: print `READY`, write `total` bytes of a deterministic
//!   repeating pattern to stdout (flushed every 4 KiB), then read stdin
//!   until EOF (stay alive) — a sustained output load for the N28
//!   multi-session benchmark;
//! - `checkhold <marker> <q-hex> <expect-hex>`: like `check`, but after
//!   printing the verdict the child keeps reading stdin until EOF instead of
//!   exiting, so a pump test can read the verdict from a non-terminated
//!   tick's snapshot;
//! - `osc`: print `READY`, wait 100 ms, then emit the OSC 133 A/B/C/D
//!   marker frames, an OSC 7 cwd frame and an OSC 8 hyperlink frame (each
//!   ST-terminated, 100 ms apart), print `OSC-DONE`, exit 0;
//! - anything else: echo fallback.

use std::io::{self, BufRead, Read, Write};

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Like a full-screen application, the helper switches its own tty to
    // raw mode: tests send escape sequences and control bytes and expect
    // them byte-exact. The emulator leaves the line discipline cooked
    // (audit P2); applications choose raw themselves.
    make_stdin_raw();
    let mut out = io::stdout().lock();
    match args.first().map(String::as_str) {
        None => echo(&mut out),
        Some("argv") => {
            for (index, arg) in args.iter().skip(1).enumerate() {
                let _ = writeln!(out, "A:{} {arg}", index + 1);
                let _ = out.flush();
            }
        }
        Some("pwd") => {
            let dir = std::env::current_dir().unwrap_or_default();
            let _ = writeln!(out, "PWD:{}", dir.display());
            let _ = out.flush();
        }
        Some("status") => {
            ready(&mut out);
            let code = args
                .get(1)
                .and_then(|arg| arg.parse::<i32>().ok())
                .unwrap_or(0);
            std::process::exit(code);
        }
        Some("stop") => {
            let limit = args.get(1).map(String::as_str).unwrap_or("0");
            stop(&mut out, limit);
        }
        Some("flood") => {
            let total = args.get(1).map(String::as_str).unwrap_or("0");
            flood(&mut out, total);
        }
        Some("ws") => winsize(&mut out),
        Some("winch") => winch(&mut out),
        Some("dying") => {
            ready(&mut out);
            // Let READY reach the screen through the loop's normal master
            // read before the query goes out and the child exits (N06a
            // decision 9).
            std::thread::sleep(std::time::Duration::from_millis(100));
            let query = hex_decode(args.get(1).map(String::as_str).unwrap_or(""));
            let _ = out.write_all(&query);
            let _ = out.flush();
            std::process::exit(0);
        }
        Some("title") => {
            ready(&mut out);
            // Let READY reach the screen through the loop's normal master
            // read before the title goes out and the child exits.
            std::thread::sleep(std::time::Duration::from_millis(100));
            let title = args.get(1).map(String::as_str).unwrap_or("");
            let _ = out.write_all(format!("\x1b]0;{title}\x07").as_bytes());
            let _ = out.flush();
            std::process::exit(0);
        }
        Some("apc") => {
            ready(&mut out);
            // Let READY reach the screen through the loop's normal master
            // read before the APC goes out (N32b).
            std::thread::sleep(std::time::Duration::from_millis(100));
            let b64 = args.get(1).map(String::as_str).unwrap_or("");
            // kitty-graphics APC (real protocol, per ADR-011):
            // ESC _ G<control data>;<base64> ESC \. A 2x2 RGBA transmit; the
            // test only checks that the APC reaches the shared queue.
            let payload = format!("Gs=2,v=2;{b64}");
            let _ = out.write_all(format!("\x1b_{payload}\x1b\\").as_bytes());
            let _ = out.flush();
            // Let the APC reach the master before the marker.
            std::thread::sleep(std::time::Duration::from_millis(100));
            let _ = writeln!(out, "APC-DONE");
            let _ = out.flush();
            std::process::exit(0);
        }
        Some("osc") => {
            ready(&mut out);
            // Let READY reach the screen through the loop's normal master
            // read before the OSC frames go out (N35).
            std::thread::sleep(std::time::Duration::from_millis(100));
            // Shell-integration frames, each ST-terminated and spaced 100 ms
            // apart so each frame reaches the master in its own read batch:
            // OSC 133 A/B/C/D markers, an OSC 7 cwd, and an OSC 8 hyperlink.
            let frames = [
                "\x1b]133;A\x1b\\",
                "\x1b]133;B\x1b\\",
                "\x1b]133;C\x1b\\",
                "\x1b]133;D\x1b\\",
                "\x1b]7;file:///tmp\x1b\\",
                "\x1b]8;;https://example.com/x\x1b\\",
            ];
            for frame in frames {
                let _ = out.write_all(frame.as_bytes());
                let _ = out.flush();
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let _ = writeln!(out, "OSC-DONE");
            let _ = out.flush();
            std::process::exit(0);
        }
        Some("check") => {
            let marker = args.get(1).map(String::as_str).unwrap_or("?");
            let query = hex_decode(args.get(2).map(String::as_str).unwrap_or(""));
            let expect = hex_decode(args.get(3).map(String::as_str).unwrap_or(""));
            check(&mut out, marker, &query, &expect);
        }
        Some("checkhold") => {
            let marker = args.get(1).map(String::as_str).unwrap_or("?");
            let query = hex_decode(args.get(2).map(String::as_str).unwrap_or(""));
            let expect = hex_decode(args.get(3).map(String::as_str).unwrap_or(""));
            check_hold(&mut out, marker, &query, &expect);
        }
        Some("descendant") => {
            descendant(&mut out, args.get(1).map(String::as_str).unwrap_or("0"));
        }
        Some(_) => echo(&mut out),
    }
}

/// Print `READY` and flush.
fn ready(out: &mut impl Write) {
    let _ = writeln!(out, "READY");
    let _ = out.flush();
}

/// Read stdin until at least `limit` bytes have been received and a
/// newline byte has been seen, print `STOPPED`, then read until EOF.
fn stop(out: &mut impl Write, limit: &str) {
    // READY once stdin is raw (set in main): input sent before that is
    // subject to the canonical line limit (1 KiB on macOS).
    ready(out);
    let target = limit.parse::<usize>().unwrap_or(0);
    let mut input = io::stdin().lock();
    let mut buffer = [0u8; 4096];
    let mut received = 0;
    let mut newline_seen = false;
    let mut stopped = false;
    loop {
        let n = match input.read(&mut buffer) {
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        received += n;
        if !newline_seen && buffer[..n].contains(&b'\n') {
            newline_seen = true;
        }
        if !stopped && received >= target && newline_seen {
            stopped = true;
            let _ = writeln!(out, "STOPPED");
            let _ = out.flush();
        }
    }
}

/// Write `total` bytes of a deterministic repeating pattern to `out`,
/// flushing every 4 KiB, then read stdin until EOF (stay alive).
///
/// Used by the N28 multi-session benchmark as a sustained output load:
/// each flood pane keeps its PTY master, the pinned reader and the shared
/// parse lock busy while the probe pane's input latency is measured. The
/// pattern is a fixed repeating sequence, so the load is deterministic
/// without passing a corpus to the child.
fn flood(out: &mut impl Write, _total: &str) {
    ready(out);

    // Deterministic repeating pattern: "0123456789\r\n" (12 bytes).
    let pattern = b"0123456789\r\n";
    let mut buf = Vec::with_capacity(4096);
    while buf.len() < 4096 {
        buf.extend_from_slice(pattern);
    }
    buf.truncate(4096);

    // Write the pattern continuously (flushing each 4 KiB chunk) until the
    // session closes: a sustained output load for as long as the pane is
    // alive. When the session closes, the slave PTY closes and the writes
    // fail with EPIPE, ending the loop.
    loop {
        if out.write_all(&buf).is_err() {
            break;
        }
        let _ = out.flush();
    }
}

/// Print the terminal winsize actually in effect on stdin.
/// Set by the SIGWINCH handler installed in [`winch`].
static WINCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_winch(_signal: libc::c_int) {
    // Async-signal-safe: a single atomic store.
    WINCH.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Put stdin into raw mode when it is a terminal (`cfmakeraw`).
fn make_stdin_raw() {
    // SAFETY: `termios` is a plain C struct; zeroed is a valid input that
    // `tcgetattr` overwrites before any read, and `tcsetattr` only runs
    // after it succeeded. fd 0 is this process's stdin.
    unsafe {
        if libc::isatty(0) == 1 {
            let mut termios: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut termios) == 0 {
                libc::cfmakeraw(&mut termios);
                libc::tcsetattr(0, libc::TCSANOW, &termios);
            }
        }
    }
}

/// `READY`, then wait up to 5 s for SIGWINCH; print `WINCH` with the
/// kernel's window size and the number of bytes pending on stdin
/// (`PENDING:0` proves nothing was injected as input on resize), exit 0.
fn winch(out: &mut impl Write) {
    // SAFETY: installs an async-signal-safe handler (one atomic store).
    unsafe {
        libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
    }
    ready(out);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !WINCH.load(std::sync::atomic::Ordering::SeqCst) {
        if std::time::Instant::now() > deadline {
            let _ = writeln!(out, "NOWINCH");
            let _ = out.flush();
            std::process::exit(1);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // Let any (wrongly) injected bytes arrive before counting them.
    std::thread::sleep(std::time::Duration::from_millis(100));
    let mut pending: libc::c_int = 0;
    // SAFETY: FIONREAD writes one c_int; `pending` outlives the call.
    unsafe { libc::ioctl(0, libc::FIONREAD as libc::c_ulong, &mut pending) };
    let _ = write!(out, "WINCH ");
    winsize(out);
    let _ = writeln!(out, "PENDING:{pending}");
    let _ = out.flush();
    std::process::exit(0);
}

fn winsize(out: &mut impl Write) {
    // SAFETY: `libc::winsize` is a plain struct of four `u16` fields with
    // no invariants; zeroed is a valid input, and TIOCGWINSZ only writes
    // those four fields.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    unsafe { libc::ioctl(0, libc::TIOCGWINSZ as libc::c_ulong, &mut size) };
    let _ = writeln!(out, "WS:{};{}", size.ws_row, size.ws_col);
    let _ = out.flush();
}

/// Decode pairs of hex digits into bytes (non-hex pairs are ignored).
fn hex_decode(hex: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks(2) {
        if let Ok(pair) = std::str::from_utf8(pair) {
            out.extend(u8::from_str_radix(pair, 16).ok());
        }
    }
    out
}

/// `READY`, then the query bytes, then wait on stdin for `expect` as a
/// contiguous byte sequence: `CHECK:<marker>` when seen, `CHECKFAIL:<marker>`
/// when stdin reaches EOF first; exit 0 either way.
fn check(out: &mut impl Write, marker: &str, query: &[u8], expect: &[u8]) {
    ready(out);
    let _ = out.write_all(query);
    let _ = out.flush();
    let mut input = io::stdin().lock();
    let mut seen: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut found = false;
    loop {
        let n = match input.read(&mut chunk) {
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        seen.extend_from_slice(&chunk[..n]);
        if contains_sequence(&seen, expect) {
            found = true;
            break;
        }
        // Keep only a tail that can still complete a match, so a long-lived
        // stream never grows the buffer without bound.
        let keep = expect.len().saturating_sub(1);
        if seen.len() > keep {
            seen.drain(..seen.len() - keep);
        }
    }
    if found {
        let _ = writeln!(out, "CHECK:{marker}");
    } else {
        let _ = writeln!(out, "CHECKFAIL:{marker}");
    }
    let _ = out.flush();
}

/// Like [`check`], but after printing the verdict the child keeps running
/// (reading stdin until EOF) instead of exiting. This lets a pump test read
/// the verdict from a non-terminated tick's snapshot: a terminated tick
/// probes nothing, so a child that exits on the verdict would race the
/// confirmation output with the termination report.
fn check_hold(out: &mut impl Write, marker: &str, query: &[u8], expect: &[u8]) {
    ready(out);
    let _ = out.write_all(query);
    let _ = out.flush();
    let mut input = io::stdin().lock();
    let mut seen: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut found = false;
    loop {
        let n = match input.read(&mut chunk) {
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        seen.extend_from_slice(&chunk[..n]);
        if contains_sequence(&seen, expect) {
            found = true;
            break;
        }
        // Keep only a tail that can still complete a match, so a long-lived
        // stream never grows the buffer without bound.
        let keep = expect.len().saturating_sub(1);
        if seen.len() > keep {
            seen.drain(..seen.len() - keep);
        }
    }
    if found {
        let _ = writeln!(out, "CHECK:{marker}");
    } else {
        let _ = writeln!(out, "CHECKFAIL:{marker}");
    }
    let _ = out.flush();
    // Keep the child alive after the verdict: read stdin until EOF so the
    // session stays live and the verdict is observable in a snapshot.
    loop {
        let n = match input.read(&mut chunk) {
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
    }
}

/// True when `buffer` contains `needle` as a contiguous subsequence (an
/// empty needle is found anywhere).
fn contains_sequence(buffer: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    buffer.windows(needle.len()).any(|window| window == needle)
}

/// `READY`, then spawn `/bin/sleep <hold_ms as seconds>` with inherited
/// stdin (it holds the slave PTY fd for the whole hold), wait for it,
/// print `DESCENDANT-DONE`, and exit 0 (N12a).
fn descendant(out: &mut impl Write, hold_ms: &str) {
    ready(out);
    let ms = hold_ms.parse::<u64>().unwrap_or(0);
    // Convert milliseconds to a seconds string, e.g. 300 -> "0.300".
    let seconds = format!("{}.{:03}", ms / 1000, ms % 1000);
    // Inherited stdin: the grandchild keeps the slave PTY fd open for
    // the whole hold, exercising the bounded final drain (N12a).
    let mut command = std::process::Command::new("/bin/sleep");
    command.arg(seconds);
    command.stdin(std::process::Stdio::inherit());
    let _ = command.status();
    let _ = writeln!(out, "DESCENDANT-DONE");
    let _ = out.flush();
    std::process::exit(0);
}

/// `READY`, then echo each stdin line as `E:<line>`; exit 0 on EOF.
fn echo(out: &mut impl Write) {
    ready(out);
    for line in io::stdin().lock().lines() {
        let line = line.unwrap_or_default();
        let _ = writeln!(out, "E:{line}");
        let _ = out.flush();
    }
}
