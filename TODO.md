# delryn — Roadmap

Living backlog. See `ARCHITECTURE.md` for the target structure and `DESIGN.md`
for the original spec. Phases are sequential; within a phase, items ship in
small green commits (build + `cargo test` + `cargo clippy` clean each step).

## Phase 0 — Foundation (`refactor/workspace`) — ✅ complete

Migrate to a Cargo workspace and clear every dev docs violation. Done: workspace
extracted into 8 crates; the `app` god-object and every god-file (store, epub,
library/meta_edit views) split into focused modules; let-chains modernization;
`cargo clippy` 0-warning workspace-wide. Every multi-concern file — including the
editor and reader view-models and the App-core dispatch — is split by concern
(modularity is first-class, not gated on size). No file in the core logic exceeds
the size guidelines.

- [x] Workspace skeleton: root `[workspace]`, crate → `crates/delryn`.
- [x] Extract `delryn-model` (content/metadata/toc/math types + naming helpers).
- [x] Extract `delryn-infra` (paths + config + theme; task/cache/export later).
- [x] Extract `delryn-store` (SQLite). *Split by entity later.*
- [x] Extract `delryn-online` (metadata/cover lookup).
- [x] Extract `delryn-media` (image protocols/decode).
- [x] Extract `delryn-format` (Document trait + epub: read/extract/cover/html).
- [x] Extract `delryn-render` (layout + highlight; paginate/table later).
- [x] Extract `delryn-library` (scan/index; collections/query/dedup/stats later).
- [x] Modernize: let-chains (killed the 28 collapsible_if) + the 3 real clippy
      fixes → **0 warnings workspace-wide**.
- [x] `cargo fmt` the workspace (canonical Rust 1.96 style).
- [x] Reinstall path is now `cargo install --path crates/delryn`.
- [x] Split `app.rs` into `app/` submodules: `confirm`, `settings`, `mouse`,
      `rename`, `select`, `collections`, `editor`, `reader`, `library` (mod.rs
      5.6k → ~1.9k, of which ~1.0k is the inline test module; ~0.9k non-test App
      core: struct + constructors + lifecycle + `on_key`/`apply` dispatch + overlay
      key handlers). Each a green commit. Pattern: child-module `impl App`,
      cross-module methods `pub(crate)`, types re-exported from `mod.rs`; concern
      tests stay in mod.rs (shared `key`/`ctrl`/`code` helpers).
- [x] Split the App-core dispatch into `app/dispatch.rs`: `on_key`, the overlay
      key handlers (images/notes/annotations/search prompt), and `apply` — routing
      is its own concern (mod.rs non-test core ~900 → ~500).
- [x] Sub-split `app/editor.rs` (1172 → mod 699 + `editor/lookup.rs` 485): the
      online metadata/cover lookup + background execution as a child module.
- [x] Sub-split `app/reader.rs` (1044 → mod 601 + `images`/`sidebar`/`search`):
      image lifecycle, TOC sidebar, in-book search as child modules; core loop
      (decode/wrap/scroll/nav/history) stays in mod.rs.
- [x] Split oversized views: `view/library` (814 → library/ dir: grid/detail/
      sections/books/status) and `view/meta_edit` (619 → meta_edit/ dir: hits +
      online), each leaving render() + shared helpers in mod.rs.
- [x] Split `delryn-store` (1065 → ~590) by entity (books/progress/annotations/
      shelves/search submodules, each an `impl Store` block).
- [x] Split `delryn-format::epub` (997 → mod 575 + content_meta 437): carve the
      content-based metadata heuristics into `epub/content_meta.rs`.
- [x] Resolve the 4 `#[allow(too_many_arguments)]`: `view::reader::render_column`
      was under threshold (allow removed); `meta_edit::form_field` grouped its 5
      field-state args into a `FieldState` struct; the two `Store` row-writers
      (`upsert_book`/`update_book_meta`) keep a *documented* scoped suppression —
      their args are the `books` columns 1:1, so a param struct would only shadow
      the table (and would churn ~30 call sites). `cargo clippy` is 0-warning.

## Phase 1 — Technical content rendering

- [x] Rich `Block` model: `Math`/`Table`/`Callout`/`Footnote` variants + figure
      captions on `Image`; `CalloutKind` (label + `from_word` classifier). Layout
      renders all (centred Unicode math, aligned tables w/ header rule, bordered
      callouts/footnotes via a width-aware nested wrapper). Producers land per-
      feature below. *(Citation/CrossRef are inline — they arrive with footnotes.)*
