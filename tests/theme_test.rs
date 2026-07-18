mod common;

use std::collections::HashSet;

use common::{init_repo, press, write};
use strix::app::{App, Flash, FlashKind};
use strix::config::Config;
use strix::ratatui::style::Color;
use strix::terminal::{dump_frame, render_to_buffer};
use strix::ui::syntax::syntax_for;
use strix::ui::theme::Theme;
use tempfile::TempDir;

/// Build a `themes/` dir under a fresh temp config dir, writing each `(stem,
/// body)` as `<stem>.toml`. Returns the temp dir (kept alive by the caller).
fn config_with_themes(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (stem, body) in files {
        write(dir.path(), &format!("themes/{stem}.toml"), body);
    }
    dir
}

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
    let dir = config_with_themes(&[(
        "mine",
        "base = \"gruvbox\"\nsyntax = \"InspiredGitHub\"\n[colors]\nadd = \"#00ff00\"\n",
    )]);

    let theme = Theme::resolve("mine", Some(dir.path())).1;
    assert_eq!(theme.syntax_theme, "InspiredGitHub", "syntax overridden");
    assert_eq!(theme.add, Color::Rgb(0, 255, 0), "colour overridden");
    // A colour not listed keeps the gruvbox base value.
    assert_eq!(theme.bg, Color::Rgb(40, 40, 40));
}

#[test]
fn unknown_theme_falls_back_to_default() {
    let theme = Theme::resolve("nope", None).1;
    assert_eq!(theme.syntax_theme, Theme::default().syntax_theme);
}

// --- resolve: canonical name reporting (§3.5) ---

#[test]
fn resolve_folds_aliases_to_canonical_preset_names() {
    assert_eq!(Theme::resolve("tokyonight", None).0, "tokyo-night");
    assert_eq!(Theme::resolve("mocha", None).0, "catppuccin");
    assert_eq!(Theme::resolve("gruvbox-dark", None).0, "gruvbox");
    // Case / separator spelling folds too.
    assert_eq!(Theme::resolve("Tokyo_Night", None).0, "tokyo-night");
}

#[test]
fn resolve_unknown_name_reports_tokyo_night() {
    let (name, theme) = Theme::resolve("does-not-exist", None);
    assert_eq!(name, "tokyo-night");
    assert_eq!(theme.syntax_theme, Theme::default().syntax_theme);
}

#[test]
fn resolve_valid_custom_file_reports_its_stem() {
    let dir = config_with_themes(&[("mine", "base = \"gruvbox\"\nsyntax = \"InspiredGitHub\"\n")]);
    let (name, theme) = Theme::resolve("mine", Some(dir.path()));
    assert_eq!(name, "mine", "a valid custom file resolves to its stem");
    assert_eq!(theme.syntax_theme, "InspiredGitHub");
}

#[test]
fn resolve_malformed_custom_file_reports_default_name() {
    // A file that fails to parse must never leave the reported name pointing at a
    // theme that isn't actually shown.
    let dir = config_with_themes(&[("broken", "this is = = not valid toml\n")]);
    let (name, theme) = Theme::resolve("broken", Some(dir.path()));
    assert_eq!(
        name, "tokyo-night",
        "malformed file resolves to the default name"
    );
    assert_eq!(theme.syntax_theme, Theme::default().syntax_theme);
}

// --- available: order + preset-shadowing rule (§3.5) ---

#[test]
fn available_lists_presets_first_then_sorted_user_themes() {
    let dir = config_with_themes(&[
        ("zebra", "base = \"dark\"\n"),
        ("apple", "base = \"light\"\n"),
    ]);
    let names = Theme::available(Some(dir.path()));
    let mut expected: Vec<String> = Theme::PRESETS.iter().map(|p| p.to_string()).collect();
    // User stems come after the presets, lexically sorted.
    expected.push("apple".to_string());
    expected.push("zebra".to_string());
    assert_eq!(names, expected);
}

