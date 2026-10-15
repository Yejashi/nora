//! N18a configuration regression suite: types, defaults, validation, TOML
//! parsing, and XDG discovery in `nora::config`.
//!
//! All filesystem tests use unique temp directories (removed on drop). The
//! single test that mutates the process environment (`xdg_discovery`) is the
//! only one that does, so no env-var race is possible under parallel test
//! threads.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use nora::config::{
    ConfigChange, ConfigError, ConfigState, FontConfig, KeysConfig, NoraConfig, TerminalConfig,
    ThemeConfig, ZOOM_DEFAULT, ZOOM_MAX, ZOOM_MIN, classify_change, load_config,
    load_config_with_candidates, parse_config,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temp directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before UNIX epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "nora-config-test-{}-{}-{}",
            std::process::id(),
            n,
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_config(dir: &std::path::Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write config file");
    path
}

#[test]
fn default_config() {
    let c = NoraConfig::default();
    assert_eq!(c.terminal.history, 10_000);
    assert!(!c.terminal.mouse);
    assert_eq!(c.font.family, "monospace");
    assert_eq!(c.font.size, 15.0);
    assert_eq!(c.theme.background, "#000000");
    assert_eq!(c.theme.foreground, "#ffffff");
    assert_eq!(c.theme.cursor, "#ffffff");
    assert!(c.theme.colors.is_empty());
    assert!(c.keys.bindings.is_empty());
    // The built-in defaults must always be valid.
    assert!(c.validate().is_empty());
}

#[test]
fn validate_valid_config() {
    let c = NoraConfig {
        terminal: TerminalConfig {
            history: 5000,
            mouse: true,
        },
        font: FontConfig {
            family: "DejaVu Sans Mono".to_string(),
            size: 14.0,
        },
        theme: ThemeConfig {
            background: "#101418".to_string(),
            foreground: "#c5c8c6".to_string(),
            cursor: "#ffffff".to_string(),
            colors: vec!["#000000".to_string(), "#ff0000".to_string()],
        },
        keys: KeysConfig {
            bindings: HashMap::new(),
        },
        ..Default::default()
    };
    assert!(c.validate().is_empty());
}

#[test]
fn validate_invalid_history() {
    let mut c = NoraConfig::default();
    c.terminal.history = 1_000_001;
    let errors = c.validate();
    assert!(
        errors.iter().any(|e| e.starts_with("terminal.history")),
        "expected a history error, got: {errors:?}"
    );
}

#[test]
fn validate_invalid_font_size() {
    let mut zero = NoraConfig::default();
    zero.font.size = 0.0;
    assert!(
        zero.validate().iter().any(|e| e.starts_with("font.size")),
        "expected a font.size error for 0.0"
    );

    let mut negative = NoraConfig::default();
    negative.font.size = -1.0;
    assert!(
        negative
            .validate()
            .iter()
            .any(|e| e.starts_with("font.size")),
        "expected a font.size error for -1.0"
    );

    let mut huge = NoraConfig::default();
    huge.font.size = 1000.1;
    assert!(
        huge.validate().iter().any(|e| e.starts_with("font.size")),
        "expected a font.size error for 1000.1"
    );
}

#[test]
fn validate_invalid_color() {
    let mut c = NoraConfig::default();
    c.theme.background = "not-a-color".to_string();
    let errors = c.validate();
    assert!(
        errors.iter().any(|e| e.starts_with("theme.background")),
        "expected a theme.background error, got: {errors:?}"
    );

    let mut d = NoraConfig::default();
    d.theme.colors.push("zzz".to_string());
    assert!(
        d.validate()
            .iter()
            .any(|e| e.starts_with("theme.colors[0]")),
        "expected a theme.colors[0] error"
    );
}

