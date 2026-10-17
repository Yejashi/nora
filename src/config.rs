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
    /// Font settings.
    #[serde(default)]
    pub font: FontConfig,
    /// Colors (overrides the legacy `[theme]`).
    #[serde(default)]
    pub colors: ColorsConfig,
    /// Initial window size.
    #[serde(default)]
    pub window: WindowConfig,
    /// Cursor shape.
    #[serde(default)]
    pub cursor: CursorConfig,
    /// Scrollback and wheel.
    #[serde(default)]
    pub scrolling: ScrollingConfig,
    /// Key bindings (override the legacy `[keys]`).
    #[serde(default)]
    pub keyboard: KeyboardConfig,
    /// Inline suggestions from the shell integration (ADR-013).
    #[serde(default)]
    pub autocomplete: AutocompleteConfig,
    /// Legacy section (`history`); `[scrolling]` takes precedence.
    #[serde(default)]
    pub terminal: TerminalConfig,
    /// Legacy section; `[colors]` takes precedence.
    #[serde(default)]
    pub theme: ThemeConfig,
    /// Legacy section; `[[keyboard.bindings]]` take precedence.
    #[serde(default)]
    pub keys: KeysConfig,
}

/// `[colors]`: `#RRGGBB` strings; unset entries keep the theme/built-in
/// value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColorsConfig {
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub cursor: Option<String>,
    pub selection_background: Option<String>,
    #[serde(default)]
    pub normal: AnsiColors,
    #[serde(default)]
    pub bright: AnsiColors,
}

/// The eight ANSI colors of `[colors.normal]` or `[colors.bright]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnsiColors {
    pub black: Option<String>,
    pub red: Option<String>,
    pub green: Option<String>,
    pub yellow: Option<String>,
    pub blue: Option<String>,
    pub magenta: Option<String>,
    pub cyan: Option<String>,
    pub white: Option<String>,
}

impl AnsiColors {
    /// The colors in ANSI order (black, red, ..., white) with their names.
    pub fn entries(&self) -> [(&'static str, &Option<String>); 8] {
        [
            ("black", &self.black),
            ("red", &self.red),
            ("green", &self.green),
            ("yellow", &self.yellow),
            ("blue", &self.blue),
            ("magenta", &self.magenta),
            ("cyan", &self.cyan),
            ("white", &self.white),
        ]
    }
}

/// `[window]`: initial size in cells.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowConfig {
    pub columns: Option<u16>,
    pub lines: Option<u16>,
}

/// Default window size in cells.
pub const DEFAULT_COLUMNS: u16 = 80;
pub const DEFAULT_LINES: u16 = 24;

/// Cursor shapes for `[cursor] style`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorShape {
    #[default]
    Block,
    Beam,
    Underline,
}

/// `[cursor]`: the shape used until an application sets one (DECSCUSR).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorConfig {
    #[serde(default)]
    pub style: CursorShape,
}

/// `[autocomplete]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutocompleteConfig {
    /// Suggest and record command lines; default true. Off, nora neither
    /// reads nor writes its history file.
    pub enabled: Option<bool>,
}

/// `[scrolling]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScrollingConfig {
    /// Scrollback lines (0..=1_000_000); default 10 000.
    pub history: Option<usize>,
    /// Lines scrolled per mouse-wheel step (1..=100); default 3.
    pub multiplier: Option<u8>,
}

/// One `[[keyboard.bindings]]` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingConfig {
    /// A chord such as `"ctrl+shift+c"`.
    pub keys: String,
    /// An action name (`copy`, `paste`, `new_tab`, ..., or `none` to free
    /// a default chord).
    pub action: String,
}

/// `[keyboard]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyboardConfig {
    #[serde(default)]
    pub bindings: Vec<BindingConfig>,
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

/// 10 000 lines, the product default.
fn default_history() -> usize {
    10_000
}

