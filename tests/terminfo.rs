//! N29: validated terminal identity and SSH fallback.
//!
//! These tests compile the `nora` terminfo entry (`terminfo/nora.terminfo`)
//! with `tic` into a temporary directory and read capabilities back through
//! `tput -T nora` with `TERMINFO` pointing there, so they check the
//! sequences curses programs actually get. They skip (pass with a note)
//! when ncurses' `tic`, `tput` or `infocmp` is not on the PATH.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The terminfo source file, relative to the crate root.
const TERMINFO_SRC: &str = "terminfo/nora.terminfo";

/// Capabilities whose `tput` output must match exactly, as
/// `(tput arguments, expected bytes)`. Key capabilities are what nora's
/// legacy encoder (`src/input.rs`) sends with `smkx` in effect.
const EXPECTED_SEQUENCES: &[(&[&str], &[u8])] = &[
    (&["setaf", "1"], b"\x1b[31m"),
    (&["setaf", "12"], b"\x1b[94m"),
    (&["setaf", "200"], b"\x1b[38;5;200m"),
    (&["setab", "1"], b"\x1b[41m"),
    (&["setab", "200"], b"\x1b[48;5;200m"),
    (&["sgr0"], b"\x1b(B\x1b[m"),
    (&["op"], b"\x1b[39;49m"),
    (&["csr", "0", "9"], b"\x1b[1;10r"),
    (&["sc"], b"\x1b7"),
    (&["rc"], b"\x1b8"),
    (&["ri"], b"\x1bM"),
    (&["cup", "2", "3"], b"\x1b[3;4H"),
    (&["smso"], b"\x1b[7m"),
    (&["rmso"], b"\x1b[27m"),
    (&["sitm"], b"\x1b[3m"),
    (&["smkx"], b"\x1b[?1h\x1b="),
    (&["kcuu1"], b"\x1bOA"),
    (&["khome"], b"\x1bOH"),
    (&["kend"], b"\x1bOF"),
    (&["kich1"], b"\x1b[2~"),
    (&["kdch1"], b"\x1b[3~"),
    (&["kpp"], b"\x1b[5~"),
    (&["knp"], b"\x1b[6~"),
    (&["kbs"], b"\x7f"),
    (&["kcbt"], b"\x1b[Z"),
    (&["kf5"], b"\x1b[15~"),
    // Shift+Left/Right, not PageUp/PageDown.
    (&["kLFT"], b"\x1b[1;2D"),
    (&["kRIT"], b"\x1b[1;2C"),
    (&["kmous"], b"\x1b[<"),
];

/// Capabilities nora does not implement, removed from the alacritty entry
/// it is derived from (see the header comment of the source file).
const REMOVED_CAPS: &[&str] = &[
    "blink", "flash", "mc0", "mc4", "mc5", "mc5i", "meml", "memu", "smm", "rmm", "kb2", "Ms",
];

/// Capabilities that must not be advertised: nora's kitty graphics and
/// keyboard support is negotiated at runtime, never claimed by terminfo.
const UNSUPPORTED_CAPS: &[&str] = &["kitty", "kitty-graphics", "kitty_keyboard", "kimg"];

