//! User configuration loaded from `config.toml` in the config directory
//! (`$XDG_CONFIG_HOME/strix` or `~/.config/strix`). Every field is optional;
//! missing or invalid config falls back to defaults.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::app::DiffMode;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Theme name: a built-in preset or a file in `themes/`.
    pub theme: Option<String>,
    /// Default diff mode: `unified` or `side-by-side`.
    pub diff_mode: Option<String>,
    /// Keybinding overrides: action name → list of key chords.
    pub keys: Option<HashMap<String, Vec<String>>>,
    /// Auto-refresh on filesystem / git changes (a background watcher). On by
    /// default; set `false` to disable the watcher and refresh only with `r`.
    pub auto_refresh: Option<bool>,
    /// Whether the diff pane shows line-number gutters. On by default; set
    /// `false` to start with them hidden (toggle at runtime with `n`).
    pub line_numbers: Option<bool>,
}

impl Config {
    pub fn diff_mode(&self) -> DiffMode {
        match self.diff_mode.as_deref().map(str::trim) {
            Some("side-by-side") | Some("sidebyside") | Some("split") => DiffMode::SideBySide,
            _ => DiffMode::Unified,
        }
    }

    pub fn auto_refresh(&self) -> bool {
        self.auto_refresh.unwrap_or(true)
    }

    pub fn line_numbers(&self) -> bool {
        self.line_numbers.unwrap_or(true)
    }
}

/// The config directory: `$XDG_CONFIG_HOME/strix`, else `~/.config/strix`.
pub fn config_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("strix"));
        }
    }
    directories::BaseDirs::new().map(|base| base.home_dir().join(".config/strix"))
}

/// Load `config.toml`. A missing file is normal (defaults); an invalid one logs
/// a warning and falls back to defaults.
pub fn load() -> Config {
    let Some(path) = config_dir().map(|dir| dir.join("config.toml")) else {
        return Config::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
            tracing::warn!("invalid config ({err}); using defaults");
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}
