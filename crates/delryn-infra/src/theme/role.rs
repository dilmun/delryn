//! Semantic UI roles — the vocabulary the view layer paints in. A view never
//! names a `Color` or a flat [`Theme`] field directly; it asks for a [`Role`],
//! and the theme resolves it to a concrete `Style`. Centralising the mapping
//! here is what keeps "every heading is bold", "every quote is italic", "every
//! key hint is dim" in one place — and is the seam a theme later overrides to
//! restyle a single role without touching any call site.
//!
//! **Adding a themeable element = add one [`Role`] variant + one [`resolve`]
//! arm.** No edit to any individual theme — the theming analogue of the parser's
//! "one data entry" rule. See `docs/theming.md`.

use ratatui::style::{Color, Modifier};

use super::Theme;

/// A semantic role the UI paints, grouped by surface. Resolve one with
/// [`Theme::style`](super::Theme::style) (full `Style`) or
/// [`Theme::color`](super::Theme::color) (just the foreground).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    // ── Content (prose on the page) ──────────────────────────────────────
    /// Body text.
    Body,
    /// Section heading (bold).
    Heading,
    /// Block quote (italic).
    Quote,
    /// Hyperlink / cross-reference / citation anchor.
    Link,
    /// Inline `code` and the unhighlighted code-block fallback.
    Code,
    /// List markers, stars, flags, "converted" badges — accent ink on the page.
    Marker,
    /// Rules, faint separators, de-emphasised text, footnotes, the code gutter.
    Muted,
    /// Display equation (accented like a heading).
    Math,

    // ── Chrome (frames, titles, hints) ───────────────────────────────────
    /// Pane / popup border.
    Border,
    /// Border of the focused pane / active field.
    BorderFocus,
    /// Panel / overlay title (bold accent).
    Title,
    /// A bare accent foreground (counts, the active field, emphasis marks).
    Accent,
    /// Accent foreground, emphasised (bold).
    AccentStrong,
    /// A faint hint line (key legends outside the status bar) — muted + dim.
    Hint,

    // ── Selection & matches ──────────────────────────────────────────────
    /// Selected row / current item / status "pill" — ink on the accent (bold).
    Selection,
    /// A search-match highlight.
    Match,
    /// The link cursor — reverse video, theme-agnostic.
    Cursor,

    // ── Semantic ─────────────────────────────────────────────────────────
    /// Errors, invalid input, destructive actions.
    Danger,

    // ── Status bar ───────────────────────────────────────────────────────
    /// The status-row background (carries `status_fg` over `status_bg`).
    StatusBar,
    /// Status text.
    StatusText,
    /// Status text, emphasised (bold) — the primary state segment.
    StatusStrong,
    /// Status text, dimmed — secondary fields, key hints, separators.
    StatusDim,
}

/// A resolved role: a foreground (`None` = leave the underlying colour, used by
/// reverse-video roles), an optional background, and modifiers.
pub(super) struct Resolved {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub modifier: Modifier,
}

/// The default role map: every [`Role`] resolved against a theme's swatches.
/// This is the one place a role's colour + emphasis is decided.
pub(super) fn resolve(t: &Theme, role: Role) -> Resolved {
    use Role::*;
    let plain = |c: Color| Resolved {
        fg: Some(c),
        bg: None,
        modifier: Modifier::empty(),
    };
    let with = |c: Color, m: Modifier| Resolved {
        fg: Some(c),
        bg: None,
        modifier: m,
    };
    match role {
        Body => plain(t.fg),
        Heading => with(t.heading, Modifier::BOLD),
        Quote => with(t.quote, Modifier::ITALIC),
        Link => plain(t.link),
        Code => plain(t.code_fg),
        Marker => plain(t.marker),
        Muted => plain(t.muted),
        Math => plain(t.heading),

        Border => plain(t.muted),
        BorderFocus => plain(t.accent),
        Title => with(t.accent, Modifier::BOLD),
        Accent => plain(t.accent),
        AccentStrong => with(t.accent, Modifier::BOLD),
        Hint => with(t.muted, Modifier::DIM),

        Selection | Match => Resolved {
            fg: Some(t.on_accent()),
            bg: Some(t.accent),
            modifier: Modifier::BOLD,
        },
        Cursor => Resolved {
            fg: None,
            bg: None,
            modifier: Modifier::REVERSED | Modifier::BOLD,
        },

        Danger => plain(t.danger),

        StatusBar => Resolved {
            fg: Some(t.status_fg),
            bg: Some(t.status_bg),
            modifier: Modifier::empty(),
        },
        StatusText => plain(t.status_fg),
        StatusStrong => with(t.status_fg, Modifier::BOLD),
        StatusDim => with(t.status_fg, Modifier::DIM),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::DARK;

    #[test]
    fn content_roles_carry_their_emphasis() {
        let s = DARK.style(Role::Heading);
        assert_eq!(s.fg, Some(DARK.heading));
        assert!(s.add_modifier.contains(Modifier::BOLD));

        let q = DARK.style(Role::Quote);
        assert_eq!(q.fg, Some(DARK.quote));
        assert!(q.add_modifier.contains(Modifier::ITALIC));

        // Body is plain — no emphasis baked in.
        let b = DARK.style(Role::Body);
        assert_eq!(b.fg, Some(DARK.fg));
        assert!(b.add_modifier.is_empty());
    }

    #[test]
    fn selection_is_ink_on_the_accent() {
        let s = DARK.style(Role::Selection);
        assert_eq!(s.fg, Some(DARK.on_accent()));
        assert_eq!(s.bg, Some(DARK.accent));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn cursor_is_reverse_video_with_no_own_colour() {
        let s = DARK.style(Role::Cursor);
        assert_eq!(s.fg, None);
        assert!(s.add_modifier.contains(Modifier::REVERSED));
        // color() of a colourless role falls back to the body colour.
        assert_eq!(DARK.color(Role::Cursor), DARK.fg);
    }

    #[test]
    fn status_bar_paints_fg_over_bg() {
        let s = DARK.style(Role::StatusBar);
        assert_eq!(s.fg, Some(DARK.status_fg));
        assert_eq!(s.bg, Some(DARK.status_bg));
    }

    #[test]
    fn color_returns_the_bare_foreground() {
        assert_eq!(DARK.color(Role::Accent), DARK.accent);
        assert_eq!(DARK.color(Role::Danger), DARK.danger);
        assert_eq!(DARK.color(Role::Border), DARK.muted);
    }
}