/// Whether the ncurses tool `name` runs (`-V` prints its version).
fn have(name: &str) -> bool {
    Command::new(name)
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A compiled terminfo database in a fresh temp directory, removed on drop.
struct Compiled(PathBuf);

impl Drop for Compiled {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Compile the terminfo source into a fresh temp directory, or `None`
/// (with a note) when ncurses' tools are missing. Panics if the compile
/// fails or reports warnings.
fn compile_terminfo(test: &str) -> Option<Compiled> {
    if !(have("tic") && have("tput") && have("infocmp")) {
        eprintln!("skipping {test}: ncurses tic/tput/infocmp not on PATH");
        return None;
    }
    let dir = std::env::temp_dir().join(format!(
        "nora-terminfo-{test}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("creating temp terminfo dir");
    let compiled = Compiled(dir);

    let src = PathBuf::from(TERMINFO_SRC);
    assert!(
        src.is_file(),
        "terminfo source not found at {TERMINFO_SRC} (run from the crate root)"
    );
    let output = Command::new("tic")
        .args(["-x", "-o"])
        .arg(&compiled.0)
        .arg(&src)
        .output()
        .expect("running tic");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "tic failed to compile {TERMINFO_SRC}:\n{stderr}"
    );
    assert!(
        stderr.trim().is_empty(),
        "tic warned about {TERMINFO_SRC}:\n{stderr}"
    );
    assert!(
        compiled.0.join("n").join("nora").is_file(),
        "tic did not produce n/nora"
    );
    Some(compiled)
}

/// `tput -T nora <args>` against `dir`: the output bytes, or `None` when
/// the capability is absent (tput exits non-zero).
fn tput(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("tput")
        .arg("-T")
        .arg("nora")
        .args(args)
        .env("TERMINFO", dir)
        .env_remove("TERMINFO_DIRS")
        .output()
        .expect("running tput");
    output.status.success().then_some(output.stdout)
}

/// The capability names of the compiled entry (`infocmp -x -1 nora`).
fn capability_names(dir: &Path) -> Vec<String> {
    let output = Command::new("infocmp")
        .args(["-x", "-1", "nora"])
        .env("TERMINFO", dir)
        .env_remove("TERMINFO_DIRS")
        .output()
        .expect("running infocmp");
    assert!(
        output.status.success(),
        "infocmp failed to read back 'nora': {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dump = String::from_utf8_lossy(&output.stdout);
    assert!(
        dump.contains("nora|"),
        "infocmp did not emit 'nora':\n{dump}"
    );
    dump.lines()
        .filter(|line| line.starts_with('\t'))
        .filter_map(|line| {
            let line = line.trim().trim_end_matches(',');
            let name = line.split(['=', '#']).next()?;
            (!name.is_empty()).then(|| name.trim_end_matches('@').to_string())
        })
        .collect()
}

fn escaped(bytes: &[u8]) -> String {
    bytes.escape_ascii().to_string()
}

/// The entry compiles warning-free with `tic` and needs no other entry, so
/// a remote host without an alacritty entry can compile it.
#[test]
fn terminfo_compiles_standalone() {
    let source = std::fs::read_to_string(TERMINFO_SRC).expect("reading terminfo source");
    assert!(
        !source
            .lines()
            .any(|line| line.trim_start().starts_with("use=")),
        "{TERMINFO_SRC} must not depend on another entry via use="
    );
    let Some(dir) = compile_terminfo("compiles") else {
        return;
    };
    assert!(capability_names(&dir.0).iter().any(|name| name == "cup"));
}

/// The core output and key capabilities exist and give the sequences
/// nora implements and sends.
#[test]
fn terminfo_core_sequences() {
    let Some(dir) = compile_terminfo("sequences") else {
        return;
    };
    let mut wrong = Vec::new();
    for (args, expected) in EXPECTED_SEQUENCES {
        match tput(&dir.0, args) {
            Some(actual) if actual == *expected => {}
            Some(actual) => wrong.push(format!(
                "{}: got {} want {}",
                args.join(" "),
                escaped(&actual),
                escaped(expected)
            )),
            None => wrong.push(format!("{}: missing", args.join(" "))),
        }
    }
    assert!(
        wrong.is_empty(),
        "wrong capabilities:\n  {}",
        wrong.join("\n  ")
    );
    assert_eq!(tput(&dir.0, &["colors"]).as_deref(), Some(&b"256\n"[..]));
}

/// `sgr` composes attributes into one well-formed SGR sequence (bold,
/// underline and reverse; the alternate charset selects G0 line drawing).
#[test]
fn terminfo_sgr_is_sane() {
    let Some(dir) = compile_terminfo("sgr") else {
        return;
    };
    // sgr parameters: standout underline reverse blink dim bold invis
    // protect altcharset.
    let cases: &[(&[&str], &[u8])] = &[
        (
            &["0", "0", "0", "0", "0", "0", "0", "0", "0"],
            b"\x1b(B\x1b[0m",
        ),
        (
            &["0", "1", "0", "0", "0", "1", "0", "0", "0"],
            b"\x1b(B\x1b[0;1;4m",
        ),
        (
            &["1", "0", "0", "0", "0", "0", "0", "0", "0"],
            b"\x1b(B\x1b[0;7m",
        ),
        (
            &["0", "0", "1", "0", "1", "0", "1", "0", "0"],
            b"\x1b(B\x1b[0;2;7;8m",
        ),
        (
            &["0", "0", "0", "1", "0", "0", "0", "0", "0"],
            b"\x1b(B\x1b[0m",
        ),
        (
            &["0", "0", "0", "0", "0", "0", "0", "0", "1"],
            b"\x1b(0\x1b[0m",
        ),
    ];
    for (params, expected) in cases {
        let mut args = vec!["sgr"];
        args.extend_from_slice(params);
        let actual = tput(&dir.0, &args).expect("sgr is defined");
        assert_eq!(
            escaped(&actual),
            escaped(expected),
            "tput sgr {}",
            params.join(" ")
        );
    }
}

/// Capabilities nora does not implement are not advertised.
#[test]
fn terminfo_no_unsupported_caps() {
    let Some(dir) = compile_terminfo("unsupported") else {
        return;
    };
    let names = capability_names(&dir.0);
    let advertised: Vec<&str> = REMOVED_CAPS
        .iter()
        .chain(UNSUPPORTED_CAPS)
        .copied()
        .filter(|cap| names.iter().any(|name| name == cap))
        .collect();
    assert!(
        advertised.is_empty(),
        "unsupported capabilities advertised: {advertised:?}"
    );
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