/// Font settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// 15 pt = 20 px at 96 DPI, the size nora has always rendered at.
fn default_font_size() -> f32 {
    15.0
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
    /// Effective scrollback lines: `[scrolling] history`, else the legacy
    /// `[terminal] history`.
    pub fn history(&self) -> usize {
        self.scrolling.history.unwrap_or(self.terminal.history)
    }

    /// Effective wheel lines per step.
    /// Whether inline suggestions are on (`[autocomplete] enabled`).
    pub fn autocomplete_enabled(&self) -> bool {
        self.autocomplete.enabled.unwrap_or(true)
    }

    pub fn scroll_multiplier(&self) -> u8 {
        self.scrolling.multiplier.unwrap_or(3)
    }

    /// Effective initial window size in cells (columns, lines).
    pub fn window_cells(&self) -> (u16, u16) {
        (
            self.window.columns.unwrap_or(DEFAULT_COLUMNS),
            self.window.lines.unwrap_or(DEFAULT_LINES),
        )
    }

    /// All configured key bindings, legacy `[keys]` first so that
    /// `[[keyboard.bindings]]` entries override them.
    pub fn binding_entries(&self) -> Vec<(String, String)> {
        let mut legacy: Vec<(String, String)> = self
            .keys
            .bindings
            .iter()
            .map(|(k, a)| (k.clone(), a.clone()))
            .collect();
        legacy.sort();
        legacy
            .into_iter()
            .chain(
                self.keyboard
                    .bindings
                    .iter()
                    .map(|b| (b.keys.clone(), b.action.clone())),
            )
            .collect()
    }

    /// Validate the configuration and return a list of human-readable errors.
    ///
    /// An empty list means the config is valid. This is a pure check: it does
    /// not mutate the config and does not consult the environment.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if let Some(history) = self.scrolling.history
            && history > MAX_HISTORY
        {
            errors.push(format!(
                "scrolling.history: {history} exceeds maximum of {MAX_HISTORY}"
            ));
        }
        if let Some(m) = self.scrolling.multiplier
            && !(1..=100).contains(&m)
        {
            errors.push(format!(
                "scrolling.multiplier: {m} must be between 1 and 100"
            ));
        }
        for (name, value) in [
            ("columns", self.window.columns),
            ("lines", self.window.lines),
        ] {
            if let Some(v) = value
                && !(2..=1000).contains(&v)
            {
                errors.push(format!("window.{name}: {v} must be between 2 and 1000"));
            }
        }
        let colors = &self.colors;
        for (name, value) in [
            ("colors.foreground", &colors.foreground),
            ("colors.background", &colors.background),
            ("colors.cursor", &colors.cursor),
            ("colors.selection_background", &colors.selection_background),
        ] {
            if let Some(v) = value
                && !is_valid_hex_color(v)
            {
                errors.push(format!("{name}: '{v}' is not a valid hex color (#RRGGBB)"));
            }
        }
        for (group, ansi) in [("normal", &colors.normal), ("bright", &colors.bright)] {
            for (name, value) in ansi.entries() {
                if let Some(v) = value
                    && !is_valid_hex_color(v)
                {
                    errors.push(format!(
                        "colors.{group}.{name}: '{v}' is not a valid hex color (#RRGGBB)"
                    ));
                }
            }
        }
        for (keys, action) in self.binding_entries() {
            if let Err(message) = crate::input::bindings::parse_binding(&keys, &action) {
                errors.push(format!(
                    "keyboard.bindings: '{keys}' = '{action}': {message}"
                ));
            }
        }
        // Validate history.
        if self.terminal.history > MAX_HISTORY {
            errors.push(format!(
                "terminal.history: {} exceeds maximum of {}",
                self.terminal.history, MAX_HISTORY
            ));
        }

        // Validate font size.
        // `!(a && b)` also rejects NaN, for which every comparison is false.
        if !(self.font.size > 0.0 && self.font.size <= MAX_FONT_SIZE) {
            errors.push(format!(
                "font.size: {} must be greater than 0 and at most {}",
                self.font.size, MAX_FONT_SIZE
            ));
        }

        // Validate the 16-color ANSI palette.
        if self.theme.colors.len() > 16 {
            errors.push(format!(
                "theme.colors: {} entries given; at most 16 (ANSI 0-15) are allowed",
                self.theme.colors.len()
            ));
        }
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

/// One zoom step (10%) and the zoom limits (N19b): zoom ranges from
/// [`ZOOM_MIN`] to [`ZOOM_MAX`], starting at [`ZOOM_DEFAULT`].
pub const ZOOM_STEP: f32 = 0.1;
/// Minimum zoom level (50%).
pub const ZOOM_MIN: f32 = 0.5;
/// Maximum zoom level (400%).
pub const ZOOM_MAX: f32 = 4.0;
/// The initial zoom level (100%).
pub const ZOOM_DEFAULT: f32 = 1.0;

/// Zoom in by one step, clamped to [`ZOOM_MAX`].
///
/// The result is rounded to one decimal place so repeated steps land on
/// exact tenths instead of drifting in floating point.
pub fn zoom_in(zoom: f32) -> f32 {
    let steps = (zoom + ZOOM_STEP) * 10.0;
    steps.round().min(ZOOM_MAX * 10.0) / 10.0
}

/// Zoom out by one step, clamped to [`ZOOM_MIN`].
///
/// The result is rounded to one decimal place, as in [`zoom_in`].
pub fn zoom_out(zoom: f32) -> f32 {
    let steps = (zoom - ZOOM_STEP) * 10.0;
    steps.round().max(ZOOM_MIN * 10.0) / 10.0
}

/// Reset the zoom to the default level (100%).
pub fn zoom_reset() -> f32 {
    ZOOM_DEFAULT
}

/// Classify the differences between two configs, one entry per changed field.
///
/// The result follows a fixed field order (terminal, scrolling, window,
/// font, colors, theme, cursor, keys, keyboard, autocomplete), so callers
/// get a deterministic list. An empty result means the configs are
/// identical. The kinds follow how a live reload applies them: colors,
/// bindings, the wheel multiplier, the cursor shape and autocomplete at
/// once; fonts staged; scrollback for new sessions; the window size only
/// for a new window (reported as `NewSession`).
pub fn classify_change(old: &NoraConfig, new: &NoraConfig) -> Vec<(&'static str, ConfigChange)> {
    let mut changes = Vec::new();

    // Terminal settings.
    if old.terminal.history != new.terminal.history {
        changes.push(("terminal.history", ConfigChange::NewSession));
    }
    if old.terminal.mouse != new.terminal.mouse {
        changes.push(("terminal.mouse", ConfigChange::Immediate));
    }

    // Scrolling and window size.
    if old.scrolling.history != new.scrolling.history {
        changes.push(("scrolling.history", ConfigChange::NewSession));
    }
    if old.scrolling.multiplier != new.scrolling.multiplier {
        changes.push(("scrolling.multiplier", ConfigChange::Immediate));
    }
    if old.window.columns != new.window.columns {
        changes.push(("window.columns", ConfigChange::NewSession));
    }
    if old.window.lines != new.window.lines {
        changes.push(("window.lines", ConfigChange::NewSession));
    }

    // Font settings.
    if old.font.family != new.font.family {
        changes.push(("font.family", ConfigChange::Staged));
    }
    if old.font.size != new.font.size {
        changes.push(("font.size", ConfigChange::Staged));
    }

    // Colors (`[colors]` overrides the legacy `[theme]`).
    if old.colors.foreground != new.colors.foreground {
        changes.push(("colors.foreground", ConfigChange::Immediate));
    }
    if old.colors.background != new.colors.background {
        changes.push(("colors.background", ConfigChange::Immediate));
    }
    if old.colors.cursor != new.colors.cursor {
        changes.push(("colors.cursor", ConfigChange::Immediate));
    }
    if old.colors.selection_background != new.colors.selection_background {
        changes.push(("colors.selection_background", ConfigChange::Immediate));
    }
    if old.colors.normal != new.colors.normal {
        changes.push(("colors.normal", ConfigChange::Immediate));
    }
    if old.colors.bright != new.colors.bright {
        changes.push(("colors.bright", ConfigChange::Immediate));
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

    // Cursor shape (until an application sets one).
    if old.cursor.style != new.cursor.style {
        changes.push(("cursor.style", ConfigChange::Immediate));
    }

    // Key bindings.
    if old.keys.bindings != new.keys.bindings {
        changes.push(("keys.bindings", ConfigChange::Immediate));
    }
    if old.keyboard.bindings != new.keyboard.bindings {
        changes.push(("keyboard.bindings", ConfigChange::Immediate));
    }

    // Suggestions (the pump loads or drops the history at once).
    if old.autocomplete.enabled != new.autocomplete.enabled {
        changes.push(("autocomplete.enabled", ConfigChange::Immediate));
    }

    changes
}
