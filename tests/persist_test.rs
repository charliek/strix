//! Config write-back (§3.6 / C5). Every test below drives `config::persist`
//! or an `App` against an injected temp dir — never the real config dir — per
//! the plan's hard requirement that `cargo test` must not touch a developer's
//! `~/.config/strix`.

mod common;

use std::fs;
use std::path::Path;

use common::{init_repo, press};
use strix::app::{App, DiffMode, Flash, FlashKind};
use strix::config::{persist, Config, Setting};

fn read_config(dir: &Path) -> String {
    fs::read_to_string(dir.join("config.toml")).unwrap()
}

/// Any `config.toml.tmp.<pid>` residue left behind in `dir`.
fn tmp_residue(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("config.toml.tmp."))
                .collect()
        })
        .unwrap_or_default()
}

// --- persist(): direct API ---

#[test]
fn persist_preserves_comments_unknown_sections_and_unrelated_formatting() {
    let dir = tempfile::tempdir().unwrap();
    let original = "\
# a top-level comment describing this file
theme = \"dark\"

# a comment right before keys
[keys]
quit = [\"q\", \"ctrl+c\"]  # inline comment on a keys entry

# an unknown section from a future version
[future]
mystery = 42
notes = \"leave me alone\"
";
    fs::write(dir.path().join("config.toml"), original).unwrap();

    persist(dir.path(), Setting::Theme("gruvbox".to_string())).unwrap();

    let after = read_config(dir.path());
    assert!(
        after.contains("theme = \"gruvbox\""),
        "new value present: {after}"
    );
    // Everything after the changed `theme` line is an untouched region —
    // assert it survives verbatim, comments and formatting included.
    let untouched_region = "\n# a comment right before keys\n[keys]\n\
quit = [\"q\", \"ctrl+c\"]  # inline comment on a keys entry\n\n\
# an unknown section from a future version\n[future]\nmystery = 42\n\
notes = \"leave me alone\"\n";
    assert!(
        after.ends_with(untouched_region),
        "unrelated region must be byte-preserved:\n{after}"
    );
    assert!(after.starts_with("# a top-level comment describing this file\n"));
    assert!(tmp_residue(dir.path()).is_empty(), "no temp-file residue");

    // Round-trip: the written file must parse back through the read path's
    // deserialization with the new value in place (key landed at top level,
    // not inside a trailing table).
    let parsed: strix::config::Config = toml::from_str(&after).unwrap();
    assert_eq!(parsed.theme.as_deref(), Some("gruvbox"));
}

#[test]
fn missing_dir_is_created() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("nested").join("config-dir");
    assert!(!dir.exists());

    persist(&dir, Setting::LineNumbers(false)).unwrap();

    assert!(dir.is_dir(), "config dir created");
    assert!(tmp_residue(&dir).is_empty());
}

#[test]
fn missing_file_is_created_with_just_the_one_key() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!dir.path().join("config.toml").exists());

    persist(dir.path(), Setting::LineNumbers(false)).unwrap();

    let contents = read_config(dir.path());
    assert_eq!(contents.trim(), "line_numbers = false");
    assert!(tmp_residue(dir.path()).is_empty());
}

#[test]
fn invalid_existing_toml_is_never_clobbered() {
    let dir = tempfile::tempdir().unwrap();
    let broken = "this is = = not valid toml\n";
    fs::write(dir.path().join("config.toml"), broken).unwrap();

    let result = persist(dir.path(), Setting::Theme("dark".to_string()));

    assert!(result.is_err(), "persist must report the parse failure");
    assert_eq!(
        read_config(dir.path()),
        broken,
        "the broken-but-recoverable file must stay byte-identical"
    );
    assert!(tmp_residue(dir.path()).is_empty(), "no temp-file residue");
}

#[test]
fn unusable_config_dir_fails_deterministically_with_no_residue() {
    // A config dir whose parent is a regular file: create_dir_all fails on
    // every platform and for every euid (0o555 tricks pass under root).
    let base = tempfile::tempdir().unwrap();
    let blocker = base.path().join("blocker");
    fs::write(&blocker, "not a directory").unwrap();
    let dir = blocker.join("strix");

    let result = persist(&dir, Setting::Theme("dark".to_string()));

    assert!(
        result.is_err(),
        "a dir that cannot exist can't accept the temp file"
    );
    assert!(tmp_residue(base.path()).is_empty());
}

// --- App wiring: t/d/n persist through the injected config dir ---

