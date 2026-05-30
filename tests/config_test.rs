mod common;

use common::init_repo;
use strix::app::{App, DiffMode};
use strix::config::Config;

#[test]
fn diff_mode_parses_from_config() {
    let mut config = Config::default();
    assert_eq!(config.diff_mode(), DiffMode::Unified);
    config.diff_mode = Some("side-by-side".to_string());
    assert_eq!(config.diff_mode(), DiffMode::SideBySide);
    config.diff_mode = Some("unified".to_string());
    assert_eq!(config.diff_mode(), DiffMode::Unified);
}

#[test]
fn with_config_applies_theme_and_diff_mode() {
    let repo = init_repo();
    let config = Config {
        theme: Some("gruvbox".to_string()),
        diff_mode: Some("side-by-side".to_string()),
        keys: None,
        auto_refresh: None,
    };
    let app = App::with_config(repo.path().to_path_buf(), &config).unwrap();
    assert_eq!(app.theme.syntax_theme, "base16-eighties.dark");
    assert_eq!(app.diff_mode, DiffMode::SideBySide);
}
