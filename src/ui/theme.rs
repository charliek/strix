use std::path::Path;

use ratatui::style::Color;

use crate::util::normalize;

/// Colour palette for the whole UI. Every colour the renderer uses comes from
/// here so themes can be swapped wholesale. `syntax_theme` names the bundled
/// syntect theme used for code highlighting. Values are truecolor RGB.
#[derive(Clone, Debug)]
pub struct Theme {
    /// Name of the syntect theme used for code highlighting.
    pub syntax_theme: String,
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
    pub border: Color,
    pub border_focused: Color,
    pub title: Color,
    pub header_bg: Color,
    pub header_fg: Color,
    pub footer_bg: Color,
    pub footer_fg: Color,
    pub footer_key: Color,
    pub staged: Color,
    pub unstaged: Color,
    pub untracked: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub add: Color,
    pub add_bg: Color,
    /// Side-by-side word-diff emphasis for a changed span on the new side (plan
    /// §3.7) — brighter/more saturated than `add_bg`'s flat wash, so an edited
    /// substring reads distinctly instead of blending into the whole line.
    pub add_emph: Color,
    /// Side-by-side filler for the empty old column opposite a pure addition
    /// (plan §3.2) — a dim green tint between `bg` and `add_bg`, so the change
    /// reads as anchored instead of the empty side looking like flat background.
    pub add_gutter: Color,
    pub del: Color,
    pub del_bg: Color,
    /// Side-by-side word-diff emphasis for a changed span on the old side (plan
    /// §3.7); the `del_bg` counterpart to `add_emph`.
    pub del_emph: Color,
    /// Side-by-side filler for the empty new column opposite a pure deletion
    /// (plan §3.2); the `del_bg` counterpart to `add_gutter`.
    pub del_gutter: Color,
    pub hunk: Color,
    pub line_no: Color,
    /// Accent for review comments (the `● you`/`● agent` rows and the file-list
    /// `● n` badge). A purple/magenta family in every preset, distinct from the
    /// add/del/hunk diff colours.
    pub comment: Color,
    /// Cycling colours for the history graph's branch lanes.
    pub lanes: Vec<Color>,
}

impl Theme {
    /// The colour for graph lane `index`, cycling through the palette.
    pub fn lane(&self, index: usize) -> Color {
        if self.lanes.is_empty() {
            return self.title;
        }
        self.lanes[index % self.lanes.len()]
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

impl Theme {
    /// The names of the bundled presets, for help text and error messages.
    pub const PRESETS: &'static [&'static str] =
        &["tokyo-night", "dark", "light", "catppuccin", "gruvbox"];

    /// The canonical preset name for `name` (folding aliases and case/`_`-`-`
    /// spelling), or `None` if there's no such preset. Single source of truth for
    /// the alias table, shared by [`preset`](Self::preset) and
    /// [`resolve`](Self::resolve).
    fn preset_canonical(name: &str) -> Option<&'static str> {
        Some(match normalize(name).as_str() {
            "tokyo-night" | "tokyonight" | "tokyo" => "tokyo-night",
            "dark" => "dark",
            "light" => "light",
            "catppuccin" | "catppuccin-mocha" | "mocha" => "catppuccin",
            "gruvbox" | "gruvbox-dark" => "gruvbox",
            _ => return None,
        })
    }

    /// A built-in theme by name (case-insensitive, `_` and `-` interchangeable),
    /// or `None` if there's no such preset.
    pub fn preset(name: &str) -> Option<Theme> {
        Some(Self::theme_for_canonical(Self::preset_canonical(name)?))
    }

    /// Build the palette for an already-canonical preset name. Kept separate from
    /// [`preset_canonical`](Self::preset_canonical) so [`resolve`](Self::resolve)
    /// can build a theme from a canonical name it already computed, without
    /// re-normalizing or re-matching the alias table.
    fn theme_for_canonical(canonical: &str) -> Theme {
        match canonical {
            "tokyo-night" => Self::tokyo_night(),
            "dark" => Self::dark(),
            "light" => Self::light(),
            "catppuccin" => Self::catppuccin(),
            "gruvbox" => Self::gruvbox(),
            other => unreachable!("non-canonical preset name {other:?}"),
        }
    }

