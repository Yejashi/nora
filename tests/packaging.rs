//! Packaging tests (N30): install artifacts, install/uninstall scripts,
//! desktop entry and example config.
//!
//! All tests are headless-safe (no display or GPU required). The
//! build-centric tests run the real scripts against a temporary
//! `HOME` / `CARGO_TARGET_DIR`, so they never touch a developer's real
//! install or the shared target directory.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// A unique temporary directory for one test run, removed when the test
/// ends -- pass or fail. Without that every run leaked one: the clean-build
/// test's is a whole ~800 MB release build, and nearly 900 of them filled
/// the system disk during a long session (2026-10-14). A failing script's
/// output is already in the panic message, so nothing is lost by removing
/// the directory after a failure too.
struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir() -> TempDir {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before the Unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "nora-packaging-{}-{nanos}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    TempDir(dir)
}

/// Run one of the repository's shell scripts with a controlled
/// environment, capturing its output for failure reports.
fn run_script(script: &str, env: &[(String, String)]) -> std::process::Output {
    run_script_without(script, env, &[])
}

/// [`run_script`] with the variables `remove` taken out of the inherited
/// environment.
fn run_script_without(
    script: &str,
    env: &[(String, String)],
    remove: &[&str],
) -> std::process::Output {
    let mut cmd = Command::new("bash");
    cmd.arg(format!("./scripts/{script}"))
        .current_dir(repo_root())
        .stdin(std::process::Stdio::null());
    for key in remove {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output()
        .unwrap_or_else(|err| panic!("failed to run scripts/{script}: {err}"))
}

/// Environment for the install/uninstall tests: a temporary `HOME`
/// isolates the install destinations, while the ambient `CARGO_HOME`
/// is preserved so the scripts' `cargo build` reuses the existing
/// registry and toolchain instead of re-downloading into the
/// temporary home.
fn home_env(home: &Path) -> Vec<(String, String)> {
    let mut env = vec![
        ("HOME".to_string(), home.to_str().unwrap().to_string()),
        (
            "XDG_CONFIG_HOME".to_string(),
            home.join(".config").to_str().unwrap().to_string(),
        ),
    ];
    // Pin CARGO_HOME to the ambient cargo setup (explicit, or the
    // default `$HOME/.cargo`) so the build reuses the existing
    // registry and toolchain.
    let cargo_home = match std::env::var("CARGO_HOME") {
        Ok(cargo_home) => Some(cargo_home),
        Err(_) => std::env::var("HOME")
            .ok()
            .map(|home| format!("{home}/.cargo")),
    };
    if let Some(cargo_home) = cargo_home {
        env.push(("CARGO_HOME".to_string(), cargo_home));
    }
    env
}

fn assert_script_ok(out: &std::process::Output, script: &str) {
    if !out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        panic!(
            "scripts/{script} failed with {:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
            out.status.code()
        );
    }
}

/// Acceptance: a clean checkout builds reproducibly. `cargo clean` +
/// `cargo build --release` succeeds with no prior build state, and the
/// resulting binary runs.
#[test]
fn clean_build_succeeds() {
    let target = temp_dir();
    let out = run_script(
        "clean-build.sh",
        &[(
            "CARGO_TARGET_DIR".to_string(),
            target.to_str().unwrap().to_string(),
        )],
    );
    assert_script_ok(&out, "clean-build.sh");

    let bin = target.join("release/nora");
    assert!(bin.is_file(), "clean build must produce {}", bin.display());
    let version = Command::new(&bin)
        .arg("--version")
        .output()
        .expect("run the freshly built binary");
    assert!(version.status.success(), "fresh binary must run");
    let stdout = String::from_utf8_lossy(&version.stdout);
    assert!(
        stdout.contains("nora"),
        "unexpected --version output: {stdout}"
    );
}

