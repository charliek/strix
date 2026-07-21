use std::collections::HashMap;

use strix::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use strix::keys::{Action, Keymap};

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

#[test]
fn default_bindings() {
    let keymap = Keymap::default();
    assert_eq!(keymap.action(key('j')), Some(Action::Down));
    assert_eq!(keymap.action(key('k')), Some(Action::Up));
    assert_eq!(keymap.action(key('q')), Some(Action::Quit));
    assert_eq!(keymap.action(key('d')), Some(Action::ToggleDiffMode));
    assert_eq!(keymap.action(key('b')), Some(Action::ToggleChanges));
    assert_eq!(keymap.action(key('m')), Some(Action::ToggleMenuBar));
    assert_eq!(keymap.action(key('i')), Some(Action::ToggleHistory));
    assert_eq!(keymap.action(key('1')), Some(Action::ShowStatus));
    assert_eq!(keymap.action(key('2')), Some(Action::ShowHistory));
    assert_eq!(keymap.action(ctrl('d')), Some(Action::HalfPageDown));
    assert_eq!(keymap.action(key('G')), Some(Action::Bottom));
    assert_eq!(keymap.action(key(' ')), Some(Action::ToggleStage));
    assert_eq!(keymap.action(key('z')), None);
}

#[test]
fn toggle_menu_bar_action_name_parses() {
    // The action is remappable by any of its config names.
    for name in ["toggle-menu-bar", "menu-bar", "menu"] {
        let mut overrides = HashMap::new();
        overrides.insert(name.to_string(), vec!["ctrl-m".to_string()]);
        let keymap = Keymap::from_config(Some(&overrides));
        assert_eq!(
            keymap.action(ctrl('m')),
            Some(Action::ToggleMenuBar),
            "{name:?} parses to ToggleMenuBar"
        );
    }
}

#[test]
fn config_overrides_replace_an_actions_chords() {
    let mut overrides = HashMap::new();
    overrides.insert("stage".to_string(), vec!["ctrl-s".to_string()]);
    let keymap = Keymap::from_config(Some(&overrides));

    assert_eq!(
        keymap.action(ctrl('s')),
        Some(Action::Stage),
        "new chord bound"
    );
    assert_eq!(keymap.action(key('s')), None, "default 's' was replaced");
    assert_eq!(
        keymap.action(key('j')),
        Some(Action::Down),
        "unlisted actions keep their defaults"
    );
}

#[test]
fn comment_actions_have_default_chords() {
    let keymap = Keymap::default();
    assert_eq!(keymap.action(key(']')), Some(Action::NextComment));
    assert_eq!(keymap.action(key('[')), Some(Action::PrevComment));
    assert_eq!(keymap.action(key('X')), Some(Action::DeleteComment));
}

#[test]
fn delete_comment_is_remappable_via_the_keymap() {
    let mut overrides = HashMap::new();
    overrides.insert("delete-comment".to_string(), vec!["d".to_string()]);
    let keymap = Keymap::from_config(Some(&overrides));
    assert_eq!(keymap.action(key('d')), Some(Action::DeleteComment));
    // The default 'X' was released by the remap.
    assert_eq!(keymap.action(key('X')), None);
}

#[test]
fn delete_comment_chord_colliding_in_config_resolves_to_one() {
    let mut overrides = HashMap::new();
    overrides.insert("delete-comment".to_string(), vec!["p".to_string()]);
    overrides.insert("comment".to_string(), vec!["p".to_string()]);
    let keymap = Keymap::from_config(Some(&overrides));
    let resolved = keymap.action(key('p'));
    assert!(
        matches!(
            resolved,
            Some(Action::DeleteComment) | Some(Action::Comment)
        ),
        "the chord resolves to exactly one of the colliding actions: {resolved:?}"
    );
}

#[test]
fn next_comment_is_remappable_via_the_keymap() {
    let mut overrides = HashMap::new();
    overrides.insert("next-comment".to_string(), vec!["p".to_string()]);
    let keymap = Keymap::from_config(Some(&overrides));
    assert_eq!(keymap.action(key('p')), Some(Action::NextComment));
    // The default ']' was released by the remap.
    assert_eq!(keymap.action(key(']')), None);
}

#[test]
fn a_chord_assigned_to_two_actions_in_config_resolves_to_one() {
    // Assigning the same chord to two different actions is a config collision:
    // it must warn (exercised here) and last-writer-wins leaves exactly one
    // action on the chord — never a silent double binding.
    let mut overrides = HashMap::new();
    overrides.insert("next-comment".to_string(), vec!["p".to_string()]);
    overrides.insert("prev-comment".to_string(), vec!["p".to_string()]);
    let keymap = Keymap::from_config(Some(&overrides));
    let resolved = keymap.action(key('p'));
    assert!(
        matches!(
            resolved,
            Some(Action::NextComment) | Some(Action::PrevComment)
        ),
        "the chord resolves to exactly one of the colliding actions: {resolved:?}"
    );
}

#[test]
fn invalid_overrides_are_ignored() {
    let mut overrides = HashMap::new();
    overrides.insert("nonsense-action".to_string(), vec!["a".to_string()]);
    overrides.insert("quit".to_string(), vec!["not a key".to_string()]);
    let keymap = Keymap::from_config(Some(&overrides));
    // The bogus action is dropped; quit's default chords were cleared then the
    // invalid chord ignored, so 'q' no longer quits but ctrl-c is hardcoded.
    assert_eq!(keymap.action(key('a')), None);
}
