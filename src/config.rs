//! Validated TOML configuration (N18a).
//!
//! This module owns the configuration *types*, their *defaults*, *validation*,
//! and *discovery/loading* from an explicit path or the XDG user directories.
//! CLI precedence (N18b) layers on top of the loaded config and is not part of
//! this module.
//!
//! The public surface is deliberately small and engine-free:
//! - [`NoraConfig`] and its section types (all `deny_unknown_fields`).
//! - [`NoraConfig::validate`] returning human-readable error strings.
//! - [`load_config`] / [`load_config_with_candidates`] returning a
//!   [`LoadedConfig`] (config + the path it came from, if any).
//! - [`ConfigError`] for I/O, parse and validation failures.
//! - [`ConfigState`] / [`classify_change`] for transactional reload and
//!   change classification (N19a).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

/// Upper bound for `terminal.history` (lines).
pub const MAX_HISTORY: usize = 1_000_000;
/// Upper bound for `font.size` (points).
pub const MAX_FONT_SIZE: f32 = 1000.0;

/// The complete nora configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoraConfig {
    /// Terminal settings.
    #[serde(default)]
    pub terminal: TerminalConfig,
    /// Font settings.
    #[serde(default)]
    pub font: FontConfig,
    /// Theme/color settings.
    #[serde(default)]
    pub theme: ThemeConfig,
    /// Key bindings.
    #[serde(default)]
    pub keys: KeysConfig,
}

/// Terminal behavior settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalConfig {
    /// Scrollback lines (0 to [`MAX_HISTORY`]).
    #[serde(default = "default_history")]
    pub history: usize,
    /// Whether to enable mouse support by default.
    #[serde(default)]
    pub mouse: bool,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            history: default_history(),
            mouse: false,
        }
    }
}

fn default_history() -> usize {
    1000
}

/// Font settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontConfig {
    /// Font family name.
    #[serde(default = "default_font_family")]
    pub family: String,
    /// Font size in points.
    #[serde(default = "default_font_size")]
    pub size: f32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: default_font_family(),
            size: default_font_size(),
        }
    }
}

fn default_font_family() -> String {
    "monospace".to_string()
}

fn default_font_size() -> f32 {
    12.0
}

/// Theme/color settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    /// Background color (hex: #RRGGBB).
    #[serde(default = "default_bg")]
    pub background: String,
    /// Foreground color (hex: #RRGGBB).
    #[serde(default = "default_fg")]
    pub foreground: String,
    /// Cursor color (hex: #RRGGBB).
    #[serde(default = "default_cursor")]
    pub cursor: String,
    /// ANSI colors (16 colors, hex: #RRGGBB).
    #[serde(default)]
    pub colors: Vec<String>,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            background: default_bg(),
            foreground: default_fg(),
            cursor: default_cursor(),
            colors: Vec::new(),
        }
    }
}

fn default_bg() -> String {
    "#000000".to_string()
}

fn default_fg() -> String {
    "#ffffff".to_string()
}

fn default_cursor() -> String {
    "#ffffff".to_string()
}

/// Key bindings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeysConfig {
    /// Custom key bindings (key sequence → action).
    #[serde(default)]
    pub bindings: HashMap<String, String>,
}

impl NoraConfig {
    /// Validate the configuration and return a list of human-readable errors.
    ///
    /// An empty list means the config is valid. This is a pure check: it does
    /// not mutate the config and does not consult the environment.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Validate history.
        if self.terminal.history > MAX_HISTORY {
            errors.push(format!(
                "terminal.history: {} exceeds maximum of {}",
                self.terminal.history, MAX_HISTORY
            ));
        }

        // Validate font size.
        if self.font.size <= 0.0 || self.font.size > MAX_FONT_SIZE {
            errors.push(format!(
                "font.size: {} must be between 0 and {}",
                self.font.size, MAX_FONT_SIZE
            ));
        }

        // Validate the 16-color ANSI palette.
        for (i, color) in self.theme.colors.iter().enumerate() {
            if !is_valid_hex_color(color) {
                errors.push(format!(
                    "theme.colors[{}]: '{}' is not a valid hex color (#RRGGBB)",
                    i, color
                ));
            }
        }

        // Validate background/foreground/cursor.
        if !is_valid_hex_color(&self.theme.background) {
            errors.push(format!(
                "theme.background: '{}' is not a valid hex color (#RRGGBB)",
                self.theme.background
            ));
        }

