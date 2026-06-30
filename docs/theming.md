# Theming (delryn-infra::theme)

delryn's theme is the **single source of truth** for every colour and emphasis the
app paints. This document specifies the redesigned, user-extensible theming
system (redesign Phase R-C). Goal: *every* surface is themeable, themes are
authorable as files, and adding a new themeable element is one entry — not an
edit to every theme.

## Two layers: Palette → Roles

A theme is **not** a flat list of UI colours. It is a small **palette** of named
swatches plus a **role map** that assigns each semantic UI role to a palette
entry (with optional modifiers). The UI only ever names a *role*; it never names
a palette colour or a literal `Color`.

```
            ┌─────────── Palette (≈16 swatches) ───────────┐
theme.toml  │ bg surface overlay  text muted subtle bright │
            │ accent accent2  red green yellow blue …       │
            └───────────────────────┬──────────────────────┘
                                     │ RoleMap (defaults + per-theme overrides)
                     ┌───────────────┴───────────────┐
   Role::Heading ───►│ heading → bright (bold)        │
   Role::Link    ───►│ link    → blue                 │──► theme.style(role) -> Style
   Role::Selection ─►│ selection → accent (on=bg)     │    theme.color(role) -> Color
   Role::Status... ─►│ status.bar.bg → surface …      │
                     └───────────────────────────────┘
```

### Palette (`theme/palette.rs`)

~16 named swatches, the vocabulary a theme author fills in. Modeled on base16 /
terminal palettes so it's familiar:

```
bg surface overlay          # backgrounds: page, raised panel, popup
text muted subtle bright    # foregrounds: body, de-emphasised, faint, emphasised
accent accent2              # primary/secondary accents
red green yellow blue magenta cyan orange   # semantic + syntax anchors
```

A swatch is a concrete sRGB (`Color::Rgb`) **or** the sentinel `terminal`
(meaning "use the terminal's own fg/bg / an ANSI index") so the `terminal` theme
keeps working with no colours of its own.

### Roles (`theme/role.rs`)

The semantic tokens the UI asks for. Each resolves to a palette swatch plus
optional `Modifier`s (bold/italic/dim). The default `RoleMap` maps every role;
a theme overrides only the roles it wants to change.

```
Content:   body heading subheading quote link code code_surface marker rule
Chrome:    border border_focus scrollbar gutter title pane_bg overlay_bg
Selection: selection selection_fg cursor match match_current
Marks:     bookmark bookmark_named footnote citation crossref
Semantic:  danger warning success info
Status:    status.bar_bg status.bar_fg status.mode_fg status.mode_bg
           status.segment_fg status.segment_bg status.separator
           status.progress status.progress_track status.accent status.key status.key_dim
Images:    ink paper   (the recolour pair for monochrome/line-art figures)
```

**Adding a themeable element = add one `Role` variant + one default-map entry.**
No edit to any theme file. This is the theming analogue of the parser's
"one data entry" rule.

### Resolution API (`theme/mod.rs`)

```rust
impl Theme {
    fn style(&self, role: Role) -> Style;   // fg + optional bg + modifiers
    fn color(&self, role: Role) -> Color;   // just the fg colour
    fn paper(&self) -> Color;               // concrete popup background
    fn image_ink(&self) -> ([u8;3],[u8;3]); // (ink, paper) for figure recolour
    fn syntect(&self) -> &str;              // coordinated code-highlight theme
}
```

The view layer calls `theme.style(Role::Heading)` etc. — replacing today's direct
field reads (`theme.heading`) and the scattered inline `Modifier::*` calls. There
must be **no literal `Color::` in `view/` or `app/`** (today there is exactly one
legitimate exception — applying syntect's own highlight RGB in `view/reader.rs`).

## User themes — file-configurable (`theme/load.rs`)

Themes are TOML, loaded from `~/.config/delryn/themes/*.toml`. Built-in themes ship
in the **same format** (embedded via `include_str!`), so built-in and custom are
uniform and a user can copy a built-in to start.

```toml
# ~/.config/delryn/themes/my-theme.toml
name = "my-theme"
syntect = "base16-ocean.dark"

[palette]
bg      = "#1e1e2e"
surface = "#181825"
text    = "#cdd6f4"
muted   = "#6c7086"
accent  = "#89b4fa"
blue    = "#89dceb"
# … the rest fall back to sensible derivations of bg/text/accent if omitted

[roles]            # optional — override individual roles
heading = { color = "bright", bold = true }
link    = "blue"
"status.bar_bg" = "surface"
```

Load order / merge: **role defaults ← theme palette ← theme role overrides.**
Missing palette swatches are derived (e.g. `surface` = `bg` nudged toward `text`,
the existing `code_surface` trick generalized). A theme that fails to parse or
fails the contrast check is skipped with a logged reason, never crashes.

### Validation

The existing image-ink contrast test generalizes into load-time validation: every
theme must yield a readable `(ink, paper)` pair and a legible `body`-on-`bg`. A
shared `luma()` (de-duplicated from `delryn-media` + `delryn-infra`, per audit R-D)
backs the contrast maths and the magic-number fallbacks are removed.

## Module layout (`delryn-infra/src/theme/`)

| File | Responsibility |
|---|---|
| `mod.rs` | `Theme`, `Role` resolution (`style`/`color`), `by_name`/`next`/`prev`, the registry of loaded themes. |
| `palette.rs` | `Palette` (the ≈16 swatches) + swatch parsing + derivations. |
| `role.rs` | `Role` enum + the default `RoleMap`. |
| `builtin.rs` | The built-in themes, embedded in the TOML format. |
| `load.rs` | Read `~/.config/delryn/themes/*.toml`, parse, merge, validate. |
| `image.rs` | `image_ink`/`paper` resolution + shared `luma()`. |

## Migration / compatibility

The current public surface (`Theme`, `by_name`, `next`/`prev`, `text_style`,
`paper`, `image_ink`, `code_surface`, the `THEMES` list) is preserved, re-expressed
over roles, so the rest of the app keeps compiling while call sites migrate to
`theme.style(Role::…)`. The persisted `theme` name in `config.toml` is unchanged;
a name now also resolves against user theme files.
