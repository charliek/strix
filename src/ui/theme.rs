use ratatui::style::Color;

/// Colour palette for the whole UI. Every colour the renderer uses comes from
/// here so themes can be swapped wholesale (see M7). Values are truecolor RGB;
/// the default palette is a Tokyo Night–style dark scheme.
#[derive(Clone, Debug)]
pub struct Theme {
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

impl Theme {
    pub fn dark() -> Self {
        Theme {
            bg: Color::Rgb(26, 27, 38),
            fg: Color::Rgb(169, 177, 214),
            dim: Color::Rgb(86, 95, 137),
            border: Color::Rgb(41, 46, 66),
            border_focused: Color::Rgb(122, 162, 247),
            title: Color::Rgb(122, 162, 247),
            header_bg: Color::Rgb(22, 22, 30),
            header_fg: Color::Rgb(192, 202, 245),
            footer_bg: Color::Rgb(22, 22, 30),
            footer_fg: Color::Rgb(86, 95, 137),
            footer_key: Color::Rgb(122, 162, 247),
            staged: Color::Rgb(158, 206, 106),
            unstaged: Color::Rgb(224, 175, 104),
            untracked: Color::Rgb(125, 207, 255),
            selection_bg: Color::Rgb(40, 52, 87),
            selection_fg: Color::Rgb(192, 202, 245),
            add: Color::Rgb(158, 206, 106),
            add_bg: Color::Rgb(32, 44, 38),
            del: Color::Rgb(247, 118, 142),
            del_bg: Color::Rgb(49, 32, 39),
            hunk: Color::Rgb(125, 207, 255),
            line_no: Color::Rgb(60, 67, 99),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}