#[test]
fn pressing_d_n_t_with_an_injected_dir_persists_each_setting() {
    let repo = init_repo();
    let config_dir = tempfile::tempdir().unwrap();
    let mut app = App::new(repo.path().to_path_buf())
        .unwrap()
        .with_config_dir(Some(config_dir.path().to_path_buf()));

    press(&mut app, 'd');
    assert_eq!(app.diff_mode, DiffMode::SideBySide);
    assert!(read_config(config_dir.path()).contains("diff_mode = \"side-by-side\""));

    press(&mut app, 'n');
    assert!(!app.show_line_numbers);
    assert!(read_config(config_dir.path()).contains("line_numbers = false"));

    press(&mut app, 't');
    assert_eq!(app.theme_name, "dark");
    assert!(read_config(config_dir.path()).contains("theme = \"dark\""));

    assert!(tmp_residue(config_dir.path()).is_empty());
}

#[test]
fn pressing_m_persists_the_menu_bar_setting() {
    let repo = init_repo();
    let config_dir = tempfile::tempdir().unwrap();
    let mut app = App::new(repo.path().to_path_buf())
        .unwrap()
        .with_config_dir(Some(config_dir.path().to_path_buf()));
    assert!(app.show_menu_bar, "menu bar starts visible");

    press(&mut app, 'm');
    assert!(!app.show_menu_bar, "m hides the menu bar");
    assert!(read_config(config_dir.path()).contains("menu_bar = false"));

    press(&mut app, 'm');
    assert!(app.show_menu_bar, "m shows it again");
    assert!(read_config(config_dir.path()).contains("menu_bar = true"));

    assert!(tmp_residue(config_dir.path()).is_empty());
}

#[test]
fn pressing_d_and_n_with_no_config_dir_writes_nothing_and_flashes_nothing() {
    let repo = init_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();

    press(&mut app, 'd');
    assert_eq!(app.diff_mode, DiffMode::SideBySide, "in-app change stands");
    assert!(
        app.flash.is_none(),
        "no config dir means persistence silently no-ops, so no flash"
    );

    press(&mut app, 'n');
    assert!(!app.show_line_numbers, "in-app change stands");
    assert!(app.flash.is_none());
}

#[test]
fn pressing_t_with_no_config_dir_still_flashes_the_theme_not_a_persist_error() {
    let repo = init_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();

    press(&mut app, 't');

    // `t` always flashes the theme name it cycled to; with no config dir this
    // must be the plain cycle flash, never a "couldn't save setting" override.
    match &app.flash {
        Some(Flash {
            kind: FlashKind::Info,
            text,
        }) => assert_eq!(text, "dark"),
        other => panic!("expected the theme-name flash, got {other:?}"),
    }
}

#[test]
fn a_deterministic_persist_failure_flashes_info_but_keeps_the_in_app_change() {
    let repo = init_repo();
    let base = tempfile::tempdir().unwrap();
    let blocker = base.path().join("blocker");
    fs::write(&blocker, "not a directory").unwrap();
    let dir = blocker.join("strix");

    let mut app = App::new(repo.path().to_path_buf())
        .unwrap()
        .with_config_dir(Some(dir.clone()));

    press(&mut app, 'd');

    assert_eq!(
        app.diff_mode,
        DiffMode::SideBySide,
        "the in-app change stands even though the write failed"
    );
    match &app.flash {
        Some(Flash {
            kind: FlashKind::Info,
            text,
        }) => assert!(
            text.starts_with("couldn't save setting"),
            "unexpected flash text: {text}"
        ),
        other => panic!("expected an info flash on persist failure, got {other:?}"),
    }
}

// --- `--theme` startup override vs. a later cycle ---

#[test]
fn theme_cli_override_does_not_persist_but_a_later_cycle_does() {
    let repo = init_repo();
    let config_dir = tempfile::tempdir().unwrap();
    // Mirrors lib::run(): `config.theme = cli.theme.or(config.theme)` before
    // the app is built, i.e. a `--theme gruvbox` startup override.
    let config = Config {
        theme: Some("gruvbox".to_string()),
        ..Config::default()
    };
    let mut app = App::with_config(repo.path().to_path_buf(), &config)
        .unwrap()
        .with_config_dir(Some(config_dir.path().to_path_buf()));

    assert_eq!(app.theme_name, "gruvbox");
    assert!(
        !config_dir.path().join("config.toml").exists(),
        "a CLI theme override must never persist at startup"
    );

    press(&mut app, 't');

    let contents = read_config(config_dir.path());
    assert!(
        contents.contains(&format!("theme = \"{}\"", app.theme_name)),
        "cycling from a CLI override persists the newly chosen theme: {contents}"
    );
}