#[test]
fn available_omits_a_stem_that_shadows_a_preset() {
    // A `dark.toml` must not add a second "dark" entry: presets win.
    let dir = config_with_themes(&[
        ("dark", "base = \"light\"\n"),
        ("custom", "base = \"dark\"\n"),
    ]);
    let names = Theme::available(Some(dir.path()));
    assert_eq!(
        names.iter().filter(|n| *n == "dark").count(),
        1,
        "no duplicate 'dark'"
    );
    let mut expected: Vec<String> = Theme::PRESETS.iter().map(|p| p.to_string()).collect();
    expected.push("custom".to_string());
    assert_eq!(names, expected);
}

#[test]
fn available_without_a_config_dir_is_the_presets() {
    let names = Theme::available(None);
    let expected: Vec<String> = Theme::PRESETS.iter().map(|p| p.to_string()).collect();
    assert_eq!(names, expected);
}

// --- cycle: order, wraparound, alias start, deleted-file fallback (§3.5) ---

#[test]
fn cycle_advances_through_presets_then_the_user_theme() {
    let dir = config_with_themes(&[("aaa", "base = \"gruvbox\"\n")]);
    // Ordered set: [tokyo-night, dark, light, catppuccin, gruvbox, aaa].
    assert_eq!(Theme::cycle("gruvbox", Some(dir.path())).0, "aaa");
}

#[test]
fn cycle_wraps_from_the_last_theme_back_to_the_first() {
    let dir = config_with_themes(&[("aaa", "base = \"gruvbox\"\n")]);
    assert_eq!(Theme::cycle("aaa", Some(dir.path())).0, "tokyo-night");
}

#[test]
fn cycle_from_a_canonicalized_alias_advances_from_the_canonical_position() {
    // Startup resolves the alias "tokyonight" → "tokyo-night" (index 0); cycling
    // from that canonical name must land on the *next* preset, not somewhere else.
    let (canonical, _) = Theme::resolve("tokyonight", None);
    assert_eq!(canonical, "tokyo-night");
    assert_eq!(Theme::cycle(&canonical, None).0, "dark");
}

#[test]
fn cycle_from_a_deleted_current_theme_restarts_at_index_zero() {
    // "mine" was the current theme but its file is gone: cycling must not get
    // stuck; it restarts the cycle at index 0.
    let dir = config_with_themes(&[("other", "base = \"dark\"\n")]);
    let (name, _) = Theme::cycle("mine", Some(dir.path()));
    assert_eq!(
        name, "tokyo-night",
        "an absent current name restarts at index 0"
    );
}

// --- App wiring: `t` cycles, clears the highlight cache, flashes (§3.5) ---

/// A status session whose selected file is Rust source, so the diff pane has
/// syntax-highlighted content to prove colour changes against.
fn rust_app() -> (TempDir, App) {
    let repo = init_repo();
    write(repo.path(), "code.rs", "fn main() {\n    let x = 5;\n}\n");
    let app = App::new(repo.path().to_path_buf()).unwrap();
    (repo, app)
}

#[test]
fn t_cycles_the_theme_and_flashes_the_canonical_name() {
    let (_repo, mut app) = rust_app();
    assert_eq!(app.theme_name, "tokyo-night");

    press(&mut app, 't'); // -> dark
    assert_eq!(app.theme_name, "dark");
    match &app.flash {
        Some(Flash {
            text,
            kind: FlashKind::Info,
        }) => assert_eq!(text, "dark"),
        other => panic!("expected an info flash naming the theme, got {other:?}"),
    }
}

