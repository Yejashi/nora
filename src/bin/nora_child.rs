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
//! - `checkhold <marker> <q-hex> <expect-hex>`: like `check`, but after
//!   printing the verdict the child keeps reading stdin until EOF instead of
//!   exiting, so a pump test can read the verdict from a non-terminated
//!   tick's snapshot;
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
