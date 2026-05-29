//! Syntax highlighting via [syntect]. The bundled Sublime syntaxes + a dark
//! theme are loaded once (lazily) and shared. Each diff line is highlighted
//! independently — no cross-line state — which keeps work to the visible window
//! and avoids leaking one diff line's parser state into the next.

use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::Color;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

struct Assets {
    syntaxes: SyntaxSet,
    theme: Theme,
}

fn assets() -> &'static Assets {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    ASSETS.get_or_init(|| {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let mut themes = ThemeSet::load_defaults().themes;
        let theme = themes
            .remove("base16-ocean.dark")
            .or_else(|| themes.into_values().next())
            .expect("syntect ships at least one default theme");
        Assets { syntaxes, theme }
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

/// Highlight one line into `(foreground colour, text)` segments.
pub fn highlight_line(syntax: &SyntaxReference, line: &str) -> Vec<(Color, String)> {
    let assets = assets();
    let mut highlighter = HighlightLines::new(syntax, &assets.theme);
    match highlighter.highlight_line(line, &assets.syntaxes) {
        Ok(ranges) => ranges
            .into_iter()
            .map(|(style, text)| (to_color(style.foreground), text.to_string()))
            .collect(),
        Err(_) => vec![(Color::Reset, line.to_string())],
    }
}

fn to_color(color: syntect::highlighting::Color) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}
