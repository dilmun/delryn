# Theming (delryn-infra::theme)

delryn's theme is the **single source of truth** for every colour and emphasis the
app paints. Goal: *every* surface is themeable, themes are authorable as files,
and adding a new themeable element is one entry — not an edit to every theme.

## Three layers: Palette → Theme → Roles

```
  themes/*.toml          load.rs                    role.rs
 ┌──────────────┐   derive  ┌──────────────┐   map  ┌─────────────────────────┐
 │  [palette]   │ ───────►  │  Theme (flat │ ─────► │ Role::Heading → heading │
 │ bg text      │  missing  │   resolved   │        │              + BOLD     │──► theme.style(role) -> Style
 │ accent …     │  swatches │   swatches)  │        │ Role::Link    → link    │    theme.color(role) -> Color
 └──────────────┘           └──────────────┘        │ Role::Status* → status… │
   author input              the palette the          └─────────────────────────┘
                             role map draws from
```

1. **Palette** (`palette.rs`) — the named swatches a theme author fills in
   (`bg`, `text`, `accent`, `heading`, `quote`, `link`, `muted`, `marker`,
   `code`, `danger`, `status_fg`, `status_bg`). Only `bg`/`text` are required;
   the rest derive (see `load.rs`).
2. **`Theme`** (`mod.rs`) — the flat, fully-resolved swatches: a `Copy` struct of
   concrete `Color`s. This is the *resolved palette* the role map reads from. A
   swatch is a concrete sRGB **or** terminal-relative (`Color::Reset`, no `bg`)
   so the `terminal` theme keeps working with no colours of its own.
3. **Roles** (`role.rs`) — the semantic tokens the UI asks for. Each resolves to a
   palette swatch plus the emphasis that belongs to it (heading→bold,
   quote→italic, hint→dim). The view layer **only ever names a `Role`**; it never
   names a flat field or a literal `Color`.

### Roles (`theme/role.rs`)

```
Content:    Body Heading Quote Link Code Marker Muted Math
Chrome:     Border BorderFocus Title Accent AccentStrong Hint
Selection:  Selection Match Cursor
Semantic:   Danger
Status:     StatusBar StatusText StatusStrong StatusDim
```

`resolve(theme, role)` is the **one place** a role's colour + emphasis is decided.
A view calls `theme.style(Role::Heading)` (a full `Style`: fg + optional bg +
modifiers) or `theme.color(Role::Accent)` (just the fg, for the few sites that
compose their own border/gauge style).

**Adding a themeable element = add one `Role` variant + one `resolve` arm.** No
edit to any theme. This is the theming analogue of the parser's "one data entry"
rule.

There is **no literal `Color::` in `view/` or `app/`** — with one documented
exception: applying syntect's own highlight RGB to a code run in `view/reader.rs`.

### Resolution API (`theme/mod.rs`)

```rust
impl Theme {
    fn style(&self, role: Role) -> Style;   // fg + optional bg + modifiers
    fn color(&self, role: Role) -> Color;   // just the fg colour
    fn text_style(&self) -> Style;          // page fg (+ bg when the theme paints one)
    fn paper(&self) -> Color;               // concrete popup background
    fn code_surface(&self) -> Option<Color>;// faint code-block panel
    fn image_ink(&self) -> ([u8;3],[u8;3]); // (ink, paper) for figure recolour
}
```

## User themes — file-configurable (`theme/load.rs`)

Themes are TOML in `~/.config/delryn/themes/*.toml`, appended to the built-in
registry at runtime (built-ins first, then user themes, sorted by filename for a
stable cycle order). A name in `config.toml` resolves against both.

```toml
# ~/.config/delryn/themes/my-theme.toml
name = "my-theme"
syntect = "base16-ocean.dark"

[palette]
bg      = "#1e1e2e"
text    = "#cdd6f4"
accent  = "#89b4fa"
link    = "#89dceb"
# … the rest fall back to sensible derivations of bg/text/accent if omitted
```

Map / derive: `bg`/`text` are required; omitted swatches derive (e.g. `muted` =
`text` mixed halfway to `bg`; `link`/`marker` ← `accent`; `heading` ← `text`).
Names + syntect are leaked to `&'static str` once at startup, so `Theme` stays
`Copy` and every existing call site is untouched.

### Validation

A user theme whose body text barely separates from its page (luma diff `< 40` on
the 0–255 scale) is **dropped at load** — best-effort, the same as a malformed
file, so a broken palette can't render delryn unreadable. The check reuses the
shared `delryn_infra::color::luma()` (de-duplicated from `delryn-media` +
`delryn-infra`). Terminal-relative colours can't be measured, so they're trusted.
The image-ink resolver separately guarantees a readable `(ink, paper)` pair on
every theme.

## Module layout (`delryn-infra/src/theme/`)

| File | Responsibility |
|---|---|
| `mod.rs` | `Theme`, `style`/`color` role resolution, `paper`/`image_ink`/`code_surface`, `by_name`/`next`/`prev`, the loaded registry. |
| `palette.rs` | `Palette` (author swatches) + hex parsing. |
| `role.rs` | `Role` enum + the default role map (`resolve`). |
| `builtin.rs` | The built-in themes. |
| `load.rs` | Read `~/.config/delryn/themes/*.toml`, parse, derive, contrast-gate. |

## Future / deferred (the seam is in place)

- **Per-theme `[roles]` overrides** — letting a theme restyle an individual role
  (e.g. `heading = { color = "accent", bold = false }`) without touching call
  sites. The seam is `role::resolve`: give `Theme` an optional leaked role-table
  and consult it there before the default. Deferred — no demand yet, and it would
  pressure `Theme`'s `Copy`-ness; not worth the complexity until asked for.
- **Per-segment `status.*` roles + the `[status]` config block** — reorder/toggle
  status segments per zone. The status bar already paints in `Role::Status*`; the
  config block is the remaining piece (see `docs/status.md`).
