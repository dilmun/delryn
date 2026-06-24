//! Colour themes — the single source of truth for every colour the app paints.
//! Each theme maps semantic roles (body, heading, quote, link, code, markers,
//! status bar, errors) to colours, names a syntect theme so prose and code stay
//! coordinated, and resolves the concrete `ink`/`paper` used to recolour images.
//! No renderer should hardcode a colour or a fallback — go through a `Theme`.
//! See `DESIGN.md` §7.

use std::sync::OnceLock;

use ratatui::style::{Color, Style};

/// The terminal's real background colour, queried once at startup (OSC 11). The
/// `terminal` theme uses no colours of its own, so without this it would recolour
/// images against a white-paper fallback; with it, equations and inverted figures
/// match the actual backdrop.
static TERMINAL_BG: OnceLock<[u8; 3]> = OnceLock::new();

/// Record the detected terminal background (best-effort; ignored if already set).
pub fn set_terminal_background(rgb: [u8; 3]) {
    let _ = TERMINAL_BG.set(rgb);
}

fn terminal_background() -> Option<[u8; 3]> {
    TERMINAL_BG.get().copied()
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub name: &'static str,
    /// Pane background; `None` means use the terminal's own background.
    pub bg: Option<Color>,
    pub fg: Color,
    pub heading: Color,
    pub quote: Color,
    pub link: Color,
    /// Rules, code gutter, de-emphasised text.
    pub muted: Color,
    /// List markers and accents.
    pub marker: Color,
    /// Inline code / preformatted fallback.
    pub code_fg: Color,
    pub status_fg: Color,
    pub status_bg: Color,
    /// Sidebar selection / current-chapter accent.
    pub accent: Color,
    /// Errors / invalid input / destructive actions.
    pub danger: Color,
    /// syntect theme name used to highlight code blocks.
    pub syntect: &'static str,
}

impl Theme {
    /// The next theme in the cycle.
    pub fn next(&self) -> Theme {
        let i = THEMES.iter().position(|t| t.name == self.name).unwrap_or(0);
        THEMES[(i + 1) % THEMES.len()]
    }

    /// The previous theme in the cycle.
    pub fn prev(&self) -> Theme {
        let i = THEMES.iter().position(|t| t.name == self.name).unwrap_or(0);
        THEMES[(i + THEMES.len() - 1) % THEMES.len()]
    }

    /// The base text style: foreground always, background only when the theme
    /// paints one (so the `terminal` theme keeps the terminal's own backdrop).
    pub fn text_style(&self) -> Style {
        let s = Style::default().fg(self.fg);
        match self.bg {
            Some(bg) => s.bg(bg),
            None => s,
        }
    }

    /// A concrete, opaque page colour for popups/overlays that must stay readable
    /// over content. Falls back to black for the `terminal` theme, which has no
    /// background of its own.
    pub fn paper(&self) -> Color {
        self.bg.unwrap_or(Color::Black)
    }

    /// The foreground for text drawn *on* the `accent` colour (selections, pills,
    /// highlighted rows) — i.e. the inverse of the page.
    pub fn on_accent(&self) -> Color {
        self.paper()
    }

    /// The (ink, paper) sRGB pair used to recolour monochrome/line-art images to
    /// match the theme. Terminal-relative colours (`Reset`, no `bg`) fall back to
    /// black ink on a white page — the publisher's intent — and the two are
    /// forced apart if they'd be too close to read.
    pub fn image_ink(&self) -> ([u8; 3], [u8; 3]) {
        self.resolve_image_ink(terminal_background())
    }

    /// As [`image_ink`](Self::image_ink), but with the terminal-background fallback
    /// passed in (so it's testable without the process-global). For the `terminal`
    /// theme (no `bg` of its own), `term_bg` becomes the page colour.
    fn resolve_image_ink(&self, term_bg: Option<[u8; 3]>) -> ([u8; 3], [u8; 3]) {
        let ink = rgb_of(self.fg).unwrap_or([0, 0, 0]);
        let paper = self
            .bg
            .and_then(rgb_of)
            .or(term_bg)
            .unwrap_or([255, 255, 255]);
        let lum = |c: [u8; 3]| 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32;
        if (lum(ink) - lum(paper)).abs() < 64.0 {
            let opposite = if lum(paper) < 128.0 {
                [235, 235, 235]
            } else {
                [20, 20, 20]
            };
            (opposite, paper)
        } else {
            (ink, paper)
        }
    }
}

