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
//! - anything else: echo fallback.

use std::io::{self, BufRead, Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
        Some("ws") => winsize(&mut out),
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

/// Print the terminal winsize actually in effect on stdin.
fn winsize(out: &mut impl Write) {
    // SAFETY: `libc::winsize` is a plain struct of four `u16` fields with
    // no invariants; zeroed is a valid input, and TIOCGWINSZ only writes
    // those four fields.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    unsafe { libc::ioctl(0, libc::TIOCGWINSZ as libc::c_ulong, &mut size) };
    let _ = writeln!(out, "WS:{};{}", size.ws_row, size.ws_col);
    let _ = out.flush();
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