        if !is_valid_hex_color(&self.theme.foreground) {
            errors.push(format!(
                "theme.foreground: '{}' is not a valid hex color (#RRGGBB)",
                self.theme.foreground
            ));
        }

        if !is_valid_hex_color(&self.theme.cursor) {
            errors.push(format!(
                "theme.cursor: '{}' is not a valid hex color (#RRGGBB)",
                self.theme.cursor
            ));
        }

        errors
    }
}

/// Check if a string is a valid hex color (`#RRGGBB`, case-insensitive hex).
fn is_valid_hex_color(s: &str) -> bool {
    if !s.starts_with('#') || s.len() != 7 {
        return false;
    }
    s.chars().skip(1).all(|c| c.is_ascii_hexdigit())
}

/// The result of loading a configuration.
#[derive(Debug)]
pub struct LoadedConfig {
    /// The parsed configuration.
    pub config: NoraConfig,
    /// The path the config was loaded from (if any). `None` when the built-in
    /// defaults were used because no config file was found.
    pub path: Option<PathBuf>,
}

/// Errors that can occur when loading configuration.
#[derive(Debug)]
pub enum ConfigError {
    /// I/O error reading the config file.
    Io {
        /// The path that could not be read.
        path: PathBuf,
        /// The underlying I/O error, as a string.
        error: String,
    },
    /// Parse error (invalid TOML).
    Parse {
        /// The path that failed to parse.
        path: PathBuf,
        /// The parser error, as a string.
        error: String,
    },
    /// Validation error (structurally valid TOML, but invalid values).
    Validation {
        /// The path that failed validation.
        path: PathBuf,
        /// The individual validation errors.
        errors: Vec<String>,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io { path, error } => {
                write!(
                    f,
                    "Failed to read config file {}: {}",
                    path.display(),
                    error
                )
            }
            ConfigError::Parse { path, error } => {
                write!(
                    f,
                    "Failed to parse config file {}: {}",
                    path.display(),
                    error
                )
            }
            ConfigError::Validation { path, errors } => {
                write!(f, "Config file {} failed validation:", path.display())?;
                for error in errors {
                    write!(f, "\n  - {}", error)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Load the configuration from the specified path or XDG locations.
///
/// Search order:
/// 1. Explicit path (if provided) — loaded directly; a missing file is an error.
/// 2. `$XDG_CONFIG_HOME/nora/config.toml` (if the variable is set and the file exists).
/// 3. `$HOME/.config/nora/config.toml` (if the file exists).
/// 4. Built-in defaults (`path: None`).
///
/// The explicit path short-circuits before any environment lookup, so it is
/// safe to call with a path regardless of the ambient environment.
pub fn load_config(explicit_path: Option<&str>) -> Result<LoadedConfig, ConfigError> {
    if let Some(path) = explicit_path {
        return load_from_path(PathBuf::from(path));
    }

    let candidates = xdg_candidates();
    load_config_with_candidates(None, &candidates)
}

/// Load configuration given an explicit path and an ordered list of XDG
/// candidate paths.
///
/// This is the environment-free core of [`load_config`]: it is exposed so
/// callers (and tests) can drive discovery with deterministic paths instead of
/// the process environment. An explicit path is loaded directly; otherwise the
/// first *existing* candidate is loaded; otherwise the built-in defaults are
/// returned with `path: None`.
pub fn load_config_with_candidates(
    explicit_path: Option<&str>,
    candidates: &[PathBuf],
) -> Result<LoadedConfig, ConfigError> {
    if let Some(path) = explicit_path {
        return load_from_path(PathBuf::from(path));
    }

    for path in candidates {
        if path.exists() {
            return load_from_path(path.clone());
        }
    }

    Ok(LoadedConfig {
        config: NoraConfig::default(),
        path: None,
    })
}

/// Build the ordered XDG candidate paths from the environment:
/// `$XDG_CONFIG_HOME/nora/config.toml` (if set) then
/// `$HOME/.config/nora/config.toml` (if `HOME` is set).
fn xdg_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(xdg_home) = std::env::var_os("XDG_CONFIG_HOME") {
        candidates.push(PathBuf::from(xdg_home).join("nora").join("config.toml"));
    }

    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join(".config")
                .join("nora")
                .join("config.toml"),
        );
    }

    candidates
}

/// Load and validate a config from a specific path.
fn load_from_path(path: PathBuf) -> Result<LoadedConfig, ConfigError> {
    let content = std::fs::read_to_string(&path).map_err(|e| ConfigError::Io {
        path: path.clone(),
        error: e.to_string(),
    })?;

    let config: NoraConfig = toml::from_str(&content).map_err(|e| ConfigError::Parse {
        path: path.clone(),
        error: e.to_string(),
    })?;

    // Validate.
    let errors = config.validate();
    if !errors.is_empty() {
        return Err(ConfigError::Validation {
            path: path.clone(),
            errors,
        });
    }

    Ok(LoadedConfig {
        config,
        path: Some(path),
    })
}

/// Convenience for tests and callers: parse TOML from a string and validate it,
/// without touching the filesystem.
pub fn parse_config(toml_text: &str) -> Result<NoraConfig, ConfigError> {
    let config: NoraConfig = toml::from_str(toml_text).map_err(|e| ConfigError::Parse {
        path: PathBuf::from("<string>"),
        error: e.to_string(),
    })?;

    let errors = config.validate();
    if !errors.is_empty() {
        return Err(ConfigError::Validation {
            path: PathBuf::from("<string>"),
            errors,
        });
    }

    Ok(config)
}

/// The state of the configuration, supporting transactional reload (N19a).
///
/// `current` is the active configuration, `path` is where it was loaded from
/// (if any), and `epoch` increments on every successful reload so callers can
/// tell which config version is active.
#[derive(Debug, Clone)]
pub struct ConfigState {
    /// The currently active configuration.
    pub current: NoraConfig,
    /// The path the config was loaded from (if any).
    pub path: Option<PathBuf>,
    /// The epoch of the current config (increments on each reload).
    pub epoch: u64,
}

impl ConfigState {
    /// Create a new [`ConfigState`] from a loaded config (epoch 0).
    pub fn new(loaded: LoadedConfig) -> Self {
        Self {
            current: loaded.config,
            path: loaded.path,
            epoch: 0,
        }
    }

