# Status bar (`view/status`)

The bottom status row is a first-class, configurable, themeable subsystem rather
than ad-hoc strings scattered across the views. This describes what ships.

Code lives in `crates/delryn/src/view/status/`:

| File | Role |
| ---- | ---- |
| `segment.rs` | The `Zone` / `SegmentId` / `Segment` / `StatusBar` model |
| `producers.rs` | Each context builds its own segments (`reader_bar`, `library_bar`, `overlay_bar`) |
| `render.rs` | Zone layout, ordering, priority-based dropping, truncation |
| `clock.rs` | The optional wall-clock segment |

## Model: zones and segments

A bar is a list of **segments**, each assigned to one of three **zones** and
styled through the theme's `status.*` roles.

```
┌────────────────────────────────────────────────────────────────────────────┐
│ [Left]                        [Center]                            [Right]  │
│  book title / context          optional centred context   reading fields   │
└────────────────────────────────────────────────────────────────────────────┘
```

```rust
enum Zone { Left, Center, Right }

enum SegmentId {
    Context,     // book title / author, or the library / overlay label
    Flash,       // transient message ("copied", "cover embedded", …)
    Search,      // match counter (⌕ 3/17)
    Theme,       // active theme name
    View,        // view-mode label (single / two-page / …)
    Continuous,  // continuous-scroll indicator
    Manga,       // right-to-left indicator
    Page,        // page counter (p 12/340)
    Zoom,        // zoom / fit label (PDF)
    Position,    // section position (12/31)
    Percent,     // reading percent (23%)
    Gauge,       // slim unicode progress gauge
    Clock,       // wall clock (14:05)
    Keys,        // contextual key hints
}
```

Each `Segment` carries its id, its zone, pre-themed spans, and a priority —
higher priority survives longer when the row is too narrow.

## Producers, not one god-cascade

Every context contributes its own segments, so input and state stay decoupled
from the renderer:

- **`reader_bar`** — title/author, reading fields (position, percent, gauge,
  page, zoom), and the mode indicators (view, continuous, manga).
- **`library_bar`** — the library context label, counts, and selection state.
- **`overlay_bar`** — an open overlay's own label and key hints; `None` when no
  overlay wants the row.

The key hints that used to be one long `if let` cascade over the overlay fields
are now the `Keys` segment, produced by whichever context owns the screen.

## Rendering

`render` lays the three zones out in one row, then narrows gracefully:

1. Order each zone. A zone's order comes from the `[status]` config when the
   user has listed one, otherwise from the built-in order — so config only ever
   *reorders*; hiding a segment is its own toggle.
2. Drop the lowest-priority segments first while the row overflows.
3. Truncate what remains — the book title truncates at the **middle**, so a long
   title keeps both its start and its end rather than overrunning the reading
   fields to its right.

The bar floats on the page rather than sitting in a filled band: there is no
`status_bg`, and the ink grades against the page colour through the theme roles.

## Configuration

The `[status]` block in `config.toml`:

| Key | Meaning |
| --- | ------- |
| `theme`, `view`, `position`, `percent`, `gauge`, `clock` | Per-segment on/off |
| `separator` | Divider drawn between segments in a zone (a space each side) |
| `left`, `center`, `right` | Explicit segment order per zone, by `SegmentId` label |

Zone lists name segments in lowercase (`"position"`, `"percent"`, `"gauge"`,
`"page"`, `"zoom"`, `"search"`, `"theme"`, `"view"`, `"continuous"`, `"manga"`,
`"clock"`). Anything unlisted keeps its built-in position.