#[test]
fn cycling_to_a_different_syntax_theme_changes_highlight_colors() {
    // tokyo-night and dark share `base16-ocean.dark`; light uses `InspiredGitHub`.
    // The highlight cache is keyed by line text only, so without the cache clear
    // the cycle would hand back stale colours — this proves it is cleared.
    let (_repo, mut app) = rust_app();
    let syntax = syntax_for("code.rs");
    let line = "    let x = 5;";

    let before: Vec<Color> = app
        .highlight(syntax, &app.theme.syntax_theme, line)
        .iter()
        .map(|(color, _)| *color)
        .collect();

    press(&mut app, 't'); // -> dark
    press(&mut app, 't'); // -> light (a genuinely different syntax_theme)
    assert_eq!(app.theme_name, "light");

    let after: Vec<Color> = app
        .highlight(syntax, &app.theme.syntax_theme, line)
        .iter()
        .map(|(color, _)| *color)
        .collect();

    assert_ne!(
        before, after,
        "re-highlighting after the cycle yields new colours"
    );
}

#[test]
fn cycling_changes_rendered_foreground_colors_at_the_buffer_level() {
    // dump_frame drops styles, so assert at the ratatui buffer level.
    let (_repo, mut app) = rust_app();
    let fg = |app: &App| -> HashSet<Color> {
        let buffer = render_to_buffer(app, 100, 24).unwrap();
        buffer.content().iter().map(|cell| cell.fg).collect()
    };
    let before = fg(&app);

    press(&mut app, 't'); // -> dark
    press(&mut app, 't'); // -> light

    assert_ne!(
        before,
        fg(&app),
        "the rendered frame's colours change after cycling"
    );
}

#[test]
fn cycle_theme_is_remappable_via_the_keymap() {
    let repo = init_repo();
    write(repo.path(), "code.rs", "fn main() {}\n");
    let mut keys = std::collections::HashMap::new();
    keys.insert("cycle-theme".to_string(), vec!["y".to_string()]);
    let config = Config {
        keys: Some(keys),
        ..Config::default()
    };
    let mut app = App::with_config(repo.path().to_path_buf(), &config).unwrap();

    // `t` no longer cycles once remapped; `y` does.
    press(&mut app, 't');
    assert_eq!(
        app.theme_name, "tokyo-night",
        "default chord released after remap"
    );
    press(&mut app, 'y');
    assert_eq!(
        app.theme_name, "dark",
        "the remapped chord cycles the theme"
    );
}

// --- Flash rendering: Info vs Error styling (§3.5) ---

#[test]
fn info_and_error_flashes_render_with_distinct_styling() {
    let repo = init_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();

    // Error keeps the `✗` marker + bold `del` styling.
    app.flash = Some(Flash::error("boom"));
    let error_out = dump_frame(&app, 100, 20).unwrap();
    assert!(error_out.contains("✗ boom"), "error flash keeps its marker");

    // Info renders the text plainly with no marker.
    app.flash = Some(Flash::info("light"));
    let info_out = dump_frame(&app, 100, 20).unwrap();
    assert!(info_out.contains("light"), "info flash shows its text");
    assert!(!info_out.contains('✗'), "info flash has no error marker");
}

#[test]
fn flash_foreground_color_differs_by_kind() {
    let repo = init_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    let theme_del = app.theme.del;
    let theme_fg = app.theme.fg;

    // The footer is the last row; find the coloured flash text cell there.
    let footer_fg = |app: &App, needle: char| -> Color {
        let buffer = render_to_buffer(app, 100, 20).unwrap();
        let last = buffer.area.height - 1;
        (0..buffer.area.width)
            .map(|x| buffer[(x, last)].clone())
            .find(|cell| cell.symbol() == needle.to_string())
            .unwrap_or_else(|| panic!("flash char {needle:?} not found in footer"))
            .fg
    };

    app.flash = Some(Flash::error("boom"));
    assert_eq!(
        footer_fg(&app, 'b'),
        theme_del,
        "error text uses the del colour"
    );

    app.flash = Some(Flash::info("info"));
    assert_eq!(
        footer_fg(&app, 'f'),
        theme_fg,
        "info text uses the fg colour"
    );
}
