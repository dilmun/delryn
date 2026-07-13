//! Colour themes — the single source of truth for every colour the app paints.
//! Each theme maps semantic roles (body, heading, quote, link, code, markers,
//! status bar, errors) to colours, names a syntect theme so prose and code stay
//! coordinated, and resolves the concrete `ink`/`paper` used to recolour images.
//! No renderer should hardcode a colour or a fallback — go through a `Theme`.
//! See `DESIGN.md` §7.

use std::sync::OnceLock;

use ratatui::style::{Color, Style};

use crate::color::luma;

/// The terminal's real background colour, queried once at startup (OSC 11). The
/// `auto` theme uses no colours of its own, so without this it would recolour
/// images (and paint overlays) against a fixed fallback; with it, its page,
/// equations, and inverted figures match the actual backdrop — light or dark.
static TERMINAL_BG: OnceLock<[u8; 3]> = OnceLock::new();

/// Record the detected terminal background (best-effort; ignored if already set).
pub fn set_terminal_background(rgb: [u8; 3]) {
    let _ = TERMINAL_BG.set(rgb);
}

fn terminal_background() -> Option<[u8; 3]> {
    TERMINAL_BG.get().copied()
}

mod builtin;
mod load;
mod palette;
mod role;

pub use builtin::*;
pub use role::Role;

