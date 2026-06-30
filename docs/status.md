# Status bar (view/status)

The bottom status bar is a first-class, **configurable, themeable** subsystem —
not ad-hoc strings scattered across the views. This document specifies the
redesigned status bar (redesign Phase R-C).

## Why the redesign

Today the status row is:
- **Split across two renderers** — `view/status.rs::bar` (library + overlay
  footer) *and* a separate ~88-line `view/reader.rs::render_status` with a
  different layout — so the two never quite match.
- **Coupled to the App god-object** — `legend(app)` is one long `if let` cascade
  over the overlay fields, hardcoding each overlay's context + key hints inline.
- **Not configurable** — content, order, and presence are fixed in code.
- **Coarsely themed** — only `status_fg`/`status_bg` (two colours total).

## Model: zones + segments

The bar is a list of **segments**, each placed in one of three **zones**, each
themed by a `status.*` role.

```
┌──────────────────────────────────────────────────────────────────────────┐
│ [Left zone]                  [Center zone]                  [Right zone]   │
│  mode pill · title            chapter / context        position · progress │
└──────────────────────────────────────────────────────────────────────────┘
```

```rust
enum Zone { Left, Center, Right }

struct Segment {
    id: SegmentId,
    spans: Vec<Span>,   // already themed via Role lookups
    zone: Zone,
    priority: u8,       // higher = dropped last when the row is narrow
}

enum SegmentId {
    Mode,        // a "pill": READER / LIBRARY / SEARCH / preset name, on status.mode_*
    Title,       // book / section title (reader) or view name (library)
    Chapter,     // current chapter label
    Position,    // "p 12/340" (page) or "12%" line position
    Progress,    // a slim unicode progress bar themed by status.progress
    Format,      // EPUB / PDF badge
    Counts,      // library: N books, M selected; reader: search "3/17"
    JumpType,    // active jump-by-type indicator (code/table/math/figure)
    Message,     // transient flash (e.g. "copied", "cover embedded")
    Legend,      // contextual key hints (the former `legend` cascade)
    Clock,       // optional
}
```

## Producers, not a god-cascade

Each context contributes its own segments — input/state stays decoupled from the
renderer:

- **Reader** emits reading segments (mode pill, title, chapter, position,
  progress, jump-type, search counts) from `Reader` state.
- **Library** emits library segments (view name, counts, filter, sort).
- **The active overlay** emits its own context label + key legend. This composes
  with the `enum Overlay` from R-A: each variant implements
  `fn status(&self) -> Vec<Segment>` (or returns its `(context, keys)`), so the
  hints live next to the overlay, not in a central `if let` chain.
- A **modal** (`pending_confirm`) overrides the bar entirely while open.

## Overflow: priority elision

When the terminal is narrow, segments are dropped by ascending `priority` until
the row fits (today's code just truncates). A shared `fit(width, segments)`
packs each zone and elides low-priority segments first, so the mode pill +
position survive on a tiny terminal while the legend drops.

## Configurable (`[status]` in config)

A new config block (single-source `Config`, per R-B) controls the bar:

```toml
[status]
left    = ["mode", "title"]
center  = ["chapter"]
right   = ["position", "progress"]
progress_bar = true       # slim unicode bar vs plain percentage
clock        = false
separator    = "·"
style        = "pill"     # "pill" (powerline-ish) | "plain"
```

Unknown/omitted keys fall back to defaults (serde `#[serde(default)]`). Users can
reorder, hide, or add segments per zone.

## Theming

Every segment draws through `status.*` roles (see `docs/theming.md`): `bar_bg`,
`bar_fg`, `mode_fg`/`mode_bg` (the pill), `segment_fg`/`segment_bg`, `separator`,
`progress`/`progress_track`, `accent`, `key`/`key_dim` (the legend's active vs
dimmed hint text). No literal colours.

## Module layout (`delryn/src/view/status/`)

| File | Responsibility |
|---|---|
| `mod.rs` | The single `render(frame, area, bar: &StatusBar, theme)` — the one renderer for reader, library, and overlays. Replaces `view/status.rs` and `view/reader.rs::render_status`. |
| `segment.rs` | `Segment`, `Zone`, `SegmentId`, `StatusBar`. |
| `producers.rs` | Build segments from `Reader` / library state / the active `Overlay`. |
| `layout.rs` | Zone packing + priority-based overflow (`fit`). |

## Outcome

One renderer, one model, decoupled producers, per-segment theming, user
configuration, graceful overflow — and the reader/library/overlay bars finally
look and behave consistently.
