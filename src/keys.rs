//! Configurable keybindings: a [`Keymap`] resolves a key event to an [`Action`],
//! which the app then interprets in context (e.g. `Down` moves the file cursor
//! in the staging pane but scrolls the diff pane). Defaults are overridable from
//! the config file.

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::util::normalize;

/// A semantic action a key triggers. Context (focused pane) decides the effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    Help,
    Refresh,
    SwitchPane,
    ToggleDiffMode,
    ToggleLineNumbers,
    CycleTheme,
    ToggleChanges,
    ToggleHistory,
    ShowStatus,
    ShowHistory,
    Down,
    Up,
    Top,
    Bottom,
    HalfPageDown,
    HalfPageUp,
    FocusStaging,
    FocusDiff,
    ToggleStage,
    Stage,
    Unstage,
    Discard,
    /// Add a comment on the code row under the cursor, or edit the human comment
    /// under it (review view, diff focus).
    Comment,
    /// Jump to the next / previous review comment on a listed file (review view).
    NextComment,
    PrevComment,
    /// Delete the comment/orphan under the diff-pane cursor (Status and Review
    /// alike). Replaces the milestone-6 overload of `x`/`Discard`, which no
    /// longer deletes comments in either view.
    DeleteComment,
}

/// A key plus modifiers, normalised so Shift is folded into the character.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Chord {
    code: KeyCode,
    mods: KeyModifiers,
}

impl Chord {
    fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        // For characters the case already encodes Shift, so drop it to match
        // however the terminal reports it.
        let mods = match code {
            KeyCode::Char(_) => mods.difference(KeyModifiers::SHIFT),
            _ => mods,
        };
        Chord { code, mods }
    }
}

pub struct Keymap {
    bindings: HashMap<Chord, Action>,
}

impl Keymap {
    /// Build the keymap from defaults, applying any config overrides. An action
    /// listed in `overrides` replaces that action's default chords; actions not
    /// listed keep their defaults.
    pub fn from_config(overrides: Option<&HashMap<String, Vec<String>>>) -> Self {
        let mut bindings = HashMap::new();
        for (chord, action) in DEFAULTS {
            if let Some(chord) = parse_chord(chord) {
                bindings.insert(chord, *action);
            }
        }
        if let Some(overrides) = overrides {
            // Track which chords the *config* has already claimed, so a chord
            // assigned to two different actions in the config surfaces a warning
            // (silent shadowing otherwise). Overriding a default is normal
            // remapping and stays quiet — only config-vs-config collisions warn.
            let mut configured: HashMap<Chord, Action> = HashMap::new();
            for (name, chords) in overrides {
                let Some(action) = parse_action(name) else {
                    tracing::warn!("unknown keybinding action {name:?}; ignored");
                    continue;
                };
                bindings.retain(|_, bound| *bound != action);
                for chord in chords {
                    match parse_chord(chord) {
                        Some(chord) => {
                            if let Some(&prev) = configured.get(&chord) {
                                if prev != action {
                                    tracing::warn!(
                                        "keybinding chord {chord:?} is assigned to both \
                                         {prev:?} and {action:?}; the later assignment wins"
                                    );
                                }
                            }
                            configured.insert(chord, action);
                            bindings.insert(chord, action);
                        }
                        None => tracing::warn!("invalid key chord {chord:?}; ignored"),
                    }
                }
            }
        }
        Keymap { bindings }
    }

    pub fn action(&self, key: KeyEvent) -> Option<Action> {
        self.bindings
            .get(&Chord::new(key.code, key.modifiers))
            .copied()
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Self::from_config(None)
    }
}

const DEFAULTS: &[(&str, Action)] = &[
    ("q", Action::Quit),
    ("ctrl-c", Action::Quit),
    ("?", Action::Help),
    ("r", Action::Refresh),
    ("tab", Action::SwitchPane),
    ("backtab", Action::SwitchPane),
    ("d", Action::ToggleDiffMode),
    ("n", Action::ToggleLineNumbers),
    ("t", Action::CycleTheme),
    ("b", Action::ToggleChanges),
    ("i", Action::ToggleHistory),
    ("1", Action::ShowStatus),
    ("2", Action::ShowHistory),
    ("j", Action::Down),
    ("down", Action::Down),
    ("k", Action::Up),
    ("up", Action::Up),
    ("g", Action::Top),
    ("home", Action::Top),
    ("G", Action::Bottom),
    ("end", Action::Bottom),
    ("ctrl-d", Action::HalfPageDown),
    ("ctrl-u", Action::HalfPageUp),
    ("h", Action::FocusStaging),
    ("left", Action::FocusStaging),
    ("l", Action::FocusDiff),
    ("right", Action::FocusDiff),
    ("space", Action::ToggleStage),
    ("enter", Action::ToggleStage),
    ("s", Action::Stage),
    ("u", Action::Unstage),
    ("x", Action::Discard),
    ("c", Action::Comment),
    ("]", Action::NextComment),
    ("[", Action::PrevComment),
    ("X", Action::DeleteComment),
];

fn parse_action(name: &str) -> Option<Action> {
    Some(match normalize(name).as_str() {
        "quit" => Action::Quit,
        "help" => Action::Help,
        "refresh" => Action::Refresh,
        "switch-pane" => Action::SwitchPane,
        "toggle-diff-mode" | "diff-mode" | "split" => Action::ToggleDiffMode,
        "toggle-line-numbers" | "line-numbers" => Action::ToggleLineNumbers,
        "cycle-theme" | "theme" => Action::CycleTheme,
        "toggle-changes" => Action::ToggleChanges,
        "toggle-history" | "history" => Action::ToggleHistory,
        "status-view" => Action::ShowStatus,
        "history-view" => Action::ShowHistory,
        "down" | "next-file" | "scroll-down" => Action::Down,
        "up" | "prev-file" | "scroll-up" => Action::Up,
        "top" => Action::Top,
        "bottom" => Action::Bottom,
        "half-page-down" => Action::HalfPageDown,
        "half-page-up" => Action::HalfPageUp,
        "focus-staging" => Action::FocusStaging,
        "focus-diff" => Action::FocusDiff,
        "toggle-stage" => Action::ToggleStage,
        "stage" => Action::Stage,
        "unstage" => Action::Unstage,
        "discard" => Action::Discard,
        "comment" => Action::Comment,
        "next-comment" => Action::NextComment,
        "prev-comment" => Action::PrevComment,
        "delete-comment" => Action::DeleteComment,
        _ => return None,
    })
}

/// Parse a chord like `ctrl-d`, `space`, `G`, `pageup`.
fn parse_chord(text: &str) -> Option<Chord> {
    let mut parts: Vec<&str> = text.split('-').collect();
    let key = parts.pop().filter(|k| !k.is_empty())?;
    let mut mods = KeyModifiers::NONE;
    for modifier in parts {
        match modifier.to_lowercase().as_str() {
            "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
            "alt" | "meta" | "option" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            _ => return None,
        }
    }
    Some(Chord::new(parse_key(key)?, mods))
}

fn parse_key(key: &str) -> Option<KeyCode> {
    Some(match key.to_lowercase().as_str() {
        "space" => KeyCode::Char(' '),
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "esc" | "escape" => KeyCode::Esc,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        _ => {
            // A single character — keep the original case so `g` and `G` differ.
            let mut chars = key.chars();
            let only = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(only)
        }
    })
}