- [x] Code blocks: syntect highlight, line numbers, wrap/h-scroll, copy, and
      next/prev navigation (`w`/`b`, "code N/M"). *Deferred: code-index overlay +
      export → unified jump-by-type / export pass.*
- [x] Callouts/admonitions (NOTE/TIP/WARNING/…), block quotes. Parser emits
      `Block::Callout` from class/`epub:type` tokens + aside-icon tables; rendered
      as a bordered, labelled box. Block quotes unchanged.
- [x] Tables: parse `<table>` → `Block::Table` (header from `<thead>`/all-`<th>`),
      rendered as aligned columns + header rule. *Deferred: h-scroll/viewer/nav.*
- [x] Footnotes + cross-references: `Span` carries an `Anchor` (link/footnote/
      cross-ref/citation); parser stamps `<a>` runs and lifts footnote defs into
      `Block::Footnote` (muted). *Deferred: ref→def jump/return/preview — needs a
      reader cursor (Phase 2).*
- [x] Math: inline + display, from both **LaTeX** and **MathML** (incl. OOXML/
      DOCX-converted EPUBs that bury MathML in an `<img alt>`). `delryn-format::
      mathml` transcodes MathML→LaTeX-ish, then reuses `latex_to_unicode`
      (∑ᵢ₌₁ⁿ i², fractions, roots, scripts). `LineKind::Math` for display.
      Alt-less inline images show a quiet ▢. *Deferred: next/prev + index.*
- [x] **Equation/line-art images render legibly** (`delryn-media`): publishers
      ship equations as black-ink-on-transparent PNGs that were black-on-black on
      a dark reader. Now classified as line-art vs photo by colourfulness +
      sparsity (no per-publisher rules) and repainted as an ink-coverage matte in
      the theme's colours (alpha or luminance matte); photos keep their colours,
      transparency flattened onto the page. Re-tints on theme change. *(Re-test
      the two-page-view "some images don't show" now that visibility is fixed.)*

Phase 1 content model is complete end-to-end (parse → rich `Block` → render).
The deferred bits are all *interactive navigation*, gathered into Phase 2's
jump-by-type + a reader cursor.

## Phase 2 — Reading experience

- [ ] Reading modes: Continuous, Page, Chapter, Focus, Study, Research, Presentation.
      *(Have: Center/Fill/TwoPage view modes, focus mode, chapter-lock.)*
- [ ] Pagination models: continuous / virtual pages / book pages (PDF) / reflowed.
- [~] Navigation: reading history + back/forward (pre-existing) and **jump-by-type**
      done — `w`/`b` cycle code/table/math/figure/footnote ("kind N/M"). *Still:
      a reader cursor for footnote ref→def jump/return + cross-ref/citation jump.*
- [ ] Bookmarks: named, quick, folders. *(Have: bookmark/note annotations + overlay.)*

## Phase 3 — Library platform

- [ ] Views: Table (sortable columns), Cover Wall; refine Grid/List.
- [x] Smart collections + **filter DSL**: `delryn-library::query` — fields
      (title/author/series/publisher/language/isbn, year/progress/rating numeric),
      flags (favorite/converted/unread/reading/finished), AND/OR/NOT, parens,
      quoted values; the `/` filter uses it (plain queries keep substring + FTS).
- [~] Reading status + **rating** (0–5 ★, keys 0–5, detail stars, sort, DSL
      `rating>=4`). *Still: a manual reading-status enum (Paused/Dropped/Reference)
      beyond the progress-derived unread/reading/finished.*
- [x] Duplicate detection: `delryn-library::dedup` (ISBN, else normalized
      title + author surname); a "Duplicates" library section lists members.
      *Still: explicit merge/keep/remove resolution UI.*
- [ ] Metadata diff view (current vs remote, selective apply).

## Phase 4 — Knowledge & power tools

- [~] Highlights/notes/tags/backlinks. *(Have: bookmark + note annotations with
      an overlay.) Still: highlight colors, selection/page-anchored notes, tags
      (needs a `tags` table + `BookRow.tags` + DSL `tag:` wire-up), backlinks.*
- [x] Statistics: `delryn-library::stats` + overlay (`i`) — totals, status mix,
      ratings, reading hours, top authors.
- [x] Export: `delryn-library::export` (`X`) — book list → CSV / JSON / Markdown.
- [x] Command palette (`:`): `delryn-library::fuzzy` matcher + `app/palette.rs` —
      jump to section/collection, sort, cycle layout, toggle panes, stats, export.

## Theming & content coherence

