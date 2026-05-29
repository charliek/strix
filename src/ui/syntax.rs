//! Syntax highlighting via [syntect]. The bundled Sublime syntaxes + themes are
//! loaded once (lazily) and shared. Each diff line is highlighted independently
//! — no cross-line state — which keeps work to the visible window and avoids
//! leaking one diff line's parser state into the next.

use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::Color;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

struct Assets {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
}

impl Assets {
    /// The named syntect theme, falling back to a bundled dark theme.
    fn theme(&self, name: &str) -> &Theme {
        self.themes
            .themes
            .get(name)
            .or_else(|| self.themes.themes.get("base16-ocean.dark"))
            .or_else(|| self.themes.themes.values().next())
            .expect("syntect ships at least one default theme")
    }
}

fn assets() -> &'static Assets {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    ASSETS.get_or_init(|| Assets {
        syntaxes: SyntaxSet::load_defaults_newlines(),
        themes: ThemeSet::load_defaults(),
    })
}

/// The syntax for a file path, matched by extension, falling back to plain text.
pub fn syntax_for(path: &str) -> &'static SyntaxReference {
    let assets = assets();
    let extension = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");
    assets
        .syntaxes
        .find_syntax_by_extension(extension)
        .unwrap_or_else(|| assets.syntaxes.find_syntax_plain_text())
}

/// Highlight one line with the named theme into `(foreground colour, text)`
/// segments. The text slices borrow `line`.
pub fn highlight_line<'a>(
    syntax: &SyntaxReference,
    theme_name: &str,
    line: &'a str,
) -> Vec<(Color, &'a str)> {
    let assets = assets();
    let mut highlighter = HighlightLines::new(syntax, assets.theme(theme_name));
    match highlighter.highlight_line(line, &assets.syntaxes) {
        Ok(ranges) => ranges
            .into_iter()
            .map(|(style, text)| (to_color(style.foreground), text))
            .collect(),
        Err(_) => vec![(Color::Reset, line)],
    }
}

fn to_color(color: syntect::highlighting::Color) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}