/// Concrete sRGB for a ratatui [`Color`], or `None` for terminal-relative colours
/// (`Reset`, palette indices) whose true RGB we can't know.
pub fn rgb_of(c: Color) -> Option<[u8; 3]> {
    Some(match c {
        Color::Rgb(r, g, b) => [r, g, b],
        Color::Black => [0, 0, 0],
        Color::White => [238, 238, 238],
        Color::Red => [205, 49, 49],
        Color::Green => [13, 188, 121],
        Color::Yellow => [229, 229, 16],
        Color::Blue => [36, 114, 200],
        Color::Magenta => [188, 63, 188],
        Color::Cyan => [17, 168, 205],
        Color::Gray => [180, 180, 180],
        Color::DarkGray => [102, 102, 102],
        Color::LightRed => [241, 76, 76],
        Color::LightGreen => [35, 209, 139],
        Color::LightYellow => [245, 245, 67],
        Color::LightBlue => [59, 142, 234],
        Color::LightMagenta => [214, 112, 214],
        Color::LightCyan => [41, 184, 219],
        Color::Reset | Color::Indexed(_) => return None,
    })
}

/// Look a theme up by name (for persistence).
pub fn by_name(name: &str) -> Option<Theme> {
    THEMES.iter().copied().find(|t| t.name == name)
}

pub fn default_theme() -> Theme {
    TERMINAL
}

/// All built-in themes, in cycle order.
pub const THEMES: &[Theme] = &[
    TERMINAL,
    DARK,
    OLED,
    HIGH_CONTRAST,
    SOLARIZED_DARK,
    SOLARIZED_LIGHT,
    DRACULA,
    GRUVBOX,
    LIGHT,
];

/// Uses the terminal's own background and ANSI palette where possible.
pub const TERMINAL: Theme = Theme {
    name: "terminal",
    bg: None,
    fg: Color::Reset,
    heading: Color::Reset,
    quote: Color::DarkGray,
    link: Color::Rgb(88, 160, 255),
    muted: Color::DarkGray,
    marker: Color::Rgb(229, 192, 123),
    code_fg: Color::Rgb(152, 195, 121),
    status_fg: Color::Black,
    status_bg: Color::Gray,
    accent: Color::Rgb(137, 180, 250),
    danger: Color::Rgb(0xe0, 0x5a, 0x5a),
    syntect: "base16-ocean.dark",
};

pub const DARK: Theme = Theme {
    name: "dark",
    bg: Some(Color::Rgb(0x1e, 0x1e, 0x2e)),
    fg: Color::Rgb(0xcd, 0xd6, 0xf4),
    heading: Color::Rgb(0x89, 0xb4, 0xfa),
    quote: Color::Rgb(0x93, 0x99, 0xb2),
    link: Color::Rgb(0x89, 0xdc, 0xeb),
    muted: Color::Rgb(0x6c, 0x70, 0x86),
    marker: Color::Rgb(0xfa, 0xb3, 0x87),
    code_fg: Color::Rgb(0xa6, 0xe3, 0xa1),
    status_fg: Color::Rgb(0x1e, 0x1e, 0x2e),
    status_bg: Color::Rgb(0x89, 0xb4, 0xfa),
    accent: Color::Rgb(0xf5, 0xc2, 0xe7),
    danger: Color::Rgb(0xf3, 0x8b, 0xa8),
    syntect: "base16-mocha.dark",
};

pub const OLED: Theme = Theme {
    name: "oled",
    bg: Some(Color::Rgb(0, 0, 0)),
    fg: Color::Rgb(0xd0, 0xd0, 0xd0),
    heading: Color::Rgb(0xff, 0xff, 0xff),
    quote: Color::Rgb(0x80, 0x80, 0x80),
    link: Color::Rgb(0x4e, 0xa1, 0xff),
    muted: Color::Rgb(0x5a, 0x5a, 0x5a),
    marker: Color::Rgb(0xff, 0xb4, 0x54),
    code_fg: Color::Rgb(0x8e, 0xc0, 0x7c),
    status_fg: Color::Rgb(0xd0, 0xd0, 0xd0),
    status_bg: Color::Rgb(0x18, 0x18, 0x18),
    accent: Color::Rgb(0x4e, 0xa1, 0xff),
    danger: Color::Rgb(0xff, 0x5a, 0x5a),
    syntect: "base16-ocean.dark",
};