/// Acceptance: the verified Linux install recipe installs the binary,
/// terminfo, desktop entry, icon and (when absent) a starting config.
#[test]
fn install_script_runs() {
    let home = temp_dir();
    let out = run_script("install.sh", &home_env(&home));
    assert_script_ok(&out, "install.sh");

    let installed = home.join(".local/bin/nora");
    assert!(
        installed.is_file(),
        "binary must be installed to ~/.local/bin/nora"
    );
    let version = Command::new(&installed)
        .arg("--version")
        .output()
        .expect("run the installed binary");
    assert!(version.status.success(), "installed binary must run");
    let stdout = String::from_utf8_lossy(&version.stdout);
    assert!(
        stdout.contains("nora"),
        "unexpected --version output: {stdout}"
    );

    // Only a compiled entry is usable, so without `tic` none is installed
    // (nora then sets TERM=xterm-256color).
    let has_tic = Command::new("tic")
        .arg("-V")
        .output()
        .is_ok_and(|out| out.status.success());
    assert_eq!(
        home.join(".terminfo/n/nora").is_file(),
        has_tic,
        "a compiled terminfo entry is installed exactly when tic exists"
    );
    assert!(
        home.join(".local/share/applications/nora.desktop")
            .is_file(),
        "desktop entry must be installed"
    );
    assert!(
        home.join(".local/share/icons/hicolor/128x128/apps/nora.png")
            .is_file(),
        "icon must be installed"
    );
    // No prior config: the example config is installed as a starting point.
    let config = home.join(".config/nora/config.toml");
    assert!(
        config.is_file(),
        "example config must be installed when absent"
    );
}

/// install.sh installs the binary from the target directory cargo used,
/// not `target/release/nora`, however it was chosen: `CARGO_TARGET_DIR`,
/// `CARGO_BUILD_TARGET_DIR` or `build.target-dir` in a cargo config (the
/// script asks `cargo metadata`), and `CARGO_TARGET_DIR` when cargo
/// cannot say. A stub `cargo` first on PATH "builds" a marker script into
/// the directory the case configured and passes `metadata` to the real
/// cargo (or fails it), so the test needs no release build and fails if
/// the script installs from anywhere else.
#[test]
fn install_script_honors_cargo_target_dir() {
    use std::os::unix::fs::PermissionsExt;

    let real_cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    // (case, the variable that names the directory, whether `cargo
    // metadata` works)
    let cases = [
        ("env", Some("CARGO_TARGET_DIR"), true),
        ("build-env", Some("CARGO_BUILD_TARGET_DIR"), true),
        ("config", None, true),
        ("no-metadata", Some("CARGO_TARGET_DIR"), false),
    ];
    for (name, variable, metadata) in cases {
        let root = temp_dir();
        let (home, stub_bin, target) = (root.join("home"), root.join("bin"), root.join("tgt"));
        for dir in [&home, &stub_bin, &target] {
            std::fs::create_dir_all(dir).expect("create test dir");
        }
        let cargo = stub_bin.join("cargo");
        let metadata_arm = if metadata {
            format!("exec '{real_cargo}' \"$@\"")
        } else {
            "exit 101".to_string()
        };
        std::fs::write(
            &cargo,
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = metadata ]; then {metadata_arm}; fi\n\
                 mkdir -p \"$NORA_TEST_TARGET/release\"\n\
                 printf '#!/bin/sh\\necho nora from-{name}\\n' > \"$NORA_TEST_TARGET/release/nora\"\n\
                 chmod 0755 \"$NORA_TEST_TARGET/release/nora\"\n"
            ),
        )
        .expect("write stub cargo");
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub cargo");

        let mut env = home_env(&home);
        let path = std::env::var("PATH").unwrap_or_default();
        env.push(("PATH".into(), format!("{}:{path}", stub_bin.display())));
        env.push(("NORA_TEST_TARGET".into(), target.display().to_string()));
        match variable {
            Some(variable) => env.push((variable.into(), target.display().to_string())),
            None => {
                // A cargo home of the test's own, whose config names the
                // directory (`metadata --no-deps` needs no registry).
                let cargo_home = root.join("cargo-home");
                std::fs::create_dir_all(&cargo_home).expect("create cargo home");
                std::fs::write(
                    cargo_home.join("config.toml"),
                    format!("[build]\ntarget-dir = \"{}\"\n", target.display()),
                )
                .expect("write cargo config");
                env.retain(|(key, _)| key != "CARGO_HOME");
                env.push(("CARGO_HOME".into(), cargo_home.display().to_string()));
            }
        }
        let out = run_script_without(
            "install.sh",
            &env,
            &["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"],
        );
        assert_script_ok(&out, "install.sh");

        let version = Command::new(home.join(".local/bin/nora"))
            .arg("--version")
            .output()
            .expect("run the installed binary");
        assert_eq!(
            String::from_utf8_lossy(&version.stdout).trim(),
            format!("nora from-{name}"),
            "{name}: install.sh must install the binary from the target directory cargo used"
        );
    }
}

