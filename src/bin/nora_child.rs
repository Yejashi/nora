//! Tagged-I/O child helper for the PTY session tests (N05).
//!
//! Behavior (every line flushed as it is printed):
//! - no args: print `READY`, then echo each stdin line as `E:<line>`;
//! - `argv a1 a2 ...`: print `A:<i> <arg>` for i = 1.., exit 0;
//! - `pwd`: print `PWD:<absolute cwd>`, exit 0;
//! - `status <n>`: print `READY`, exit `<n>` (missing `<n>` → 0);
//! - anything else: echo fallback.

use std::io::{self, BufRead, Write};

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
        Some(_) => echo(&mut out),
    }
}

/// Print `READY` and flush.
fn ready(out: &mut impl Write) {
    let _ = writeln!(out, "READY");
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
