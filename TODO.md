# delryn — Roadmap

Living backlog of **remaining** work. See `ARCHITECTURE.md` for the target
structure and `DESIGN.md` for the spec. The full history of completed work lives in
git (and was cleared from this file on 2026-07-08 — it had grown to ~1070 lines of
finished `[x]` entries). Invariant: `main` stays green — build + `cargo test` +
`cargo clippy` (0 warnings) + `cargo fmt` clean on every commit.

## Shipped (done — don't rebuild; git has the detail)

- **Phase 0 — Foundation:** Cargo workspace (8 crates), every god-file/god-object
  split by concern, 0-warning clippy workspace-wide.
- **Phase R — Redesign/cleanup:** `Config` single source of truth, shared
  `TextInput`, semantics-first parser (`html/` toolchain registry), versioned store
  migrations, theming **role system** (file-configurable + contrast gate),
  segment-model **status bar** + `[status]` config.
- **Phase 1 — Technical content:** rich `Block` model (math/table/callout/footnote +
  figure captions), syntect code blocks, tables, footnotes/cross-refs, MathML+LaTeX
  math, equation/line-art image recolour, consistent figure sizing + `ImageFit`
  (Lanczos3).
- **Phase 2 — Reading experience:** Continuous + Page modes, presets, justify + soft
  hyphens, jump-by-type + link cursor, bookmarks (named/folders/gutter ribbon),
  responsive splits, tabbed settings overlay.
- **Phase 3 — Library platform:** sortable table/grid, filter DSL + smart
  collections, reading status + ratings, TOC-based duplicate detection + resolver,
  metadata diff view, mouse everywhere, in-app **Sources manager**, delete→OS-trash,
  background library scan.