/// All available themes — the compiled-in built-ins plus any user themes found in
/// [`crate::paths::themes_dir`], loaded once on first access (built-ins first,
/// then user themes). Backs [`by_name`] and the [`Theme::next`]/[`Theme::prev`]
/// cycle, so a dropped-in theme file participates everywhere a theme is chosen.
fn registry() -> &'static [Theme] {
    static REGISTRY: OnceLock<Vec<Theme>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut all: Vec<Theme> = builtin::BUILTINS.to_vec();
        all.extend(load::load_user_themes());
        all
    })
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
        let all = registry();
        let i = all.iter().position(|t| t.name == self.name).unwrap_or(0);
        all[(i + 1) % all.len()]
    }

    /// The previous theme in the cycle.
    pub fn prev(&self) -> Theme {
        let all = registry();
        let i = all.iter().position(|t| t.name == self.name).unwrap_or(0);
        all[(i + all.len() - 1) % all.len()]
    }

    /// Resolve a semantic [`Role`] to a concrete `Style` (foreground, optional
    /// background, and modifiers). The view layer paints in roles — `theme.style
    /// (Role::Heading)` — never in raw fields, so emphasis and colour decisions
    /// live in one place ([`role`]).
    pub fn style(&self, role: Role) -> Style {
        let r = role::resolve(self, role);
        let mut s = Style::default().add_modifier(r.modifier);
        if let Some(fg) = r.fg {
            s = s.fg(fg);
        }
        if let Some(bg) = r.bg {
            s = s.bg(bg);
        }
        s
    }

    /// The foreground colour of a [`Role`], for the few sites that compose their
    /// own `Style` (border colours, gauges). Reverse-video roles (no own
    /// foreground) fall back to the body colour.
    pub fn color(&self, role: Role) -> Color {
        role::resolve(self, role).fg.unwrap_or(self.fg)
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
    /// over content. The `auto` theme (no `bg` of its own) uses the **detected**
    /// terminal background so overlays match the terminal — light or dark — instead
    /// of a fixed black that renders muddy on a light terminal; it falls back to
    /// black only when the real background is unknown (no detection).
    pub fn paper(&self) -> Color {
        match self.bg {
            Some(bg) => bg,
            None => terminal_background()
                .map(|[r, g, b]| Color::Rgb(r, g, b))
                .unwrap_or(Color::Black),
        }
    }

    /// The foreground for text drawn *on* the `accent` colour (selections, pills,
    /// highlighted rows). For a theme with its own page it's the inverse of the
    /// page; for the `auto` theme (whose page follows the terminal and so can be
    /// light or dark) it's picked from the accent's own luminance, so a selection
    /// stays legible whatever the terminal background is.
    pub fn on_accent(&self) -> Color {
        match self.bg {
            Some(_) => self.paper(),
            None => match rgb_of(self.accent) {
                Some(a) if luma(a) < 128.0 => Color::Rgb(235, 235, 235),
                _ => Color::Rgb(20, 20, 20),
            },
        }
    }

    /// A faint panel colour for code blocks — the page nudged ~8% toward the text
    /// colour, so code reads as a distinct surface that still matches the theme.
    /// `None` for the `terminal` theme when its real background is unknown (no
    /// detection), so code keeps rendering on the terminal's own backdrop.
    pub fn code_surface(&self) -> Option<Color> {
        let paper = self.bg.and_then(rgb_of).or_else(terminal_background)?;
        let ink = rgb_of(self.fg).unwrap_or(if luma(paper) < 128.0 {
            [235, 235, 235]
        } else {
            [20, 20, 20]
        });
        let mix = |a: u8, b: u8| (a as f32 * 0.92 + b as f32 * 0.08).round() as u8;
        Some(Color::Rgb(
            mix(paper[0], ink[0]),
            mix(paper[1], ink[1]),
            mix(paper[2], ink[2]),
        ))
    }

    /// The ink for inline math: body text nudged ~30% toward the heading/math
    /// accent, so an inline equation reads as subtly distinct from the prose without
    /// shouting (paired with italics in the view — see [`Role::MathInline`]). `None`
    /// when the theme's `fg`/`heading` aren't concrete RGB (e.g. the `terminal`/
    /// `auto` themes), so inline math then falls back to plain italic body text.
    pub fn math_ink(&self) -> Option<Color> {
        let fg = rgb_of(self.fg)?;
        let accent = rgb_of(self.heading)?;
        let mix = |a: u8, b: u8| (f32::from(a) * 0.70 + f32::from(b) * 0.30).round() as u8;
        Some(Color::Rgb(
            mix(fg[0], accent[0]),
            mix(fg[1], accent[1]),
            mix(fg[2], accent[2]),
        ))
    }

    /// The syntect theme name to highlight code with. A concrete-background theme
    /// trusts its configured pairing; the adaptive `auto` theme (no `bg` of its
    /// own) picks a light syntect theme when the **detected** terminal background
    /// is light, so code colours match the backdrop instead of forcing dark.
    pub fn code_syntect(&self) -> &'static str {
        if self.bg.is_none() && terminal_background().is_some_and(|bg| luma(bg) >= 128.0) {
            return "InspiredGitHub";
        }
        self.syntect
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
        if (luma(ink) - luma(paper)).abs() < 64.0 {
            let opposite = if luma(paper) < 128.0 {
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

/// Look a theme up by name (for persistence). `"terminal"` is accepted as a
/// back-compat alias for the renamed `"auto"` theme so older configs still load.
pub fn by_name(name: &str) -> Option<Theme> {
    let name = if name == "terminal" { "auto" } else { name };
    registry().iter().copied().find(|t| t.name == name)
}

pub fn default_theme() -> Theme {
    builtin::AUTO
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_is_concrete_even_for_terminal_theme() {
        // The terminal theme has no bg of its own, but popups need a concrete one.
        assert_eq!(AUTO.paper(), Color::Black);
        assert_eq!(OLED.paper(), Color::Rgb(0, 0, 0));
        assert_eq!(LIGHT.paper(), Color::Rgb(0xff, 0xff, 0xff));
    }

    #[test]
    fn every_theme_resolves_a_readable_image_ink_pair() {
        // ink and paper must always differ enough to read an equation against.
        for t in BUILTINS {
            let (ink, paper) = t.image_ink();
            assert!(
                (luma(ink) - luma(paper)).abs() >= 64.0,
                "{} ink/paper too close: {ink:?} vs {paper:?}",
                t.name
            );
        }
    }

    #[test]
    fn auto_theme_selection_pill_is_legible() {
        // Number badges / selected rows paint with `Role::Selection` (accent bg +
        // `on_accent` fg). The auto theme's `heading` is `Reset`, so a pill keyed to
        // it vanishes on the terminal's own backdrop; keyed to the concrete accent it
        // must stay opaque and contrasting on any terminal.
        let s = AUTO.style(Role::Selection);
        let fg =
            s.fg.and_then(rgb_of)
                .expect("auto selection fg is concrete");
        let bg =
            s.bg.and_then(rgb_of)
                .expect("auto selection bg is the concrete accent, not Reset");
        assert!(
            (luma(fg) - luma(bg)).abs() >= 64.0,
            "auto selection pill must contrast: {fg:?} on {bg:?}"
        );
    }

    #[test]
    fn terminal_theme_inks_black_on_white() {
        // Reset fg + no bg, no detected terminal colour → the publisher's intended
        // dark-on-light page.
        assert_eq!(AUTO.resolve_image_ink(None), ([0, 0, 0], [255, 255, 255]));
    }

    #[test]
    fn terminal_theme_uses_detected_background() {
        // With a detected dark terminal background, the page becomes that colour
        // and the ink is snapped light for contrast.
        let (ink, paper) = AUTO.resolve_image_ink(Some([20, 22, 26]));
        assert_eq!(paper, [20, 22, 26], "page = real terminal bg");
        assert!(luma(ink) > 128.0, "ink snapped light on a dark bg: {ink:?}");
        // A concrete theme ignores the terminal fallback.
        assert_eq!(OLED.resolve_image_ink(Some([20, 22, 26])).1, [0, 0, 0]);
    }

    #[test]
    fn code_surface_is_a_faint_shift_from_the_page() {
        // A concrete dark theme: surface is slightly lighter than the page.
        let lum = |c: Color| rgb_of(c).map(luma).unwrap_or(0.0);
        let surface = OLED.code_surface().expect("concrete theme has a surface");
        assert!(
            lum(surface) > 0.0 && lum(surface) < 40.0,
            "subtle lift off black: {surface:?}"
        );
        // The terminal theme without a detected background has no surface (code
        // keeps rendering on the terminal's own backdrop).
        assert_eq!(AUTO.code_surface(), None);
    }

    #[test]
    fn rgb_of_resolves_concrete_but_not_terminal_relative() {
        assert_eq!(rgb_of(Color::Rgb(1, 2, 3)), Some([1, 2, 3]));
        assert_eq!(rgb_of(Color::Black), Some([0, 0, 0]));
        assert_eq!(rgb_of(Color::Reset), None);
        assert_eq!(rgb_of(Color::Indexed(5)), None);
    }
}
