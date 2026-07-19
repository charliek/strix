//! `strix skill path` — materialize the bundled agent skill and print where it
//! landed (plan §3.5). The skill (`skills/strix-review/SKILL.md`) teaches an
//! agent the review-comment loop; agents (and their plugin loaders) find it on
//! disk via this command.

use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::BaseDirs;
use serde_json::json;

use crate::cli::SkillAction;
use crate::config;

/// The bundled skill, embedded from the in-repo file at build time — the single
/// source of truth. `strix skill path` writes this exact copy to disk, so the
/// materialized skill can never drift from the binary that ships it.
const SKILL_MD: &str = include_str!("../skills/strix-review/SKILL.md");

/// Dispatch a `strix skill` action.
pub fn run(action: &SkillAction) -> Result<()> {
    match action {
        SkillAction::Path { json } => path(*json),
    }
}

/// The base data directory: `$STRIX_DATA_DIR` when set to a non-empty value
/// (keeps tests hermetic and lets callers relocate it), else the platform data
/// dir from the `directories` crate — mirroring how `config::config_dir`
/// resolves the config directory. An empty `$STRIX_DATA_DIR` counts as unset,
/// the conventional treatment. A relative value is resolved against the current
/// directory so the printed path always honours the absolute-path contract.
fn data_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("STRIX_DATA_DIR") {
        if !dir.is_empty() {
            let dir = PathBuf::from(dir);
            let dir = if dir.is_absolute() {
                dir
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(&dir))
                    .unwrap_or(dir)
            };
            return Some(dir);
        }
    }
    BaseDirs::new().map(|base| base.data_dir().to_path_buf())
}

/// Materialize the skill under `<data_dir>/strix/skills/strix-review/` and print
/// its absolute path. Written atomically on every invocation so a stale on-disk
/// copy is always refreshed to match this binary.
fn path(json_out: bool) -> Result<()> {
    let base = data_dir().context(
        "could not determine a data directory; set STRIX_DATA_DIR to choose one explicitly",
    )?;
    let skill_dir = base.join("strix").join("skills").join("strix-review");
    let skill_path = skill_dir.join("SKILL.md");

    std::fs::create_dir_all(&skill_dir)
        .with_context(|| format!("creating skill dir {}", skill_dir.display()))?;
    config::write_atomic(&skill_dir, &skill_path, SKILL_MD)?;

    if json_out {
        let value = json!({ "path": skill_path.to_string_lossy() });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("{}", skill_path.display());
    }
    Ok(())
}