pub const HIGH_CONTRAST: Theme = Theme {
    name: "high-contrast",
    bg: Some(Color::Rgb(0, 0, 0)),
    fg: Color::Rgb(0xff, 0xff, 0xff),
    heading: Color::Rgb(0xff, 0xff, 0x00),
    quote: Color::Rgb(0xc0, 0xc0, 0xc0),
    link: Color::Rgb(0x00, 0xff, 0xff),
    muted: Color::Rgb(0xc0, 0xc0, 0xc0),
    marker: Color::Rgb(0x00, 0xff, 0x00),
    code_fg: Color::Rgb(0x00, 0xff, 0x00),
    status_fg: Color::Rgb(0x00, 0x00, 0x00),
    status_bg: Color::Rgb(0xff, 0xff, 0xff),
    accent: Color::Rgb(0xff, 0xff, 0x00),
    danger: Color::Rgb(0xff, 0x00, 0x00),
    syntect: "base16-ocean.dark",
};

pub const SOLARIZED_DARK: Theme = Theme {
    name: "solarized-dark",
    bg: Some(Color::Rgb(0x00, 0x2b, 0x36)),
    fg: Color::Rgb(0x83, 0x94, 0x96),
    heading: Color::Rgb(0xb5, 0x89, 0x00),
    quote: Color::Rgb(0x58, 0x6e, 0x75),
    link: Color::Rgb(0x26, 0x8b, 0xd2),
    muted: Color::Rgb(0x58, 0x6e, 0x75),
    marker: Color::Rgb(0xcb, 0x4b, 0x16),
    code_fg: Color::Rgb(0x2a, 0xa1, 0x98),
    status_fg: Color::Rgb(0x93, 0xa1, 0xa1),
    status_bg: Color::Rgb(0x07, 0x36, 0x42),
    accent: Color::Rgb(0x26, 0x8b, 0xd2),
    danger: Color::Rgb(0xdc, 0x32, 0x2f),
    syntect: "Solarized (dark)",
};

pub const SOLARIZED_LIGHT: Theme = Theme {
    name: "solarized-light",
    bg: Some(Color::Rgb(0xfd, 0xf6, 0xe3)),
    fg: Color::Rgb(0x65, 0x7b, 0x83),
    heading: Color::Rgb(0xb5, 0x89, 0x00),
    quote: Color::Rgb(0x93, 0xa1, 0xa1),
    link: Color::Rgb(0x26, 0x8b, 0xd2),
    muted: Color::Rgb(0x93, 0xa1, 0xa1),
    marker: Color::Rgb(0xcb, 0x4b, 0x16),
    code_fg: Color::Rgb(0x2a, 0xa1, 0x98),
    status_fg: Color::Rgb(0x58, 0x6e, 0x75),
    status_bg: Color::Rgb(0xee, 0xe8, 0xd5),
    accent: Color::Rgb(0x26, 0x8b, 0xd2),
    danger: Color::Rgb(0xdc, 0x32, 0x2f),
    syntect: "Solarized (light)",
};

pub const DRACULA: Theme = Theme {
    name: "dracula",
    bg: Some(Color::Rgb(0x28, 0x2a, 0x36)),
    fg: Color::Rgb(0xf8, 0xf8, 0xf2),
    heading: Color::Rgb(0xbd, 0x93, 0xf9),
    quote: Color::Rgb(0x62, 0x72, 0xa4),
    link: Color::Rgb(0x8b, 0xe9, 0xfd),
    muted: Color::Rgb(0x62, 0x72, 0xa4),
    marker: Color::Rgb(0xff, 0xb8, 0x6c),
    code_fg: Color::Rgb(0x50, 0xfa, 0x7b),
    status_fg: Color::Rgb(0xf8, 0xf8, 0xf2),
    status_bg: Color::Rgb(0x44, 0x47, 0x5a),
    accent: Color::Rgb(0xff, 0x79, 0xc6),
    danger: Color::Rgb(0xff, 0x55, 0x55),
    syntect: "base16-mocha.dark",
};