    /// Attempt to reload the config from disk, transactionally.
    ///
    /// On success the returned [`ConfigState`] carries the new config and an
    /// incremented [`epoch`](ConfigState::epoch); the old state is left
    /// untouched. On failure (I/O, parse or validation) the error is returned
    /// and the caller keeps the last valid state.
    ///
    /// The reload re-reads from the same source the current state came from:
    /// the explicit path when there is one, otherwise XDG discovery (which
    /// falls back to the built-in defaults with `path: None`).
    pub fn reload(&self) -> Result<ConfigState, ConfigError> {
        let loaded = match &self.path {
            Some(path) => load_from_path(path.clone()),
            None => load_config(None),
        }?;

        Ok(ConfigState {
            current: loaded.config,
            path: loaded.path,
            epoch: self.epoch + 1,
        })
    }
}

/// The type of a config change, by how it is applied (N19a).
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigChange {
    /// Immediate UI change (colors, keys) — applies without a restart.
    Immediate,
    /// Staged font/geometry change — applies on the next frame.
    Staged,
    /// New-session launch setting — applies to new sessions only.
    NewSession,
}

/// Classify the differences between two configs, one entry per changed field.
///
/// The result follows a fixed field order (terminal, font, theme, keys), so
/// callers get a deterministic list. An empty result means the configs are
/// identical.
pub fn classify_change(old: &NoraConfig, new: &NoraConfig) -> Vec<(&'static str, ConfigChange)> {
    let mut changes = Vec::new();

    // Terminal settings.
    if old.terminal.history != new.terminal.history {
        changes.push(("terminal.history", ConfigChange::NewSession));
    }
    if old.terminal.mouse != new.terminal.mouse {
        changes.push(("terminal.mouse", ConfigChange::Immediate));
    }

    // Font settings.
    if old.font.family != new.font.family {
        changes.push(("font.family", ConfigChange::Staged));
    }
    if old.font.size != new.font.size {
        changes.push(("font.size", ConfigChange::Staged));
    }

    // Theme settings.
    if old.theme.background != new.theme.background {
        changes.push(("theme.background", ConfigChange::Immediate));
    }
    if old.theme.foreground != new.theme.foreground {
        changes.push(("theme.foreground", ConfigChange::Immediate));
    }
    if old.theme.cursor != new.theme.cursor {
        changes.push(("theme.cursor", ConfigChange::Immediate));
    }
    if old.theme.colors != new.theme.colors {
        changes.push(("theme.colors", ConfigChange::Immediate));
    }

    // Key bindings.
    if old.keys.bindings != new.keys.bindings {
        changes.push(("keys.bindings", ConfigChange::Immediate));
    }

    changes
}