Goal: the active theme is the **single source of truth** that colourises every
part of the app, and all content (ink vs. pictures) blends with it. Two rules —
*ink* (text, symbols, line-art equations/diagrams, icons, marks) is painted in
theme ink/paper; *pictures* (photos, colour charts, covers) are never recoloured,
only framed/matted to sit in the theme.

- [x] **`Theme` = single source of truth** (`delryn-infra::theme`): `text_style()`,
      `paper()`, `on_accent()`, `image_ink()` + `danger` colour; removed all
      hardcoded `Color::Black/Red` fallbacks and the 5 duplicated `base()` helpers.
- [x] Equation/line-art images recoloured to theme (alpha/luminance matte); photos
      kept faithful (flattened onto paper). *(shipped earlier)*
- [ ] **Image policy + smart-invert option** (the next piece): a setting with
      modes — **Auto** (current: recolour ink, keep pictures), **Invert
      backgrounds** (lightness-invert opaque light-bg figures so white-bg charts
      go dark with detail kept; leave true photos), **Faithful** (never touch).
      Use **lightness inversion (flip L in HSL/Lab, keep hue+sat)**, NOT naive
      `255−RGB` (which negates photos). Scope by the existing ink/picture
      classifier; covers always faithful. Optional reader keybind to flip the
      current view.
- [ ] **Code-block blending:** background = reader paper, syntax palette tuned to
      sit on it (kills the mismatched-rectangle look). Biggest remaining gap.
- [ ] **Figure framing:** consistent themed border + padding (and optional soft
      scrim on very dark themes) so pictures read as intentional cards.
- [ ] **Icons → themed glyphs:** extend callout-icon handling — map common
      publisher icons (note/tip/warning/✓/•) to Unicode glyphs in theme colours.

## Phase 5 — Formats

- [x] **Format recognition foundation:** `delryn-format::BookFormat` classifies
      files by extension (`is_readable`, `label`). The scanner indexes every
      recognized format (EPUB → full metadata; PDF/MOBI/AZW3 → by filename, so
      they show in the library now, badged); opening a non-EPUB reports cleanly
      on the status row. This is the seam the backends below plug into.
- [ ] **PDF** (`delryn-format::pdf`, behind a `Document` impl):
      - Crate: evaluate `lopdf` (pure-Rust, low-level: page tree + content
        streams + font maps — most control, most work) vs `pdf-extract` (text
        out of the box, heavier, less layout fidelity). Lean `lopdf` for the
        page model + a thin text-extraction layer so figures/positions stay
        reachable later.
      - Page model: one `Section` per page (or per outline entry if present);
        map the PDF outline → `TocEntry`/`OutlineItem`.
      - Text: extract text runs with positions; group into `Block::Para` by
        line/column gaps; detect headings by font size; preserve reading order
        across columns (sort by column then y).
      - Figures: extract `XObject` images → `Block::Image` (reuse the existing
        image pipeline). Math stays rasterized (no MathML in PDF).
      - Pagination: PDF is inherently paged — honor "book pages" mode (Phase 2).
      - **Validation:** needs real PDFs + a graphics-capable terminal; build
        against a corpus of varied PDFs (single/multi-column, scanned-vs-text,
        with/without outline) before shipping. Unit-test the text-grouping and
        outline-mapping logic on small fixtures independent of rendering.
- [ ] **MOBI / AZW3**: parse the PalmDB/MOBI header + record stream; KF8 (AZW3)
      is essentially zipped XHTML — reuse the existing `html` → `Block` pipeline
      once records are decompressed (PalmDOC/HUFF-CDIC). Evaluate `mobi` crate
      vs a minimal in-house reader. Same validation discipline as PDF.

## Phase 6 — Graphical math + deep performance

- [ ] **Graphical math**: render LaTeX/MathML → image (typst or a TeX→dvipng/
      MathJax-node shell-out) → terminal graphics (the existing ratatui-image
      pipeline), cached by content hash on disk. Fall back to the current
      Unicode rendering when no renderer/graphics protocol is available. *(Needs
      a graphics-capable terminal to validate; design the cache + fallback seam
      so the Unicode path is never regressed.)*
- [ ] **Deep performance** (measure first — profile before optimizing):
      - Virtualized scrolling: wrap only the visible window + neighbors (some
        background pre-wrap exists via `SectionLoader`); cap retained wrapped
        lines for very long chapters.
      - Incremental parsing: parse sections lazily on demand (mostly so today
        via `load_section`); add an LRU of parsed `Section`s.
      - Caching at scale: persist wrapped-layout + cover thumbnails keyed by
        (path, mtime, width, theme) so re-opens are instant.