    /// Resolve a theme by name, reporting the **canonical** name actually loaded
    /// alongside the theme. Aliases fold to their canonical preset name; an
    /// unknown name and a malformed custom file both resolve to
    /// `("tokyo-night", default)`; a valid custom `<name>.toml` resolves to its
    /// stem. Reporting the resolved name (never the requested one) keeps the shown
    /// name from ever diverging from the displayed theme.
    pub fn resolve(name: &str, config_dir: Option<&Path>) -> (String, Theme) {
        if let Some(canonical) = Self::preset_canonical(name) {
            return (canonical.to_string(), Self::theme_for_canonical(canonical));
        }
        if let Some(dir) = config_dir {
            let path = dir.join("themes").join(format!("{name}.toml"));
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    return match Theme::from_toml_checked(&text) {
                        Some(theme) => (name.to_string(), theme),
                        // A malformed file warns and resolves to the default,
                        // whose canonical name is what gets reported/flashed.
                        None => (Self::PRESETS[0].to_string(), Theme::default()),
                    };
                }
                Err(err) => tracing::warn!("theme {name:?} not found ({err}); using default"),
            }
        } else {
            tracing::warn!("unknown theme {name:?}; using default");
        }
        (Self::PRESETS[0].to_string(), Theme::default())
    }

    /// The themes that can be cycled through, in cycle order: the `PRESETS` first
    /// (their canonical order), then user `themes/*.toml` stems sorted lexically.
    /// A custom stem that shadows a preset name is omitted — presets win, matching
    /// [`resolve`](Self::resolve)/[`load`](Self::load) precedence.
    pub fn available(config_dir: Option<&Path>) -> Vec<String> {
        let mut names: Vec<String> = Self::PRESETS.iter().map(|name| name.to_string()).collect();
        if let Some(dir) = config_dir {
            let mut user = Vec::new();
            if let Ok(entries) = std::fs::read_dir(dir.join("themes")) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                        continue;
                    }
                    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                        continue;
                    };
                    if Self::preset_canonical(stem).is_some() {
                        // A stem shadowing a preset name OR alias is omitted:
                        // resolve() would load the preset, never the file.
                        continue;
                    }
                    user.push(stem.to_string());
                }
            }
            user.sort();
            names.extend(user);
        }
        names
    }

    /// Resolve the theme that follows `current` in the cycle, wrapping around.
    /// `current` is matched against [`available`](Self::available) (enumerated
    /// fresh each call, so a newly added/removed user theme is picked up); a
    /// `current` no longer present — e.g. its file was deleted — restarts the
    /// cycle at index 0. Returns the canonical name + theme via
    /// [`resolve`](Self::resolve).
    pub fn cycle(current: &str, config_dir: Option<&Path>) -> (String, Theme) {
        let names = Self::available(config_dir);
        let next = match names.iter().position(|name| name == current) {
            Some(index) => (index + 1) % names.len(),
            None => 0,
        };
        Self::resolve(&names[next], config_dir)
    }

    fn tokyo_night() -> Self {
        Theme {
            syntax_theme: "base16-ocean.dark".into(),
            bg: rgb(26, 27, 38),
            fg: rgb(169, 177, 214),
            dim: rgb(86, 95, 137),
            border: rgb(41, 46, 66),
            border_focused: rgb(122, 162, 247),
            title: rgb(122, 162, 247),
            header_bg: rgb(22, 22, 30),
            header_fg: rgb(192, 202, 245),
            footer_bg: rgb(22, 22, 30),
            footer_fg: rgb(86, 95, 137),
            footer_key: rgb(122, 162, 247),
            staged: rgb(158, 206, 106),
            unstaged: rgb(224, 175, 104),
            untracked: rgb(125, 207, 255),
            selection_bg: rgb(40, 52, 87),
            selection_fg: rgb(192, 202, 245),
            add: rgb(158, 206, 106),
            add_bg: rgb(32, 44, 38),
            add_emph: rgb(46, 92, 58),
            add_gutter: rgb(26, 36, 31),
            del: rgb(247, 118, 142),
            del_bg: rgb(49, 32, 39),
            del_emph: rgb(100, 42, 60),
            del_gutter: rgb(40, 28, 33),
            hunk: rgb(125, 207, 255),
            line_no: rgb(60, 67, 99),
            comment: rgb(187, 154, 247),
            lanes: vec![
                rgb(122, 162, 247),
                rgb(158, 206, 106),
                rgb(224, 175, 104),
                rgb(187, 154, 247),
                rgb(125, 207, 255),
                rgb(247, 118, 142),
            ],
        }
    }

    fn dark() -> Self {
        Theme {
            syntax_theme: "base16-ocean.dark".into(),
            bg: rgb(28, 28, 28),
            fg: rgb(208, 208, 208),
            dim: rgb(128, 128, 128),
            border: rgb(58, 58, 58),
            border_focused: rgb(95, 135, 215),
            title: rgb(95, 135, 215),
            header_bg: rgb(18, 18, 18),
            header_fg: rgb(228, 228, 228),
            footer_bg: rgb(18, 18, 18),
            footer_fg: rgb(128, 128, 128),
            footer_key: rgb(95, 135, 215),
            staged: rgb(135, 175, 95),
            unstaged: rgb(215, 175, 95),
            untracked: rgb(95, 175, 215),
            selection_bg: rgb(48, 48, 48),
            selection_fg: rgb(228, 228, 228),
            add: rgb(135, 175, 95),
            add_bg: rgb(31, 42, 31),
            add_emph: rgb(48, 84, 42),
            add_gutter: rgb(25, 35, 25),
            del: rgb(215, 95, 95),
            del_bg: rgb(42, 31, 31),
            del_emph: rgb(84, 42, 42),
            del_gutter: rgb(35, 26, 26),
            hunk: rgb(95, 175, 215),
            line_no: rgb(88, 88, 88),
            comment: rgb(175, 135, 215),
            lanes: vec![
                rgb(95, 135, 215),
                rgb(135, 175, 95),
                rgb(215, 175, 95),
                rgb(175, 135, 215),
                rgb(95, 175, 215),
                rgb(215, 95, 95),
            ],
        }
    }

    fn light() -> Self {
        Theme {
            syntax_theme: "InspiredGitHub".into(),
            bg: rgb(250, 250, 250),
            fg: rgb(56, 58, 66),
            dim: rgb(160, 161, 167),
            border: rgb(212, 212, 212),
            border_focused: rgb(64, 120, 242),
            title: rgb(64, 120, 242),
            header_bg: rgb(234, 234, 235),
            header_fg: rgb(56, 58, 66),
            footer_bg: rgb(234, 234, 235),
            footer_fg: rgb(160, 161, 167),
            footer_key: rgb(64, 120, 242),
            staged: rgb(80, 161, 79),
            unstaged: rgb(193, 132, 1),
            untracked: rgb(1, 132, 188),
            selection_bg: rgb(208, 215, 230),
            selection_fg: rgb(56, 58, 66),
            add: rgb(80, 161, 79),
            add_bg: rgb(230, 255, 237),
            add_emph: rgb(169, 235, 180),
            add_gutter: rgb(240, 250, 242),
            del: rgb(228, 86, 73),
            del_bg: rgb(255, 238, 240),
            del_emph: rgb(250, 189, 192),
            del_gutter: rgb(252, 244, 245),
            hunk: rgb(1, 132, 188),
            line_no: rgb(192, 192, 192),
            comment: rgb(166, 38, 164),
            lanes: vec![
                rgb(64, 120, 242),
                rgb(80, 161, 79),
                rgb(193, 132, 1),
                rgb(166, 38, 164),
                rgb(1, 132, 188),
                rgb(228, 86, 73),
            ],
        }
    }

    fn catppuccin() -> Self {
        Theme {
            syntax_theme: "base16-mocha.dark".into(),
            bg: rgb(30, 30, 46),
            fg: rgb(205, 214, 244),
            dim: rgb(108, 112, 134),
            border: rgb(49, 50, 68),
            border_focused: rgb(137, 180, 250),
            title: rgb(137, 180, 250),
            header_bg: rgb(24, 24, 37),
            header_fg: rgb(205, 214, 244),
            footer_bg: rgb(24, 24, 37),
            footer_fg: rgb(108, 112, 134),
            footer_key: rgb(137, 180, 250),
            staged: rgb(166, 227, 161),
            unstaged: rgb(249, 226, 175),
            untracked: rgb(137, 220, 235),
            selection_bg: rgb(49, 50, 68),
            selection_fg: rgb(205, 214, 244),
            add: rgb(166, 227, 161),
            add_bg: rgb(35, 44, 41),
            add_emph: rgb(48, 84, 63),
            add_gutter: rgb(30, 38, 35),
            del: rgb(243, 139, 168),
            del_bg: rgb(48, 35, 43),
            del_emph: rgb(94, 48, 70),
            del_gutter: rgb(41, 30, 37),
            hunk: rgb(137, 220, 235),
            line_no: rgb(69, 71, 90),
            comment: rgb(203, 166, 247),
            lanes: vec![
                rgb(137, 180, 250),
                rgb(166, 227, 161),
                rgb(249, 226, 175),
                rgb(203, 166, 247),
                rgb(137, 220, 235),
                rgb(243, 139, 168),
            ],
        }
    }

    fn gruvbox() -> Self {
        Theme {
            syntax_theme: "base16-eighties.dark".into(),
            bg: rgb(40, 40, 40),
            fg: rgb(235, 219, 178),
            dim: rgb(146, 131, 116),
            border: rgb(60, 56, 54),
            border_focused: rgb(250, 189, 47),
            title: rgb(250, 189, 47),
            header_bg: rgb(29, 32, 33),
            header_fg: rgb(235, 219, 178),
            footer_bg: rgb(29, 32, 33),
            footer_fg: rgb(146, 131, 116),
            footer_key: rgb(250, 189, 47),
            staged: rgb(184, 187, 38),
            unstaged: rgb(250, 189, 47),
            untracked: rgb(142, 192, 124),
            selection_bg: rgb(60, 56, 54),
            selection_fg: rgb(235, 219, 178),
            add: rgb(184, 187, 38),
            add_bg: rgb(50, 54, 31),
            add_emph: rgb(86, 90, 40),
            add_gutter: rgb(43, 46, 28),
            del: rgb(251, 73, 52),
            del_bg: rgb(58, 36, 32),
            del_emph: rgb(100, 46, 38),
            del_gutter: rgb(49, 32, 29),
            hunk: rgb(131, 165, 152),
            line_no: rgb(80, 73, 69),
            comment: rgb(211, 134, 155),
            lanes: vec![
                rgb(131, 165, 152),
                rgb(184, 187, 38),
                rgb(250, 189, 47),
                rgb(211, 134, 155),
                rgb(142, 192, 124),
                rgb(251, 73, 52),
            ],
        }
    }

    /// Parse a custom theme: a `base` preset (default `tokyo-night`) with any
    /// `[colors]` and `syntax` fields overridden. Unknown/invalid fields keep the
    /// base value. Returns `None` for a malformed file (a TOML parse error), so
    /// [`resolve`](Self::resolve) can report the default theme's canonical name
    /// instead of the broken file's stem.
    fn from_toml_checked(text: &str) -> Option<Theme> {
        let file: ThemeFile = match toml::from_str(text) {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!("invalid theme file ({err}); using default");
                return None;
            }
        };
        let mut theme = file
            .base
            .as_deref()
            .and_then(Theme::preset)
            .unwrap_or_default();
        if let Some(syntax) = file.syntax {
            theme.syntax_theme = syntax;
        }
        file.colors.apply(&mut theme);
        Some(theme)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::tokyo_night()
    }
}