/// Acceptance: the desktop entry carries the required keys and passes
/// `desktop-file-validate` when that tool is available.
#[test]
fn desktop_entry_valid() {
    let path = repo_root().join("resources/nora.desktop");
    let text = std::fs::read_to_string(&path).expect("resources/nora.desktop must exist");

    let mut keys: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("desktop entry line must be key=value: {line}"));
        keys.entry(key.trim()).or_default().push(value.trim());
    }

    assert!(keys.contains_key("Name"), "Name is required");
    let exec = keys
        .get("Exec")
        .and_then(|values| values.first())
        .copied()
        .expect("Exec is required");
    assert!(
        exec == "nora",
        "Exec must launch nora from PATH, got: {exec}"
    );
    // The freedesktop spec forbids shell metacharacters in Exec.
    assert!(
        !exec.contains(|c: char| c.is_whitespace()
            || matches!(
                c,
                ';' | '&' | '|' | '$' | '`' | '(' | ')' | '<' | '>' | '\n'
            )),
        "Exec must not contain shell metacharacters: {exec}"
    );
    assert!(keys.contains_key("Icon"), "Icon is required");
    let categories = keys
        .get("Categories")
        .and_then(|values| values.first())
        .copied()
        .expect("Categories is required");
    assert!(
        categories.contains("TerminalEmulator"),
        "Categories must include TerminalEmulator, got: {categories}"
    );

    // Use the desktop-file validator when it is installed.
    if let Ok(output) = Command::new("desktop-file-validate").arg(&path).output() {
        assert!(
            output.status.success(),
            "desktop-file-validate rejected resources/nora.desktop:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Acceptance: the shipped example config loads and validates.
#[test]
fn config_example_valid() {
    let path = repo_root().join("resources/nora.example.toml");
    let loaded = nora::config::load_config(Some(path.to_str().unwrap()))
        .unwrap_or_else(|err| panic!("example config must load: {err:?}"));
    assert_eq!(loaded.path.as_deref(), Some(path.as_path()));
    let errors = loaded.config.validate();
    assert!(
        errors.is_empty(),
        "example config must validate: {errors:?}"
    );
}

/// Acceptance: uninstall removes the installed artifacts but preserves
/// the user config.
#[test]
fn uninstall_preserves_config() {
    let home = temp_dir();
    let env = home_env(&home);

    let out = run_script("install.sh", &env);
    assert_script_ok(&out, "install.sh");

    let config = home.join(".config/nora/config.toml");
    let before = std::fs::read(&config).expect("install must create the config");

    let out = run_script("uninstall.sh", &env);
    assert_script_ok(&out, "uninstall.sh");

    assert!(
        !home.join(".local/bin/nora").exists(),
        "binary must be removed"
    );
    assert!(
        !home.join(".terminfo/n/nora").exists(),
        "terminfo entry must be removed"
    );
    assert!(
        !home.join(".local/share/applications/nora.desktop").exists(),
        "desktop entry must be removed"
    );
    assert!(
        !home
            .join(".local/share/icons/hicolor/128x128/apps/nora.png")
            .exists(),
        "icon must be removed"
    );
    assert_eq!(
        std::fs::read(&config).expect("uninstall must preserve the user config"),
        before,
        "uninstall must preserve the user config"
    );
}
