//! N29: validated terminal identity and SSH fallback.
//!
//! These tests verify that the `nora` terminfo entry (in
//! `terminfo/nora.terminfo`) compiles with `tic`, reads back with `infocmp`,
//! advertises only tested capabilities, and that the SSH fallback is
//! documented. They require `tic` and `infocmp` (ncurses) on the PATH.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

/// The terminfo source file, relative to the crate root.
const TERMINFO_SRC: &str = "terminfo/nora.terminfo";

/// Capability names that must be advertised by the compiled entry. The set
/// is derived from nora's tested behavior (cursor addressing, key
/// sequences, SGR styles, modes, colors, erase/clear) — see ADR-010.
///
/// These are capability *names* (not name=value pairs) so the check is
/// robust to `infocmp`'s rendering of escape sequences (e.g. it prints
/// `\177` as `^?` and `\E[H` as `\E[H`).
const EXPECTED_CAPS: &[&str] = &[
    // Boolean capabilities.
    "am", "bce", "km", "mir", "msgr", "xenl", // Numeric capabilities.
    "colors", "pairs", "cols", "lines", "it", // Cursor addressing.
    "cup", "home", // Cursor movement (application → terminal).
    "cub1", "cud1", "cuf1", "cuu1",
    // Cursor keys (terminal → application, app mode).
    "kcub1", "kcud1", "kcuf1", "kcuu1", // Keys.
    "kbs", "kHOM", "kEND", "kIC", "kDC", "kLFT", "kRIT", "kf1", "kf2", "kf3", "kf4", "kf5", "kf6",
    "kf7", "kf8", "kf9", "kf10", "kf11", "kf12", // Styles (SGR).
    "bold", "dim", "blink", "invis", "smso", "smul", "rev", "rmso", "rmul", // Modes.
    "smcup", "rmcup", "smam", "rmam", "smkx", "rmkx",  // Colors.
    "initc", // Erase/clear.
    "clear", "ed", "el", "el1", "dch1", "dl1", // Cursor visibility.
    "civis", "cnorm", // Basic.
    "cr", "ind", "bel", "ht",
];

/// Capabilities that must NOT be advertised: nora does not implement kitty
/// graphics, image protocols, or xterm-specific key encodings. Advertising
/// them would mislead applications into emitting sequences nora does not
/// understand.
const UNSUPPORTED_CAPS: &[&str] = &[
    "kitty",
    "kitty-graphics",
    "kitty_keyboard",
    "kimg",
    "xterm-kitty",
];

/// Shared lock so parallel tests don't race on the temp directory.
static COMPILE_LOCK: Mutex<()> = Mutex::new(());

/// Compile the terminfo source into a fresh temp directory and return the
/// directory path. Panics if `tic` is unavailable or the compile fails.
fn compile_terminfo() -> PathBuf {
    let _guard = COMPILE_LOCK.lock().expect("terminfo compile lock poisoned");

    let tmp = std::env::temp_dir().join(format!(
        "nora-terminfo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).expect("creating temp terminfo dir");

    let src = PathBuf::from(TERMINFO_SRC);
    assert!(
        src.is_file(),
        "terminfo source not found at {TERMINFO_SRC} (run from the crate root)"
    );

    let output = Command::new("tic")
        .args(["-x", "-o"])
        .arg(&tmp)
        .arg(&src)
        .output()
        .expect("running tic (is ncurses installed?)");

    assert!(
        output.status.success(),
        "tic failed to compile {TERMINFO_SRC}:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // The compiled entry lands at <tmp>/n/nora.
    let compiled = tmp.join("n").join("nora");
    assert!(compiled.is_file(), "tic did not produce {compiled:?}");

    tmp
}

/// Run `infocmp -1 nora` against a compiled terminfo directory and return
/// its stdout.
fn infocmp_readback(terminfo_dir: &std::path::Path) -> String {
    let output = Command::new("infocmp")
        .args(["-1", "nora"])
        .env("TERMINFO", terminfo_dir)
        .output()
        .expect("running infocmp (is ncurses installed?)");

    assert!(
        output.status.success(),
        "infocmp failed to read back 'nora':\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Extract the capability names from an `infocmp -1` dump. Each capability
/// line is `name`, `name#value`, or `name=value`; the name is the token
/// before `=`, `#`, or the end of the line.
fn capability_names(dump: &str) -> Vec<String> {
    dump.lines()
        .filter_map(|line| {
            let line = line.trim().trim_end_matches(',').trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("nora|") {
                return None;
            }
            let name = line.split(['=', '#', ' ']).next().unwrap_or("");
            if name.is_empty() {
                return None;
            }
            Some(name.to_string())
        })
        .collect()
}

/// `terminfo_compiles`: the terminfo file compiles with `tic`.
#[test]
fn terminfo_compiles() {
    let tmp = compile_terminfo();
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `terminfo_readback`: the compiled terminfo can be read back with
/// `infocmp`.
#[test]
fn terminfo_readback() {
    let tmp = compile_terminfo();
    let dump = infocmp_readback(&tmp);
    assert!(
        dump.contains("nora|"),
        "infocmp did not emit the 'nora' entry:\n{dump}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `terminfo_capabilities_match`: the capabilities match the tested
/// behavior. Every capability in `EXPECTED_CAPS` must be advertised by the
/// compiled entry.
#[test]
fn terminfo_capabilities_match() {
    let tmp = compile_terminfo();
    let dump = infocmp_readback(&tmp);
    let names = capability_names(&dump);
    let mut missing = Vec::new();
    for cap in EXPECTED_CAPS {
        if !names.iter().any(|name| name == cap) {
            missing.push(cap.to_string());
        }
    }
    assert!(
        missing.is_empty(),
        "capabilities missing from the 'nora' terminfo entry:\n  {}",
        missing.join("\n  ")
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `terminfo_no_unsupported_keys`: no unsupported keys are advertised. The
/// entry must not claim kitty graphics, image protocols, or xterm-specific
/// key encodings.
#[test]
fn terminfo_no_unsupported_keys() {
    let tmp = compile_terminfo();
    let dump = infocmp_readback(&tmp);
    let names = capability_names(&dump);
    let mut unsupported = Vec::new();
    for cap in UNSUPPORTED_CAPS {
        if names.iter().any(|name| name == cap) {
            unsupported.push(cap.to_string());
        }
    }
    assert!(
        unsupported.is_empty(),
        "unsupported capabilities advertised by the 'nora' terminfo entry:\n  {}",
        unsupported.join("\n  ")
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `terminfo_ssh_fallback`: the SSH fallback is documented. The ADR and the
/// build/release docs must explain the installation path and the
/// `xterm-256color` fallback for remote hosts.
#[test]
fn terminfo_ssh_fallback() {
    let adr =
        std::fs::read_to_string("docs/adr/ADR-010-terminfo-identity.md").expect("reading ADR-010");
    assert!(
        adr.contains("xterm-256color"),
        "ADR-010 does not document the xterm-256color fallback"
    );
    assert!(
        adr.contains("COLORTERM"),
        "ADR-010 does not document COLORTERM handling"
    );
    assert!(
        adr.contains("TERM=nora"),
        "ADR-010 does not document setting TERM=nora"
    );

    let build =
        std::fs::read_to_string("docs/07-build-release.md").expect("reading 07-build-release.md");
    assert!(
        build.contains("xterm-256color"),
        "07-build-release.md does not document the xterm-256color fallback"
    );
    assert!(
        build.contains("tic"),
        "07-build-release.md does not document the tic installation path"
    );
}