#[test]
fn parse_valid_toml() {
    let toml_text = r##"
[terminal]
history = 2000
mouse = true

[font]
family = "DejaVu Sans Mono"
size = 14.0

[theme]
background = "#101418"
foreground = "#c5c8c6"
cursor = "#ffffff"
colors = ["#000000", "#ff0000"]

[keys]
bindings = { "Ctrl+Shift+C" = "copy" }
"##;
    let c = parse_config(toml_text).expect("valid TOML should parse");
    assert_eq!(c.terminal.history, 2000);
    assert!(c.terminal.mouse);
    assert_eq!(c.font.family, "DejaVu Sans Mono");
    assert_eq!(c.font.size, 14.0);
    assert_eq!(c.theme.background, "#101418");
    assert_eq!(c.theme.colors.len(), 2);
    assert_eq!(
        c.keys.bindings.get("Ctrl+Shift+C"),
        Some(&"copy".to_string())
    );
}

#[test]
fn parse_invalid_toml() {
    let toml_text = "[terminal\nhistory = 100";
    let err = parse_config(toml_text).expect_err("invalid TOML should fail");
    assert!(
        matches!(err, ConfigError::Parse { .. }),
        "expected a Parse error, got: {err:?}"
    );
}

#[test]
fn parse_unknown_field() {
    let toml_text = r#"
[terminal]
history = 100
not_a_real_field = 5
"#;
    let err = parse_config(toml_text).expect_err("unknown field should be rejected");
    assert!(
        matches!(err, ConfigError::Parse { .. }),
        "expected a Parse error for the unknown field, got: {err:?}"
    );
}

#[test]
fn parse_valid_but_out_of_range() {
    // Structurally valid TOML that fails *validation*, not parsing.
    let toml_text = "[terminal]\nhistory = 999999999\n";
    let err = parse_config(toml_text).expect_err("out-of-range history should fail");
    match err {
        ConfigError::Validation { errors, .. } => {
            assert!(errors.iter().any(|e| e.starts_with("terminal.history")));
        }
        other => panic!("expected a Validation error, got: {other:?}"),
    }
}

#[test]
fn load_config_defaults() {
    // No explicit path, no candidates -> built-in defaults, `path: None`.
    let loaded = load_config_with_candidates(None, &[]).expect("defaults should load");
    assert!(loaded.path.is_none());
    assert_eq!(loaded.config.history(), 10_000);
    assert!(!loaded.config.terminal.mouse);
    assert_eq!(loaded.config.font.family, "monospace");
}

#[test]
fn load_config_explicit_path() {
    let tmp = TempDir::new();
    let path = write_config(
        &tmp.0,
        "config.toml",
        "[terminal]\nhistory = 4321\nmouse = true\n",
    );
    let loaded = load_config(Some(path.to_str().unwrap())).expect("explicit path should load");
    assert_eq!(loaded.path.as_deref(), Some(path.as_path()));
    assert_eq!(loaded.config.terminal.history, 4321);
    assert!(loaded.config.terminal.mouse);
}

#[test]
fn load_config_nonexistent_path() {
    let missing = std::env::temp_dir()
        .join(format!("nora-missing-{}", std::process::id()))
        .join("config.toml");
    let err = load_config(Some(missing.to_str().unwrap())).expect_err("missing file should error");
    assert!(
        matches!(err, ConfigError::Io { .. }),
        "expected an Io error, got: {err:?}"
    );
}

#[test]
fn xdg_discovery() {
    let tmp = TempDir::new();
    let cfg_dir = tmp.0.join("nora");
    std::fs::create_dir_all(&cfg_dir).expect("create nora dir");
    let cfg_path = cfg_dir.join("config.toml");
    std::fs::write(&cfg_path, "[terminal]\nhistory = 4321\n").expect("write xdg config");

    let prev = std::env::var_os("XDG_CONFIG_HOME");
    // SAFETY: this is the only test in this binary that mutates
    // XDG_CONFIG_HOME, and no other thread reads or writes the process
    // environment concurrently.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", tmp.0.as_os_str());
    }

    let loaded = load_config(None).expect("should load from XDG_CONFIG_HOME");
    assert_eq!(loaded.path.as_deref(), Some(cfg_path.as_path()));
    assert_eq!(loaded.config.terminal.history, 4321);

    match prev {
        Some(v) => unsafe {
            std::env::set_var("XDG_CONFIG_HOME", v);
        },
        None => unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        },
    }
}

// --- N19a: transactional reload and change classification ------------------