/// Parse `#rrggbb` / `rrggbb` into a colour.
fn parse_hex(value: &str) -> Option<Color> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

#[derive(serde::Deserialize)]
struct ThemeFile {
    base: Option<String>,
    syntax: Option<String>,
    #[serde(default)]
    colors: ColorsFile,
}

/// All colours optional; present ones override the base theme.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
struct ColorsFile {
    bg: Option<String>,
    fg: Option<String>,
    dim: Option<String>,
    border: Option<String>,
    border_focused: Option<String>,
    title: Option<String>,
    header_bg: Option<String>,
    header_fg: Option<String>,
    footer_bg: Option<String>,
    footer_fg: Option<String>,
    footer_key: Option<String>,
    staged: Option<String>,
    unstaged: Option<String>,
    untracked: Option<String>,
    selection_bg: Option<String>,
    selection_fg: Option<String>,
    add: Option<String>,
    add_bg: Option<String>,
    /// Unset falls back to the base preset's `add_emph` (plan §3.7) — an
    /// existing user theme file predating this field is unaffected.
    add_emph: Option<String>,
    /// Unset falls back to the base preset's `add_gutter` (plan §3.2) — an
    /// existing user theme file predating this field is unaffected.
    add_gutter: Option<String>,
    del: Option<String>,
    del_bg: Option<String>,
    del_emph: Option<String>,
    /// Unset falls back to the base preset's `del_gutter` (plan §3.2).
    del_gutter: Option<String>,
    hunk: Option<String>,
    line_no: Option<String>,
    comment: Option<String>,
}