- **Phase 4 — Knowledge tools:** notes + tags + colour **highlights** (`H`) + **vim
  cursor mode / text selection** (`V`; sub-line highlights/notes on both pages),
  stats, export, command palette. *(Backlinks dropped — see Won't-do.)*
- **Parsing:** semantics-first refactor, EPUB3 nav, native `<math>` recovery,
  footnote semantics.
- **Theming/coherence:** `Theme` single source of truth, image policy + smart-invert,
  PDF page theming, themed code surface + callout glyphs.
- **Phase 5 — Formats:** PDF **v2** (page-image render via direct-Kitty deck; zoom/
  pan/fit, two-page, continuous, manga-RTL, viewport-matched crisp re-raster);
  MOBI/AZW3 **v1** (PalmDOC; EXTH metadata+cover).
- **Phase 6 — Math:** graphical **display** math (RaTeX, disk-cached, theme-recoloured).
- **Phase 7 — Layout system:** composition engine (`view/layout/` strategies +
  `LayoutPlan`), position preservation across mode switches, tiled/zoom/pan/fit
  presets, continuous scroll (reflow **and** paged), manga/RTL.
- **2026-07-06 audit fixes:** disjoint Kitty id namespaces; cell-based display width;
  DOM/MathML/nav/outline recursion bounded (fixed a real stack-overflow on untrusted
  input); MOBI prealloc caps; regexes → `LazyLock`.

## Remaining work

### Release/CI pipeline (release-plz + GitHub Actions)  🚧 in progress
Automated pipeline where the one decision is "merge the standing release PR". Engine is
**release-plz** (not release-please — delryn's virtual workspace + inherited
`[workspace.package].version` is release-please's weak spot). Files: `release-plz.toml`,
`.github/workflows/{ci,pr-title,release-plz,release-build,release}.yml`, `CHANGELOG.md`,
`docs/RELEASING.md`. CI = matrix build/test (ubuntu+macOS) + fmt + clippy `-D warnings`
(no system deps needed). Release binaries bundle a pinned `libpdfium` (`chromium/7763`)
per platform + `.sha256`. **Remaining to finish:** create public GitHub repo + push;
maintainer creates a release-bot GitHub App (Contents+PR RW) → `RELEASE_PLZ_APP_ID` +
`RELEASE_PLZ_PRIVATE_KEY` secrets; apply squash-only merge settings
+ branch protection (required checks + `enforce_admins`) via `gh`. See `docs/RELEASING.md`.

### Phase 5 — MOBI/AZW3 HUFF/CDIC decompression  📌 recommended next
The user has real `.azw` files (SWING TRADING / OPTIONS TRADING) that currently
refuse with "HUFF/CDIC-compressed MOBI is not supported yet." Compression type
**17480**: parse the `HUFF` record (Huffman tables: dict1 256-entry + dict2 64-entry,
per-code-length min/max codes) and the `CDIC` dictionary record(s) (byte-sequence
entries), then bit-decode each text record into the byte stream
`delryn-format/src/mobi/mod.rs::extract_text` already assembles. Self-contained
(~200 lines, no new deps) — drops in beside `palmdoc.rs` as a third compression path.
Reference impls: KindleUnpack `mobi_huff`, Calibre `mobihuff`. Covers most
Amazon-distributed MOBI/AZW. **Needs a real HUFF/CDIC file to validate (can't
unit-test blind) — the user HAS them.** DRM stays out of scope.
*(Other MOBI v1 gaps, lower priority: full KF8 skeleton/fragment reconstruction +
real `filepos` NCX TOC; MOBI full-text index.)*

### Phase 6 — Inline graphical math  (hard; build on demand)
Display math ships; inline math (`Span` with `Inline.math`) is still flattened to a
Unicode approximation. Hard because the terminal is a **cell grid** and Kitty places
images at cell origins:
1. **Source** — inline spans keep only Unicode today (`native_math().0` in
   `html/inline.rs` + the `<img>`/math-class paths); retain the raw LaTeX on the span
   (mirror what display math already does on `Block::Math`).
2. **Mid-line placement** — no mechanism drops a small image *inside* a wrapped line.
   The inline equation needs to be an **atomic N-cell-wide run** (N = rendered px /
   cell width) the filler reserves like a wide glyph, plus a new `Run`/`LineKind`
   variant carrying an image id the reader draws into those cells (baseline is
   cell-aligned, small vertical offset accepted).
3. **Height** — render at ~1 em, measure the PNG, and **fall back to Unicode for any
   inline equation taller than ~1.3 cells** so fractions/∑-with-limits don't smear.
   Most real inline math (xⁿ, αᵢ, subscripts) fits.
Reuses `delryn-math` + the disk cache; the new surface is the wrap/line model + an
inline draw path.

### Phase 6 — Deep performance  (MEASURE FIRST — no slowness reported)
- Virtualized wrapping: wrap only the visible window + neighbours (some pre-wrap
  exists via `SectionLoader`); cap retained wrapped lines for very long chapters.
- Parsed-`Section` LRU (lazy parse mostly exists via `load_section`).
- Persist wrapped-layout + cover thumbnails keyed by (path, mtime, width, theme) so
  re-opens are instant.

### Theming — Figure framing  (small, self-contained)
Consistent themed border + padding (optional soft scrim on very dark themes) so
figures read as intentional cards. Low priority — figures already look clean.

### Phase 1/2 — Image viewer zoom + pan
The figure image viewer (`i`) still lacks zoom (fit↔actual) + pan (SlicedImage);
otherwise complete (sidebar, details, jump, filter, save, whole-book scope).

### Phase 3 — Dedup further tiers  (only if duplicates still slip through)
All kept cheap/lazy, no library-wide content scan: bounded fuzzy-title fallback
(blocked by author/first-token); confidence tiers (Exact/Likely/Possible); cached
`content_hash` (blake3, incremental, rayon) for zero-false-positive "Exact"; lazy
content fingerprint only for the few books being resolved; PDF↔EPUB content matching
once a PDFium text layer exists.

### Phase 7 — remaining layout pieces  (build on demand, not speculatively)
- **7.2 deferred plumbing** (only when a mode needs it): content-kind registry
  (which modes a format allows — `Document::paged_image()` is the seam); per-strategy
  keymap (tile-select/grid-Enter); presentation/chrome toggle (extend `focus_mode` to
  drop the header for full-bleed).
- **7.5 behind the interface:** film strip (current page large + neighbours small);
  comparison (pin arbitrary non-sequential pages — needs a "pinned pages" model);
  digital-magazine reflow.
- **Config tab:** pages-per-view, step/overlap, fit strategy, gap/margin/alignment,
  presentation toggle; **per-book layout memory** (like KOReader).
- **Research spike** (informs presets): what KOReader / SumatraPDF / Okular / Calibre
  / Apple Books / comic readers expose — adopt wins, skip GUI-only smooth-scroll +
  auto-magazine reflow.

## Deferred — measure first (don't churn without a profile)
- `recolor::render_for_theme` re-converts to RGBA per branch (~2–4 full-buffer allocs
  per image build) + a double pixel scan in the transparent branch. Off-thread,
  cached, dominated by the Lanczos3 resize — would churn 3 public signatures for no
  measured win.
- Layout **grapheme-cluster** awareness: `display_width` fixes measurement, but
  wrap/hard-break/`fit` still iterate `chars()`, so a base+combining cluster can split
  across a wrap. Needs `unicode-segmentation`; low demand (only NFD combining scripts).
- Inline images can't recover from **terminal-side** eviction (transmit-once); fix =
  re-transmit-on-reveal with a fresh id + byte budget (Ghostty evicts by transmit
  time; bug #6711 blocks re-transmit-by-id) — only if it bites in heavy books.

## Tech debt — chip away when touching the area (dev docs size guidelines)
Soft review/refactor triggers, not gates; none block the build. Don't grow them.
- `delryn-render/src/layout/spans.rs` (511) — the largest layout child, in the *review*
  band; decompose the fill/emit path if it's touched again.
- `view/image.rs::render` (~196) — full-screen image view render.

## Won't do — decided; do NOT re-propose unless asked
- **Backlinks** (2026-07-08) — leans note-app over reader; tags + search cover it.
- **PDF text reflow** — PDF renders as page images (a text-extraction v1 was rejected).
- **Cover wall / coverflow** — the Grid already covers it.
- **N-up reading grid** — floods the Kitty deck with full-res rasters + letterboxes
  portrait pages; would need a real thumbnail pipeline.
- **Thumbnail page-jump browser** + trackpad scroll-accumulation.
- **In-app trash-restore** — deletion goes to the OS trash, no restore.
- **App-owned GPU / font-atlas text rendering** — the terminal's job.