/// `ConfigState::new` wraps a loaded config at epoch 0.
#[test]
fn config_state_new() {
    let loaded = load_config_with_candidates(None, &[]).expect("defaults should load");
    let state = ConfigState::new(loaded);
    assert_eq!(state.epoch, 0);
    assert!(state.path.is_none());
    assert_eq!(state.current.history(), 10_000);
}

/// A reload from a still-valid path succeeds, increments the epoch, and
/// preserves the source path.
#[test]
fn config_state_reload_success() {
    let tmp = TempDir::new();
    let path = write_config(&tmp.0, "config.toml", "[terminal]\nhistory = 2000\n");
    let loaded = load_config(Some(path.to_str().unwrap())).expect("explicit path should load");
    let state = ConfigState::new(loaded);
    assert_eq!(state.epoch, 0);
    assert_eq!(state.current.terminal.history, 2000);

    let reloaded = state.reload().expect("reload should succeed");
    assert_eq!(reloaded.epoch, 1);
    assert_eq!(reloaded.current.terminal.history, 2000);
    assert_eq!(reloaded.path.as_deref(), Some(path.as_path()));
}

/// A reload that fails validation is transactional: the error surfaces and
/// the original state is left untouched (epoch and config unchanged).
#[test]
fn config_state_reload_failure() {
    let tmp = TempDir::new();
    let path = write_config(&tmp.0, "config.toml", "[terminal]\nhistory = 2000\n");
    let loaded = load_config(Some(path.to_str().unwrap())).expect("explicit path should load");
    let state = ConfigState::new(loaded);
    assert_eq!(state.current.terminal.history, 2000);
    assert_eq!(state.epoch, 0);

    // Corrupt the file on disk; the reload must fail and keep the old state.
    std::fs::write(&path, "[terminal]\nhistory = 999999999\n").expect("rewrite config");
    let err = state
        .reload()
        .expect_err("invalid config should fail the reload");
    assert!(
        matches!(err, ConfigError::Validation { .. }),
        "expected a Validation error, got: {err:?}"
    );
    assert_eq!(
        state.current.terminal.history, 2000,
        "old config must be preserved"
    );
    assert_eq!(state.epoch, 0, "epoch must not advance on failure");
}

/// Color and key-binding changes are classified as `Immediate`.
#[test]
fn classify_change_immediate() {
    let old = NoraConfig::default();
    let mut new = NoraConfig::default();
    new.theme.background = "#101418".to_string();
    new.theme.foreground = "#c5c8c6".to_string();
    new.theme.cursor = "#00ff00".to_string();
    new.theme.colors.push("#ff0000".to_string());
    let mut bindings = HashMap::new();
    bindings.insert("Ctrl+Shift+C".to_string(), "copy".to_string());
    new.keys.bindings = bindings;

    let changes = classify_change(&old, &new);
    assert!(
        changes
            .iter()
            .all(|(_, c)| matches!(c, ConfigChange::Immediate)),
        "all theme/keys changes should be Immediate, got: {changes:?}"
    );
    let fields: Vec<&str> = changes.iter().map(|(field, _)| *field).collect();
    assert!(fields.contains(&"theme.background"));
    assert!(fields.contains(&"theme.foreground"));
    assert!(fields.contains(&"theme.cursor"));
    assert!(fields.contains(&"theme.colors"));
    assert!(fields.contains(&"keys.bindings"));
}

/// Font changes are classified as `Staged`.
#[test]
fn classify_change_staged() {
    let old = NoraConfig::default();
    let mut new = NoraConfig::default();
    new.font.family = "DejaVu Sans Mono".to_string();
    new.font.size = 14.0;

    let changes = classify_change(&old, &new);
    assert!(
        changes
            .iter()
            .all(|(_, c)| matches!(c, ConfigChange::Staged)),
        "font changes should be Staged, got: {changes:?}"
    );
    let fields: Vec<&str> = changes.iter().map(|(field, _)| *field).collect();
    assert!(fields.contains(&"font.family"));
    assert!(fields.contains(&"font.size"));
}