impl ColorsFile {
    fn apply(&self, theme: &mut Theme) {
        let set = |value: &Option<String>, slot: &mut Color| {
            if let Some(hex) = value {
                match parse_hex(hex) {
                    Some(color) => *slot = color,
                    None => tracing::warn!("invalid colour {hex:?} in theme; ignored"),
                }
            }
        };
        set(&self.bg, &mut theme.bg);
        set(&self.fg, &mut theme.fg);
        set(&self.dim, &mut theme.dim);
        set(&self.border, &mut theme.border);
        set(&self.border_focused, &mut theme.border_focused);
        set(&self.title, &mut theme.title);
        set(&self.header_bg, &mut theme.header_bg);
        set(&self.header_fg, &mut theme.header_fg);
        set(&self.footer_bg, &mut theme.footer_bg);
        set(&self.footer_fg, &mut theme.footer_fg);
        set(&self.footer_key, &mut theme.footer_key);
        set(&self.staged, &mut theme.staged);
        set(&self.unstaged, &mut theme.unstaged);
        set(&self.untracked, &mut theme.untracked);
        set(&self.selection_bg, &mut theme.selection_bg);
        set(&self.selection_fg, &mut theme.selection_fg);
        set(&self.add, &mut theme.add);
        set(&self.add_bg, &mut theme.add_bg);
        set(&self.add_emph, &mut theme.add_emph);
        set(&self.add_gutter, &mut theme.add_gutter);
        set(&self.del, &mut theme.del);
        set(&self.del_bg, &mut theme.del_bg);
        set(&self.del_emph, &mut theme.del_emph);
        set(&self.del_gutter, &mut theme.del_gutter);
        set(&self.hunk, &mut theme.hunk);
        set(&self.line_no, &mut theme.line_no);
        set(&self.comment, &mut theme.comment);
    }
}
