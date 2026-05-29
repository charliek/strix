use strix::ratatui::style::Color;
use strix::ui::theme::Theme;

#[test]
fn presets_resolve_by_name_with_aliases() {
    assert!(Theme::preset("tokyo-night").is_some());
    assert!(Theme::preset("tokyonight").is_some());
    assert!(Theme::preset("Catppuccin").is_some());
    assert!(Theme::preset("gruvbox").is_some());
    assert!(Theme::preset("light").is_some());
    assert!(Theme::preset("does-not-exist").is_none());
}

#[test]
fn default_theme_is_tokyo_night() {
    assert_eq!(Theme::default().syntax_theme, "base16-ocean.dark");
}

#[test]
fn custom_theme_overrides_a_base_preset() {
    let dir = tempfile::tempdir().unwrap();
    let themes = dir.path().join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    std::fs::write(
        themes.join("mine.toml"),
        "base = \"gruvbox\"\nsyntax = \"InspiredGitHub\"\n[colors]\nadd = \"#00ff00\"\n",
    )
    .unwrap();

    let theme = Theme::load("mine", Some(dir.path()));
    assert_eq!(theme.syntax_theme, "InspiredGitHub", "syntax overridden");
    assert_eq!(theme.add, Color::Rgb(0, 255, 0), "colour overridden");
    // A colour not listed keeps the gruvbox base value.
    assert_eq!(theme.bg, Color::Rgb(40, 40, 40));
}

#[test]
fn unknown_theme_falls_back_to_default() {
    let theme = Theme::load("nope", None);
    assert_eq!(theme.syntax_theme, Theme::default().syntax_theme);
}