pub const GRUVBOX: Theme = Theme {
    name: "gruvbox",
    bg: Some(Color::Rgb(0x28, 0x28, 0x28)),
    fg: Color::Rgb(0xeb, 0xdb, 0xb2),
    heading: Color::Rgb(0xfa, 0xbd, 0x2f),
    quote: Color::Rgb(0x92, 0x83, 0x74),
    link: Color::Rgb(0x83, 0xa5, 0x98),
    muted: Color::Rgb(0x92, 0x83, 0x74),
    marker: Color::Rgb(0xfe, 0x80, 0x19),
    code_fg: Color::Rgb(0xb8, 0xbb, 0x26),
    status_fg: Color::Rgb(0x28, 0x28, 0x28),
    status_bg: Color::Rgb(0xa8, 0x99, 0x84),
    accent: Color::Rgb(0xfa, 0xbd, 0x2f),
    danger: Color::Rgb(0xfb, 0x49, 0x34),
    syntect: "base16-eighties.dark",
};

pub const LIGHT: Theme = Theme {
    name: "light",
    bg: Some(Color::Rgb(0xff, 0xff, 0xff)),
    fg: Color::Rgb(0x1a, 0x1a, 0x1a),
    heading: Color::Rgb(0x00, 0x00, 0x00),
    quote: Color::Rgb(0x6a, 0x6a, 0x6a),
    link: Color::Rgb(0x0b, 0x5c, 0xad),
    muted: Color::Rgb(0x9a, 0x9a, 0x9a),
    marker: Color::Rgb(0xb3, 0x59, 0x00),
    code_fg: Color::Rgb(0x0a, 0x6b, 0x3a),
    status_fg: Color::Rgb(0x1a, 0x1a, 0x1a),
    status_bg: Color::Rgb(0xe6, 0xe6, 0xe6),
    accent: Color::Rgb(0x0b, 0x5c, 0xad),
    danger: Color::Rgb(0xc0, 0x28, 0x28),
    syntect: "InspiredGitHub",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_is_concrete_even_for_terminal_theme() {
        // The terminal theme has no bg of its own, but popups need a concrete one.
        assert_eq!(TERMINAL.paper(), Color::Black);
        assert_eq!(OLED.paper(), Color::Rgb(0, 0, 0));
        assert_eq!(LIGHT.paper(), Color::Rgb(0xff, 0xff, 0xff));
    }

    #[test]
    fn every_theme_resolves_a_readable_image_ink_pair() {
        // ink and paper must always differ enough to read an equation against.
        let lum = |c: [u8; 3]| 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32;
        for t in THEMES {
            let (ink, paper) = t.image_ink();
            assert!(
                (lum(ink) - lum(paper)).abs() >= 64.0,
                "{} ink/paper too close: {ink:?} vs {paper:?}",
                t.name
            );
        }
    }

    #[test]
    fn terminal_theme_inks_black_on_white() {
        // Reset fg + no bg, no detected terminal colour → the publisher's intended
        // dark-on-light page.
        assert_eq!(
            TERMINAL.resolve_image_ink(None),
            ([0, 0, 0], [255, 255, 255])
        );
    }

    #[test]
    fn terminal_theme_uses_detected_background() {
        // With a detected dark terminal background, the page becomes that colour
        // and the ink is snapped light for contrast.
        let (ink, paper) = TERMINAL.resolve_image_ink(Some([20, 22, 26]));
        assert_eq!(paper, [20, 22, 26], "page = real terminal bg");
        let lum = |c: [u8; 3]| 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32;
        assert!(lum(ink) > 128.0, "ink snapped light on a dark bg: {ink:?}");
        // A concrete theme ignores the terminal fallback.
        assert_eq!(OLED.resolve_image_ink(Some([20, 22, 26])).1, [0, 0, 0]);
    }

    #[test]
    fn rgb_of_resolves_concrete_but_not_terminal_relative() {
        assert_eq!(rgb_of(Color::Rgb(1, 2, 3)), Some([1, 2, 3]));
        assert_eq!(rgb_of(Color::Black), Some([0, 0, 0]));
        assert_eq!(rgb_of(Color::Reset), None);
        assert_eq!(rgb_of(Color::Indexed(5)), None);
    }
}
