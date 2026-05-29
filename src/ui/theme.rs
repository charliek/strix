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
    pub del: Color,
    pub del_bg: Color,
    pub hunk: Color,
    pub line_no: Color,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

impl Theme {
    /// The names of the bundled presets, for help text and error messages.
    pub const PRESETS: &'static [&'static str] =
        &["tokyo-night", "dark", "light", "catppuccin", "gruvbox"];

    /// A built-in theme by name (case-insensitive, `_` and `-` interchangeable),
    /// or `None` if there's no such preset.
    pub fn preset(name: &str) -> Option<Theme> {
        match normalize(name).as_str() {
            "tokyo-night" | "tokyonight" | "tokyo" => Some(Self::tokyo_night()),
            "dark" => Some(Self::dark()),
            "light" => Some(Self::light()),
            "catppuccin" | "catppuccin-mocha" | "mocha" => Some(Self::catppuccin()),
            "gruvbox" | "gruvbox-dark" => Some(Self::gruvbox()),
            _ => None,
        }
    }

    /// Resolve a theme by name: a built-in preset, or a `<name>.toml` file in the
    /// config directory's `themes/`. Falls back to the default with a warning.
    pub fn load(name: &str, config_dir: Option<&Path>) -> Theme {
        if let Some(theme) = Self::preset(name) {
            return theme;
        }
        if let Some(dir) = config_dir {
            let path = dir.join("themes").join(format!("{name}.toml"));
            match std::fs::read_to_string(&path) {
                Ok(text) => return Theme::from_toml(&text),
                Err(err) => tracing::warn!("theme {name:?} not found ({err}); using default"),
            }
        } else {
            tracing::warn!("unknown theme {name:?}; using default");
        }
        Theme::default()
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
            del: rgb(247, 118, 142),
            del_bg: rgb(49, 32, 39),
            hunk: rgb(125, 207, 255),
            line_no: rgb(60, 67, 99),
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
            del: rgb(215, 95, 95),
            del_bg: rgb(42, 31, 31),
            hunk: rgb(95, 175, 215),
            line_no: rgb(88, 88, 88),
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
            del: rgb(228, 86, 73),
            del_bg: rgb(255, 238, 240),
            hunk: rgb(1, 132, 188),
            line_no: rgb(192, 192, 192),
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
            del: rgb(243, 139, 168),
            del_bg: rgb(48, 35, 43),
            hunk: rgb(137, 220, 235),
            line_no: rgb(69, 71, 90),
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
            del: rgb(251, 73, 52),
            del_bg: rgb(58, 36, 32),
            hunk: rgb(131, 165, 152),
            line_no: rgb(80, 73, 69),
        }
    }

    /// Parse a custom theme: a `base` preset (default `tokyo-night`) with any
    /// `[colors]` and `syntax` fields overridden. Unknown/invalid fields keep
    /// the base value.
    fn from_toml(text: &str) -> Theme {
        let file: ThemeFile = match toml::from_str(text) {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!("invalid theme file ({err}); using default");
                return Theme::default();
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
        theme
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
    del: Option<String>,
    del_bg: Option<String>,
    hunk: Option<String>,
    line_no: Option<String>,
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
        set(&self.del, &mut theme.del);
        set(&self.del_bg, &mut theme.del_bg);
        set(&self.hunk, &mut theme.hunk);
        set(&self.line_no, &mut theme.line_no);
    }
}