/// Scrollback history is a new-session launch setting.
#[test]
fn classify_change_new_session() {
    let old = NoraConfig::default();
    let mut new = NoraConfig::default();
    new.terminal.history = 5000;

    let changes = classify_change(&old, &new);
    assert_eq!(changes.len(), 1, "only history changed; got: {changes:?}");
    assert_eq!(changes[0].0, "terminal.history");
    assert!(matches!(changes[0].1, ConfigChange::NewSession));
}

/// Identical configs produce no changes.
#[test]
fn classify_change_no_change() {
    let old = NoraConfig::default();
    let new = NoraConfig::default();
    let changes = classify_change(&old, &new);
    assert!(changes.is_empty(), "no changes expected; got: {changes:?}");
}

// --- N19b zoom logic -------------------------------------------------------
//
// The zoom functions are called through their full paths (`nora::config::
// zoom_*`) so the test names below can match the brief's names without
// colliding with the imported function names.

/// The initial zoom level is 100%.
#[test]
fn zoom_initial() {
    assert_eq!(ZOOM_DEFAULT, 1.0);
    assert_eq!(nora::config::zoom_reset(), ZOOM_DEFAULT);
}

/// Zooming in adds one 10% step.
#[test]
fn zoom_in() {
    assert_eq!(nora::config::zoom_in(1.0), 1.1);
    assert_eq!(nora::config::zoom_in(1.1), 1.2);
}

/// Zooming out subtracts one 10% step.
#[test]
fn zoom_out() {
    assert_eq!(nora::config::zoom_out(1.0), 0.9);
    assert_eq!(nora::config::zoom_out(0.9), 0.8);
}

/// Reset returns the zoom to 100%.
#[test]
fn zoom_reset() {
    assert_eq!(nora::config::zoom_reset(), 1.0);
}

/// Zoom in caps at 4.0x.
#[test]
fn zoom_max() {
    assert_eq!(nora::config::zoom_in(4.0), ZOOM_MAX);
    // Repeated steps from below land exactly on the cap.
    let mut z = ZOOM_DEFAULT;
    for _ in 0..100 {
        z = nora::config::zoom_in(z);
    }
    assert_eq!(z, ZOOM_MAX);
}

/// Zoom out caps at 0.5x.
#[test]
fn zoom_min() {
    assert_eq!(nora::config::zoom_out(0.5), ZOOM_MIN);
    // Repeated steps from above land exactly on the floor.
    let mut z = ZOOM_DEFAULT;
    for _ in 0..100 {
        z = nora::config::zoom_out(z);
    }
    assert_eq!(z, ZOOM_MIN);
}

/// M4: the `[colors]`, `[window]`, `[cursor]`, `[scrolling]` and
/// `[[keyboard.bindings]]` sections parse, the new sections take
/// precedence over the legacy ones, and invalid values are reported with
/// their key path.
#[test]
fn schema_v2_parses_and_overrides_legacy_sections() {
    let text = r##"
[terminal]
history = 500

[scrolling]
history = 20000
multiplier = 5

[window]
columns = 100
lines = 30

[cursor]
style = "beam"

[colors]
background = "#101010"
selection_background = "#224466"
[colors.normal]
red = "#aa0000"

[[keyboard.bindings]]
keys = "ctrl+shift+c"
action = "none"

[[keyboard.bindings]]
keys = "alt+t"
action = "new_tab"
"##;
    let c = nora::config::parse_config(text).expect("valid v2 config");
    assert_eq!(c.history(), 20_000, "[scrolling] beats [terminal]");
    assert_eq!(c.scroll_multiplier(), 5);
    assert_eq!(c.window_cells(), (100, 30));
    assert_eq!(c.cursor.style, nora::config::CursorShape::Beam);
    assert_eq!(c.colors.normal.red.as_deref(), Some("#aa0000"));
    assert_eq!(c.binding_entries().len(), 2);
}

#[test]
fn schema_v2_validation_names_the_key() {
    let text = r##"
[colors.bright]
blue = "blue"

[[keyboard.bindings]]
keys = "ctrl+nope"
action = "copy"

[window]
columns = 1
"##;
    let err = nora::config::parse_config(text).expect_err("invalid config");
    let message = err.to_string();
    for needle in ["colors.bright.blue", "unknown key 'nope'", "window.columns"] {
        assert!(message.contains(needle), "missing {needle:?} in: {message}");
    }
}
