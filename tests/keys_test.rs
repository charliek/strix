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
    assert_eq!(keymap.action(key('y')), Some(Action::ToggleHistory));
    assert_eq!(keymap.action(key('1')), Some(Action::ShowStatus));
    assert_eq!(keymap.action(key('2')), Some(Action::ShowHistory));
    assert_eq!(keymap.action(ctrl('d')), Some(Action::HalfPageDown));
    assert_eq!(keymap.action(key('G')), Some(Action::Bottom));
    assert_eq!(keymap.action(key(' ')), Some(Action::ToggleStage));
    assert_eq!(keymap.action(key('z')), None);
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
fn invalid_overrides_are_ignored() {
    let mut overrides = HashMap::new();
    overrides.insert("nonsense-action".to_string(), vec!["a".to_string()]);
    overrides.insert("quit".to_string(), vec!["not a key".to_string()]);
    let keymap = Keymap::from_config(Some(&overrides));
    // The bogus action is dropped; quit's default chords were cleared then the
    // invalid chord ignored, so 'q' no longer quits but ctrl-c is hardcoded.
    assert_eq!(keymap.action(key('a')), None);
}
