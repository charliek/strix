//! User configuration loaded from `config.toml` in the config directory
//! (`$XDG_CONFIG_HOME/strix` or `~/.config/strix`). Every field is optional;
//! missing or invalid config falls back to defaults.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
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
    /// Whether the top menu bar (the `View`/`Theme` labels in the header) is
    /// shown. On by default; set `false` to start with it hidden (toggle at
    /// runtime with `m`).
    pub menu_bar: Option<bool>,
    /// Whether the diff pane hard-wraps long lines at the pane width. Off by
    /// default (long lines are truncated); set `true` to start with wrapping on
    /// (toggle at runtime with `w`).
    pub wrap_lines: Option<bool>,
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

    pub fn menu_bar(&self) -> bool {
        self.menu_bar.unwrap_or(true)
    }

    pub fn wrap_lines(&self) -> bool {
        self.wrap_lines.unwrap_or(false)
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

/// A single scalar written back to `config.toml` by an explicit in-app action
/// (`t`/`d`/`n`/`m`/`w`). See [`persist`].
pub enum Setting {
    Theme(String),
    DiffMode(DiffMode),
    LineNumbers(bool),
    MenuBar(bool),
    WrapLines(bool),
}

/// Persist one setting into `config_dir/config.toml`, preserving everything
/// else in the file — comments, unrelated keys/tables, formatting — via
/// `toml_edit`. The config dir is created first if missing.
///
/// If `config.toml` exists but fails to parse, this returns an error
/// *without writing anything*: a user's broken-but-recoverable file must stay
/// byte-for-byte untouched. Otherwise the write is atomic — a sibling temp
/// file in the same directory is written and flushed, then renamed over
/// `config.toml`; the temp file is removed on any failure path.
pub fn persist(config_dir: &Path, setting: Setting) -> anyhow::Result<()> {
    std::fs::create_dir_all(config_dir)
        .with_context(|| format!("creating config dir {}", config_dir.display()))?;

    let path = config_dir.join("config.toml");
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(text) => text
            .parse::<toml_edit::DocumentMut>()
            .with_context(|| format!("parsing {}", path.display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };

    match setting {
        Setting::Theme(name) => doc["theme"] = toml_edit::value(name),
        Setting::DiffMode(mode) => {
            let value = match mode {
                DiffMode::Unified => "unified",
                DiffMode::SideBySide => "side-by-side",
            };
            doc["diff_mode"] = toml_edit::value(value);
        }
        Setting::LineNumbers(enabled) => doc["line_numbers"] = toml_edit::value(enabled),
        Setting::MenuBar(v) => doc["menu_bar"] = toml_edit::value(v),
        Setting::WrapLines(v) => doc["wrap_lines"] = toml_edit::value(v),
    }

    write_atomic(config_dir, &path, &doc.to_string())
}

/// Write `contents` to a sibling temp file (`<filename>.tmp.<pid>`, so
/// concurrent strix instances don't collide) and atomically rename it over
/// `path`. The temp file is removed if any step fails. Shared with the comments
/// store (`src/comments.rs`), which reuses this exact durability recipe.
pub(crate) fn write_atomic(dir: &Path, path: &Path, contents: &str) -> anyhow::Result<()> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".to_string());
    let tmp_path = dir.join(format!("{file_name}.tmp.{}", std::process::id()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::File::create_new(&tmp_path)
            .with_context(|| format!("creating {}", tmp_path.display()))?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("writing {}", tmp_path.display()))?;
        // flush() alone is not a durability barrier: without sync_all a crash
        // after the rename could leave an empty config where a full one was.
        file.sync_all()
            .with_context(|| format!("syncing {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, path)
            .with_context(|| format!("renaming {} to {}", tmp_path.display(), path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}
