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
- [x] **Consistent figure sizing across books**: pixel resolution is an authoring
      artifact, not the intended size, so figures used to be wildly inconsistent
      (a low-res screenshot tiny next to a high-res chart). The parser now keeps
      the authored width (`<img>` width / inline CSS `width`, `%`/px) on
      `Block::Image` as `ImageWidth`; `delryn-media::target_cells` sizes figures to
      a consistent display width — authored width when known, else `image_width_pct`
      (Settings → Content "Figure width %", default 85%) — enlarging low-res
      figures up to a bounded `MAX_UPSCALE` (2.5×, so tiny icons aren't blown up)
      but never past the column/viewport. Equation images keep native size.

Phase 1 content model is complete end-to-end (parse → rich `Block` → render).
The deferred bits are all *interactive navigation*, gathered into Phase 2's
jump-by-type + a reader cursor.

## Phase 2 — Reading experience

- [x] Reading modes: **Continuous** + **Page** (paged scroll, `p` / Settings →
      "Page mode"; vertical nav flips whole pages snapped to boundaries, status
      shows "p N/M"), Focus, chapter-lock, Center + Two-page layouts (shared
      `side_padding` % edge margin + configurable two-page `page_gap`), and
      **presets** — Study / Research / Presentation (`M` cycles, or Settings →
      Profile → "Reading mode"). Each bundles padding + spacing + sidebar +
      status + chapter-lock + paged (deliberately *not* view layout — that's the
      reader's choice); the active preset is *derived* from the live settings
      (shows "custom" once any are hand-tweaked, so it never lies).
- [x] Pagination models: continuous + **virtual pages** (page mode, snapped to
      `page_lines`, flows across chapters at edges) + **book pages** (PDF
      page-images, the direct-Kitty `PageDeck`).
- [x] **Text layout: justify + soft hyphens + spacing tidy** (`delryn-render::
      layout`). The greedy line filler now breaks long words at embedded soft
      hyphens (U+00AD dropped, a real `-` shown on break) and supports full
      **justification** (Settings → Typography "Justify text", default ragged-
      right; never the last line or single-word lines, body only). **Tidy spacing**
      (Settings → Content, default on) collapses a converter artifact — a stray
      space between a short styled variable and a hyphenated suffix (`<i>t</i>
      -distribution` → `t-distribution`, p-value, F-test) — verified against the
      raw EPUB; deliberately narrow, so numbers (`16. 3`), `p < 0.05`, dashes and
      prose are untouched. Both flow through `WrapOpts` so toggling re-wraps
      without re-parsing. *(The original `t -distribution` report was dirty source,
      not a delryn bug — the space is literally in the book's markup.)*
- [x] Navigation: reading history + back/forward, **jump-by-type** (`w`/`b` cycle
      code/table/math/figure/footnote), and a **link cursor** — `e`/`E` step
      through inline references (footnote/cross-ref/link/citation), Enter follows,
      Esc clears, Ctrl+o returns. Footnote ref→def (same section then cross-section
      endnotes); link→copy URL; **cross-ref/citation → their target** via a
      book-wide id→(section, locator) index (`Document::section_targets` +
      `html::collect_targets`), resolved to a line by the locator text.
- [x] Bookmarks (pure — no notes; notes are Phase 4): **quick** (`m` drops one at
      the cursor), **named** (`r` in the overlay sets a custom label shown instead
      of the quote), **folders** (`f` files an entry; the overlay groups by folder
      — named folders first, ungrouped last). Modern overlay (rounded frame, count
      badge, per-folder counts, hint footer) + a **left-gutter ribbon** marking
      bookmarked lines in the page margin (Center & TwoPage). `annotations` gains
      `name`/`folder`/`kind` columns (`kind` reserves notes for Phase 4) +
      migration. `--export-annotations` is folder-aware.
- [~] Image viewer (`i`): figure **sidebar** (real figures only — equation/math
      images excluded via `Block::Image.math`), large image **scaled + centered
      with equal padding** (faithful colours on a white page, no theme recolour),
      **details** (book chapter label + dimensions) + caption, **jump to the
      figure** (⏎ → `jump_to_image`), **filter** (`/`), **save** (`s`), chapter↔
      **whole-book** scope (`w`). *Still: zoom (fit↔actual) + pan (SlicedImage).*
- [x] **Responsive layout standard**: shared `view::sidebar_split` / `detail_split`
      (percentage width, clamped to cell bounds, collapse when the main pane would
      drop below a minimum — one `side_width` rule). Used by the reader TOC
      sidebar, the image viewer, and the library (sidebar + detail), whose panes
      are now percentage-based — `<`/`>` adjust the percentage so they scale with
      the window. Any future multi-pane view uses the same helpers.
- [x] **Tabbed settings overlay**: options grouped into tabs (`Tab`/`Shift-Tab`
      switch, wrapping) instead of one long list — Reader: Reading / Chrome /
      Content / Input; Library: View / Columns / General. Pill tab strip on the
      accent, section sub-headers within a tab, and the body scrolls (cursor
      centered, slim scrollbar on overflow) so options are always reachable on a
      short terminal. `↑↓` move, `←→` change.

## Phase 3 — Library platform

- [~] Views: Table (sortable columns), Cover Wall; refine Grid/List. *(Have:
      per-column show/hide (Settings → Columns: Author/Year/Type/Source/Progress/
      Size/Status), every column sortable, `s` cycles only the *visible* columns
      ascending↔descending, a `Type` column for the file format (replaced the old
      title badge). Still: a dedicated Cover Wall.)*
- [x] Smart collections + **filter DSL**: `delryn-library::query` — fields
      (title/author/series/publisher/language/isbn, year/progress/rating numeric),
      flags (favorite/converted/unread/reading/finished/paused/dropped/reference),
      AND/OR/NOT, parens, quoted values; the `/` filter uses it (plain queries
      keep substring + FTS).
- [x] Reading status + **rating** (0–5 ★, keys 0–5, detail stars, sort, DSL
      `rating>=4`). Manual reading-status enum (`delryn-model::ReadingStatus`:
      Paused/Dropped/Reference) beyond the progress-derived unread/reading/
      finished — `m` cycles it, shown as its own toggleable `Status` column +
      detail line, sortable, DSL-filterable.
- [x] Duplicate detection + resolution: `delryn-library::dedup`. **Cheap-to-expensive
      cascade.** Tier 1+2 (exact metadata, instant every refresh): each book emits a
      canonical ISBN-13 key (ISBN-10↔13 collapsed) *and* a normalized title key **per
      author** (title + each author's surname), joined by connected components
      (union-find). Matching on *any* shared key catches the common misses: a copy
      with no ISBN joins its ISBN-bearing twin; editions with different ISBNs meet on
      title+author; a multi-author book matches a copy crediting **any one** of its
      authors (any order/format). Title normalization trims subtitle/divider/paren
      tails (`main_title`), folds diacritics, strips trailing edition noise ("2nd ed")
      while keeping volume/part markers so series entries don't collapse. Tier 3 is
      the on-demand content scan (`R`, below); grouping **unions all tiers** (ISBN ∪
      title+author ∪ TOC links via union-find), so any one matching links a group and
      no tier's matches are lost. A "Duplicates" library section lists members. `D`
      opens a **resolution overlay** — every group with a checkbox per copy; a
      **smart auto-select** keeps the best (engagement > original > configured
      format keep-order > richer metadata > larger) and pre-checks the worse ones;
      `space` toggles, `a` re-auto, `u` clears, `n` **ignores the group** (stop
      flagging it — persisted in `dismissed_dups`), `I` opens the **ignored-groups
      manager** (list/restore one with `u`/⏎ or restore all with `C`), `p`
      **previews** the selected copy in the reader (q/Esc returns to the overlay —
      stashed in `dup_preview`), `r` **reveals** it in the OS file manager, `f`
      **full-screen** toggle, `o` opens the resolver's **preferences** (Library
      Settings → Duplicates: a "converted: always delete" rule + a per-format keep
      priority you reorder with l/h — `config.dup_converted_delete`/`dup_format_order`),
      `d` deletes all checked after one confirm. Rows are an **aligned table** under a
      fixed header (keep/delete · format · size · source · read-flags · path); the
      path keeps its directory and trims a long filename (whole path full-screen).
- [x] Thorough duplicate scan (user-triggered, off the default path): in the
      Duplicates view, `R` reads the **table of contents** of *every* book (so a
      content match can join copies metadata missed — grouping unions all tiers, no
      misses) from its own structure (NOT metadata): EPUB nav + PDF bookmark outline
      (`epub`/`pdf::toc_labels`;
      PDF's synthetic "Page N" fallback is rejected). Each chapter label is reduced
      to its distinctive part — leading "Chapter N"/"Part N" stripped, generic
      boilerplate ("Preface"/"Summary"/"Index"/…) dropped — and hashed into a set
      (`dedup::content_link_candidates`). Two books link when their distinctive-label
      sets share ≥4 titles at overlap-coefficient ≥0.6 (overlap, not Jaccard, so a
      finer-grained outline on one side still matches). Matching the chapter *list as
      a whole* is what stops topic-word collisions ("Artificial Intelligence and…")
      and publisher templates that broke every earlier attempt (cover+title; middle-
      text SimHash; front-text SimHash on Packt boilerplate; single-title token-set/
      prefix overlap). Links persist to `dup_links`, folded into grouping
      (`duplicate_groups_with_links`); covers every combo (epub↔epub, pdf↔pdf,
      pdf↔epub). Limits: a book with no real TOC (PDF without bookmarks, bare EPUB) is
      skipped; structural-only TOCs ("Chapter 1".."Chapter N") yield no fingerprint.
      Human-confirmed via overlay `n` ("keep both"). Tunables: TOC_MIN_LABELS,
      TOC_MIN_SHARED, TOC_OVERLAP_MIN.
- [ ] Dedup, further tiers (only if duplicates still slip through — all kept
      cheap/lazy, no library-wide content scan): **(2)** bounded fuzzy title
      fallback, blocked by author-surname / first title token to stay near-linear;
      **(3)** confidence tiers (Exact / Likely / Possible) shown + sorted in the
      overlay; **(4)** cached `content_hash` column (blake3 of file bytes,
      incremental behind the mtime/size check, rayon-parallel) for zero-false-
      positive "Exact Duplicate"; **(5)** lazy content fingerprint computed *only*
      for the few books in a group being resolved (reuse `read_fulltext`), never
      at scan time; **(6)** PDF↔EPUB content matching once the PDFium text layer is
      wired up (deferred in PDF v2).
- [x] Metadata diff view (current vs remote, selective apply): picking an online
      candidate (editor Lookup tab → ⏎) opens a diff overlay — one row per field
      with current vs remote, fields that differ pre-ticked; space toggles, `a`
      all, ⏎ applies the ticked rows into Details (+ fetches the cover), Esc
      cancels. Replaces the old apply-everything-then-review behaviour.

## Phase 4 — Knowledge & power tools

- [~] Highlights/notes/tags/backlinks. *(Have: bookmark + note annotations with
      an overlay; **tags** — free-form per-book labels: `books.tags` column +
      `BookRow.tags`, normalised via `delryn_model::tags` (lowercase/trim/dedup),
      edited with `T` (inline prompt; single replaces, multi-selection adds),
      shown in the detail pane + a toggleable sortable Tags column, filterable
      with `tag:` in the DSL.) Still: highlight colors, selection/page-anchored
      notes, backlinks.*
- [x] Statistics: `delryn-library::stats` + overlay (`i`) — totals, status mix,
      ratings, reading hours, top authors.
- [x] Export: `delryn-library::export` (`X`) — book list → CSV / JSON / Markdown.
- [x] Command palette (`:`): `delryn-library::fuzzy` matcher + `app/palette.rs` —
      jump to section/collection, sort, cycle layout, toggle panes, stats, export.

## Parsing architecture — semantics-first (delryn-format)

Goal: stop per-book whack-a-mole. Detect content from **standardized semantics**
first (`epub:type`/`role`, `data-type`, real HTML5), then **stable toolchain
fingerprints**, then generic heuristics — routed by **toolchain, not publisher**
(Packt/Manning each ship two markup families). Adding a publisher must become
*one data entry*, not edits in five places. Researched against the EPUB 3.3 spec
+ a survey of the real library (O'Reilly/Apress/No Starch/Manning/Wiley/Packt/
Pearson/self-pub). See `docs/parsing.md`.

- [x] **Phase A — semantics-first refactor.** `html.rs` (1276 lines) → `html/`:
      `mod` (orchestrator, 257), `normalize`, `dom`, `toolchain` (`ToolchainProfile`
      registry), `semantics` (`ElementRole` + `classify`), `inline`, `code`,
      `table`, `callout`, `math`, `tests`; `BookFormat` → `format.rs`.
      `block_element` is pure dispatch on `classify()`; detection consolidated;
      class/keyword data in the registry. Behaviour identical (43 tests +
      real-book verified). Per-document toolchain *routing* is a documented seam
      (not built until ≥2 profiles diverge — no premature abstraction).
- [x] **Phase B — EPUB3 navigation.** `epub/nav.rs`: parse the EPUB3 nav document
      via `get_nav_id`; TOC source **nav → NCX → spine**; `start_section` from the
      `bodymatter` landmark (first open skips front matter; saved progress
      overrides). Verified on real books. *Deferred (low impact per survey):
      page-list go-to-page + honoring spine `linear="no"` (index-remap risk).*
- [x] **Phase C — math recovery.** Native `<math>` in the body now transcodes:
      inline `<math>`→Unicode span (math-styled, in-prose); `<math display="block">`
      →`Block::Math`. Source priority **`alttext` LaTeX → `<annotation …tex>` →
      presentation-MathML walk** (`native_math_unicode`). `is_block` gates `<math>`
      on `display="block"` so inline math stays inline. Plus the prior MathML/LaTeX
      escaped in `<img alt>`. 3 new tests; no native-`<math>` book in the local
      library, so verified with synthetic fixtures (inline, display, authored-TeX).
- [x] **Phase D — footnote semantics (parse layer).** References and definitions
      now classify by the full standard set: `epub:type="noteref|footnote|endnote|
      rearnote"` **and** DPUB-ARIA `role="doc-noteref|doc-footnote|doc-endnote"`.
      `Block::Footnote` carries the raw `id` (match key) beside the display
      `label`; `Block::footnote_matches` / `find_footnote` resolve a reference to
      its definition (exact id → digit-normalized fallback). 6 new tests (model +
      parser). *Deferred to the reader-cursor task (Phase 2): the interactive
      ref→def→back jump, footnote preview popup, and cross-section endnote scan —
      the anchors + resolver are in place and ready for it.*

Honest limits (won't fake): image-only books (Pearson — every figure/table/eq is
a PNG with identical `alt="images"`) and font-class-only inline code (self-pub
"Practical Guide") get graceful placeholders, not reconstruction.

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
- [x] **Image policy + smart-invert** (Settings → Content → "Image mode",
      persisted): **Auto** (recolour ink, keep pictures), **Invert backgrounds**
      (lightness-invert opaque light-bg figures, detail kept; photos with light
      backgrounds invert too — the trade), **Faithful** (only flatten
      transparency). Uses **lightness inversion (flip L in HSL, keep hue+sat)**,
      not naive `255−RGB`. `media::RenderPolicy { tint, mode }` is part of the
      image cache key, so changing theme/mode re-renders live. Covers untouched.
      *Optional reader keybind to flip the current view — deferred.*
- [x] **Code blocks** already render on the reader page (highlight.rs takes only
      syntect *foreground*, never its background — no mismatched rectangle). Added
      a faint `Theme::code_surface()` panel (page nudged ~8% toward ink; padded to
      width for a clean rectangle) so code reads as a distinct surface.
- [x] **Icons → themed glyphs:** callout headers lead with a monochrome,
      single-width Unicode glyph per kind (ⓘ/✲/◆/△/▲), text-presentation so the
      theme tints them — no raster admonition icons.
- [ ] **Figure framing:** consistent themed border + padding (and optional soft
      scrim on very dark themes) so pictures read as intentional cards. *(Lower
      priority — figures already look clean floating on the page.)*

## Phase 5 — Formats

- [x] **Format recognition foundation:** `delryn-format::BookFormat` classifies
      files by extension (`is_readable`, `label`). The scanner indexes every
      recognized format (EPUB → full metadata; PDF/MOBI/AZW3 → by filename, so
      they show in the library now, badged); opening a non-EPUB reports cleanly
      on the status row. This is the seam the backends below plug into.
- [x] **PDF — page-as-image (v2)** (`delryn-format::pdf`) — **shipped & confirmed
      on Ghostty** (renders, fast page-flipping forward/back, single + two-page).
      Each page renders as an image (macOS-Preview fidelity), not reflowed text;
      v1 text-extraction (`feat/pdf`) was rejected.
      - **Engine:** `pdfium-render` (PDFium), runtime-bound (bundled `libpdfium`
        beside the binary → system fallback). One `Section`/page = one full-bleed
        `Block::Image` (`ImageWidth::Full`). Outline ← PDFium bookmark tree (flat
        "Page N" fallback); metadata ← Info dict. Clean status-row message when
        `libpdfium` or a graphics protocol is absent.
      - **Rendering (the hard part, solved):** full pages bypass `ratatui-image`
        and drive the **Kitty graphics protocol directly** via `app/page_deck.rs`
        (`PageDeck`), the `termpdf.py` model — transmit (`a=t`) + place (`a=p`, no
        placement id so spread pages coexist), swap only once all new pages are
        rasterized (never blanks). Pages load **async** off the main thread
        (`fetch_blocks`), the loader drops pages scrolled past (`LOADER_RADIUS`),
        flips are **throttled to the drawn frame** so a held key can't skip pages,
        and transmits go through a **temp file** (`t=t`, must be named
        `tty-graphics-protocol-*`) instead of multi-MB inline base64 — see
        [[delryn-ghostty-graphics]].
      - **Out of scope (later):** zoom/fit/pan, in-page search/selection (PDFium
        text-layer seam left for Phase 6/7). Optional future perf: transmit-once +
        `a=p`-only flips (no temp-file write per turn) — current per-turn temp-file
        transmit is already fast enough.
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

## Phase 7 — Advanced reading layout system

Generalize the two view modes (`Center` / `TwoPage`) into a **composition
engine** that renders *N* page tiles per view, parameterized rather than
hardcoded — so a new layout is a strategy + preset, not a new `if`. Grounded in
delryn's reality: it's a **terminal** (ratatui + Kitty graphics, a cell grid, no
GPU/smooth-pixel-scroll), and content is one of two kinds — **reflowable** (EPUB:
`Block`s → wrapped lines; "pages" are emergent) or **paged-image** (PDF, later
comics: one fixed page image per section). Most spread/grid modes are paged-only;
reflowable supports single/multi-column + scroll + presentation. The key
insight: the draft's ~16 "modes" are ~4 strategies + parameters. *Build the
engine + high-value presets; defer niche modes behind the interface (dev docs:
no premature abstraction, no speculative modes).*

- [ ] **7.1 Composition engine (the seam — refactor, no new user modes yet).** A
      `LayoutStrategy` mapping (viewport cells, position, content) → a list of
      *placements* (a cell `Rect` + what to draw: a page-image plan or a reflowed
      text-column slice); the renderer just draws placements. Port today's
      `Center` and `TwoPage`/PDF-spread onto it. New module `view/layout/` — not
      more bulk in `view/reader.rs` (pays down its 196-line `render` + the
      `reader/mod.rs` size debt). **Stable interface = adding a mode never edits
      the renderer.**
- [ ] **7.2 Cross-cutting plumbing.** Content-kind **registry** (which modes a
      format allows — `Document::paged_image()` already distinguishes them);
      **position preservation across switches** (paged↔paged = page index, trivial;
      reflow re-wrap = map by section + `within_frac`, already stored); per-strategy
      **keymap** (arrows scroll vs move a tile selection; ←/→ turn pages; Enter in a
      grid jumps); **presentation/chrome** as an orthogonal toggle (hide header/
      status/gutter, maximize area), layerable on any mode.
- [ ] **7.3 Tiled-pages presets (paged) — ONE parameterized strategy.** Params:
      tiles (rows×cols), step (1 = sliding / N = non-overlapping spreads),
      start-offset (cover-alone odd/even binding), direction (LTR / **manga RTL**),
      fit (width / height / page / fixed-zoom). The presets fall out: Single,
      Two-Page spread, Offset/Facing, Continuous-spread (step 1), Sliding window
      (step k), N-up, Manga. Plus **fit modes + manual zoom/pan** (re-rasterize the
      page at higher DPI — the zoom deferred from PDF v2 lands here). RTL also flips
      the gutter side + page-turn keys.
- [ ] **7.4 Distinct strategies (not presets).** Continuous **scroll across
      sections** (the long-missing chapter-join, for reflow *and* paged); **grid /
      thumbnail browser** as a visual page-jump complementing the TOC sidebar
      (arrows move selection, Enter opens).
- [ ] **7.5 Deferred behind the interface (cheap once 7.1 exists — build on
      demand, NOT speculatively).** Film strip (current page large + neighbours
      small); Comparison (pin arbitrary non-sequential pages — needs a "pinned
      pages" model distinct from current position); digital-magazine reflow.
- [ ] **Config** (Settings tab): pages-per-view, step/overlap, reading direction,
      fit strategy, gap/margin/alignment, presentation toggle; per-book layout
      memory (like KOReader).
- [ ] **Performance — reuse + measure, don't speculate.** The image pipeline
      already gives async decode + LRU + neighbour-prefetch + viewport-cull +
      deferred-on-scroll, and `ImgKey` caches per tile-size, so the engine plugs
      straight in. Grids transmit many Kitty images at once: downscale thumbnails
      hard, transmit on settle, cull off-screen. Add predictive prefetch / retained-
      page caps **only against a profile**. No GPU path — rasterize-once + cache +
      cull is the terminal-correct model.
- [ ] **Research spike** (informs the preset set, before 7.3): what KOReader
      (per-book layout + RTL), SumatraPDF (continuous-facing + cover offset), Okular,
      Calibre, Apple Books/Kindle, and comic/manga readers (CDisplayEx, Tachiyomi)
      actually expose — adopt the wins, skip GUI-only smooth-scroll + auto-magazine
      reflow. Folio-tuned, not a feature-clone.

## Tech debt — chip away, don't grow (dev docs size guidelines)

Soft "review/refactor triggers," not hard gates. Logged 2026-06-26 against the
updated dev docs; none block the build (`cargo fmt` + `clippy` are clean
workspace-wide). Address opportunistically when touching the area — and do not
grow these further.

- [ ] `app/reader/mod.rs` (~1210 non-test lines; `impl Reader` ~1024) — regrew
      past the 1000-line refactor trigger after the Phase 0 split to 601. Image
      lifecycle, sidebar, and search are already child modules; the remaining
      bulk is the decode/wrap/scroll/nav/history core. Carve further as PDF v2's
      page path lands, so it doesn't grow again.
- [ ] `delryn-render/src/layout.rs` (~1008 non-test lines) — the text-wrap engine.
      `wrap_blocks` is 287 lines (refactor trigger); decompose by block kind.
- [ ] `app/dispatch.rs::apply` (234 lines) — the action match; split by action
      group if it grows.
- [ ] `view/image.rs::render` (196 lines) — full-screen image view render.
- [ ] 2× undocumented `#[allow(clippy::too_many_arguments)]` in
      `app/reader/images.rs` (`sync_images`, `remap_section_images`) — regrew
      after Phase 0's cleanup. Bundle the (avail, max_rows, max_px, width_pct,
      policy) geometry into a struct to remove them. (The two `Store` row-writer
      allows are already documented — compliant.)
