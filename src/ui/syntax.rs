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
        syntaxes: two_face::syntax::extra_newlines(),
        themes: ThemeSet::load_defaults(),
    })
}

/// Extensions two-face's set doesn't claim under their natural name, mapped to
/// an extension it does. Discovered by running the `syntax_for` tests without
/// this map and checking which ones fell back to plain text: two-face already
/// recognises `kts`, `mts`, and `zon` directly, so only `mjs`/`cjs` need
/// aliasing to `js`. `tmpl`/`tpl` preserve the HTML fallback the old default
/// set provided for template files.
const EXTENSION_ALIASES: &[(&str, &str)] = &[
    ("mjs", "js"),
    ("cjs", "js"),
    ("tmpl", "html"),
    ("tpl", "html"),
];

/// The syntax for a file path: matched by extension, then by full file name
/// (syntect treats names like `Dockerfile` as extensions), then by an alias
/// for extensions two-face's set doesn't recognise directly, falling back to
/// plain text.
pub fn syntax_for(path: &str) -> &'static SyntaxReference {
    let assets = assets();
    let path = Path::new(path);
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");

    assets
        .syntaxes
        .find_syntax_by_extension(extension)
        .or_else(|| assets.syntaxes.find_syntax_by_extension(file_name))
        .or_else(|| {
            // Case-insensitive to match syntect's own extension lookup.
            EXTENSION_ALIASES
                .iter()
                .find(|(ext, _)| ext.eq_ignore_ascii_case(extension))
                .and_then(|(_, alias)| assets.syntaxes.find_syntax_by_extension(alias))
        })
        .unwrap_or_else(|| assets.syntaxes.find_syntax_plain_text())
}

/// Highlight one line with the named theme into owned `(foreground colour,
/// text)` segments. Owned (not borrowed) so callers can cache the result across
/// frames — see [`crate::app::App::highlight`].
pub fn highlight_line(
    syntax: &SyntaxReference,
    theme_name: &str,
    line: &str,
) -> Vec<(Color, String)> {
    let assets = assets();
    let mut highlighter = HighlightLines::new(syntax, assets.theme(theme_name));
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
