//! The built-in themes, compiled in. User themes (loaded from
//! `<config>/themes/*.toml`) are appended to the registry at runtime — see
//! [`super::themes`].

use ratatui::style::Color;

use super::Theme;

/// All built-in themes, in cycle order.
pub const BUILTINS: &[Theme] = &[
    AUTO,
    DARK,
    OLED,
    HIGH_CONTRAST,
    SOLARIZED_DARK,
    SOLARIZED_LIGHT,
    DRACULA,
    GRUVBOX,
    LIGHT,
];

/// Adapts to the terminal: no background of its own (keeps the terminal's own
/// backdrop for prose), and its overlays/images resolve against the **detected**
/// terminal background (see [`Theme::paper`]/[`Theme::on_accent`]), so it stays
/// readable on both light and dark terminals. Was named "terminal".
pub const AUTO: Theme = Theme {
    name: "auto",
    bg: None,
    fg: Color::Reset,
    heading: Color::Reset,
    // A readable slate (DarkGray was too dim on a dark terminal); the ▎ bar +
    // italic already set quotes apart, so it only needs to be legible.
    quote: Color::Rgb(0x9d, 0xa5, 0xb4),
    link: Color::Rgb(88, 160, 255),
    // Concrete slate, not DarkGray/ANSI: secondary text on this role (sidebar
    // counts, inactive tabs, section sub-headers, rules) was barely legible on a
    // dark terminal — the same reason `quote` left DarkGray above.
    muted: Color::Rgb(0x86, 0x8e, 0x9a),
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
