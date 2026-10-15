//! `nora_child`: the PTY test helper (see `unix.rs` for its modes). It
//! drives a real Unix tty (termios, window size, signals), so it exists
//! on Unix only; elsewhere it reports that and exits.

#[cfg(unix)]
mod unix;

#[cfg(unix)]
fn main() {
    unix::main();
}

#[cfg(not(unix))]
fn main() {
    eprintln!("nora_child: the PTY test helper is Unix-only");
    std::process::exit(2);
}
