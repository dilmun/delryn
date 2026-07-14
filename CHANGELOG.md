# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0](https://github.com/dilmun/delryn/compare/v0.2.0...v0.3.0) (2026-07-14)


### Features

* **reader:** code folding and the F/I number-badge pick-mode ([#20](https://github.com/dilmun/delryn/issues/20)) ([b7f4e20](https://github.com/dilmun/delryn/commit/b7f4e20a646ac0a05db2cf157984d1fd089e3848))
* **reader:** DPI-independent, consistent equation & image sizing ([#22](https://github.com/dilmun/delryn/issues/22)) ([b9eaa86](https://github.com/dilmun/delryn/commit/b9eaa86807fb40c2ae247d1a4b48edd62c397fcc))
* **reader:** fullscreen code viewer, theme-aware highlighting, and shared list nav ([#19](https://github.com/dilmun/delryn/issues/19)) ([2f984de](https://github.com/dilmun/delryn/commit/2f984de31eccd976a31ad8cc575059c994ea1138))
* **reader:** translate looked-up words in the K panel ([6757e7e](https://github.com/dilmun/delryn/commit/6757e7e75de8145a436879a9787b8303f6dbda74))
* **reader:** word lookup (K) — dictionary + Wikipedia popup ([e91315a](https://github.com/dilmun/delryn/commit/e91315a9d9ba347fc295a35ec93e63469a7fad06))


### Bug Fixes

* **epub:** honor CSS display:block so citation lines don't run together ([#18](https://github.com/dilmun/delryn/issues/18)) ([a6587eb](https://github.com/dilmun/delryn/commit/a6587ebf2f78b3177aefb4d75f49668cca9ec0a0))
* **reader:** clip inline images across continuous boundaries instead of vanishing ([#21](https://github.com/dilmun/delryn/issues/21)) ([474840c](https://github.com/dilmun/delryn/commit/474840c690cc898784c3c187434f468236100062))

## [0.2.0](https://github.com/dilmun/delryn/compare/v0.1.0...v0.2.0) (2026-07-10)


### Features

* **mobi:** HUFF/CDIC decompression (type 17480) ([#9](https://github.com/dilmun/delryn/issues/9)) ([770a351](https://github.com/dilmun/delryn/commit/770a3512ed0d0b985c29d1fa9dfc793a847132a6))
* **mobi:** KF8/AZW3 sections, NCX table of contents, and inline images ([#13](https://github.com/dilmun/delryn/issues/13)) ([288d008](https://github.com/dilmun/delryn/commit/288d008b95c172ea9b9575e32d2a3b04fbd27a6d))


### Bug Fixes

* **reader:** heading-aware TOC navigation, status bar, and format label ([#14](https://github.com/dilmun/delryn/issues/14)) ([b9c24cb](https://github.com/dilmun/delryn/commit/b9c24cb681dea6466f055dffa3570e2ee7848602))

## [Unreleased]

## [0.1.0](https://github.com/dilmun/delryn/releases/tag/v0.1.0) - 2026-07-09

### Added

- *(reader)* Ctrl-d/Ctrl-u half-page caret navigation in cursor mode
- *(reader)* vim-style visual text selection
- *(annotations)* colour highlights with H, line wash, and overlay tab
- *(annotations)* store layer for colour highlights
- *(library)* trash-delete, orphan sweep, frequency tab order, stable tabs
- *(library)* in-app Sources tab to manage library folders + folder CLI args
- *(mouse)* make every modal overlay mouse-drivable via one shared mechanism
- *(mouse)* clickable library sidebar, detail pane, and annotations tabs
- *(reader)* two-page reflow flows across chapters (fills both columns)
- *(reader)* draw following sections' figures in continuous scroll
- *(overlay)* standardize all overlay windows — one compact size, f to enlarge, scrollable
- *(status)* [status] config block — per-zone segment order, separator, clock
- *(images)* auto-boost low-res equations + live "Equation size %" knob
- *(images)* normalize figure sizing + Lanczos3 quality; text-proportional equation strips
- *(math)* "Math size %" setting for graphical equation size
- *(math)* render display equations as themed images (graphical math)
- *(math)* retain the LaTeX source on Block::Math
- *(reader)* manga (RTL) two-page continuous scroll
- *(reader)* continuous PDF default to fit-page + side padding
- *(reader)* continuous PDF zoom/centre/pan + two-page stacking
- *(reader)* continuous scroll for paged (PDF) documents
- *(reader)* manga / right-to-left reading direction for paged spreads
- *(pdf)* constant margin crop for consistent page width
- *(pdf)* viewport-matched crisp re-raster (size-keyed page cache)
- *(settings)* expose PDF margin trim toggle (Settings → Content → PDF)
- *(pdf)* margin trim + full-bleed spread + sharper raster
- *(reader)* Phase 7.3 — PDF zoom & pan (fit modes + manual zoom)
- *(reader)* Phase 7.2 — preserve reading position across re-wraps
- *(view)* round the remaining selection highlights
- *(view)* rounded selection highlights everywhere
- *(status)* unified segment-based status bar
- *(ui)* add shared TextInput widget (char-cursor single-line editor)
- *(pdf)* theme full pages to the active theme (Auto/Invert/Faithful)
- *(dedup)* rename keep→ignore; add ignored-groups manager
- *(dedup)* drop READ column and per-group title from the resolver table
- *(dedup)* resolver preview + reveal, tabular layout; drop Enter-delete
- *(dedup)* scan every book — union all tiers instead of fallback
- *(dedup)* per-author metadata match + content scan skips metadata-grouped
- *(dedup)* resolver full-screen, format/converted prefs, path column
- *(dedup)* match books by their table of contents
- *(dedup)* identify books by title read from the page, not metadata
- *(dedup)* fingerprint the title page / front matter, not the middle
- *(dedup)* content-fingerprint deep scan (SimHash), drop cover+title
- *(dedup)* thorough cover-hash duplicate scan (R in Duplicates view)
- *(dedup)* multi-key union-find detection + "keep both" dismissals
- *(library)* right-aligned book counts per sidebar group
- *(library)* PDFs/EPUBs sidebar views + clearer Collections divider
- *(library)* direction-aware cover prefetch + vim page navigation
- *(library)* single cover grid — rounded covers, details caption, no badge
- *(library)* stretch covers to fill the cell (grid + wall) + wall badge
- *(library)* Cover Wall view — immersive, chrome-minimal cover grid
- *(reader)* cover-page offset for two-page PDF mode
- *(reader)* PDF navigation polish — scroll-spy, page indicator, jumps
- *(reader)* rewrite PDF page rendering on the termpdf.py model
- *(reader)* direct-Kitty PDF page rendering (icat model, no flash)
- *(reader)* two-page spread for PDF (facing pages)
- *(format)* PDF v2 — page-as-image backend (pdfium-render)
- *(library)* duplicate-resolution overlay (checkboxes + smart auto-select)
- *(library)* duplicate resolution (keep one, delete the rest)
- *(layout)* justify, soft-hyphen breaks, converter-spacing tidy
- *(settings)* group options into scrollable tabs
- *(images)* size figures consistently from authored width
- *(library)* reading-status enum + per-column show/hide + sort rework
- *(lookup)* Year + ISBN seed fields; ISBN-only exact lookup
- *(library)* metadata diff + selective apply
- *(ui)* responsive panes everywhere (reader, library, image viewer)
- *(image-viewer)* open on current figure, copy, editable save path
- *(reader)* full-featured image viewer
- *(reader)* unify edge padding across modes; drop Fill; configurable two-page gap
- *(reader)* reading-mode presets (Study/Research/Presentation)
- *(bookmarks)* pure bookmarks, gutter ribbon, modern overlay
- *(bookmarks)* named bookmarks + folders
- *(reader)* page mode — paginated reading (Phase 2)
- *(reader)* follow cross-references & citations (complete link cursor)
- *(reader)* link cursor — follow footnote refs / links (Phase 2)
- *(reader)* toggleable table wrap + zebra-striped rows
- *(epub)* EPUB3 navigation document — TOC + bodymatter start (Parsing Phase B)
- *(theme)* code-block surface panel + themed callout glyphs
- *(media)* image rendering modes — smart lightness-invert
- *(formats)* recognize PDF/MOBI/AZW3 — index by filename, dispatch on open
- *(app)* command palette + fuzzy matcher (Phase 4)
- *(library)* export book list to CSV/JSON/Markdown (Phase 4)
- *(library)* statistics overlay (Phase 4)
- *(library)* cross-format duplicate detection (Phase 3)
- *(library)* per-book rating (Phase 3)
- *(library)* smart filter query DSL (Phase 3)
- *(reader)* unified jump-by-type navigation (Phase 2)
- *(format)* display-math detection → Block::Math
- *(format)* footnote/cross-ref anchors + footnote definitions
- *(reader)* code-block navigation (next/prev + position)

### Fixed

- *(reader)* two-stage cursor mode + fix two-page annotation anchoring
- *(images)* figures blank after jumping from the image viewer
- *(reader)* TOC click moves the highlight on the first click
- *(reader)* anchor reserves the stable estimate too (consistent with following)
- *(reader)* size following continuous sections on demand (no lag/stale wrap)
- *(reader)* stable up-front image reservation — no re-wrap, no scroll jump
- *(reader)* re-wrap when a built image's true height differs from the estimate
- *(library)* cumulative column fit so hiding others reveals a wide column
- *(images)* make Kitty image-id namespaces disjoint
- *(images)* caption-based figure/equation classification + live sizing refresh
- *(images)* stop neighbour prefetch from evicting the current section's figures
- *(images)* keep inline figures visible while scrolling; protect them from eviction
- *(images)* free the image viewer's terminal images to stop the leak
- *(library)* sidebar wheel steps one section per notch
- *(library)* scroll-into-view + per-pane wheel
- *(library)* full mouse support + visible multi-selection
- *(pdf)* x halves the page's own margin (not tight crop / not EPUB edge)
- *(pdf)* half the edge margin, not zero
- *(pdf)* keep a small gutter between spread pages (like EPUB)
- *(theme)* never let a book override the global theme
- *(pdf)* re-transmit pages when the theme/image mode changes
- *(dedup)* make the resolver footer fit so "open location" is visible
- *(dedup)* keep the directory, trim the long filename in the path column
- *(dedup)* gate cover-scan links on title agreement; add dev docs
- *(dedup)* allow D/R from the sidebar in the Duplicates view
- *(library)* bound cover cache (no blank wall) + coverless placeholder
- *(library)* bound cover-wall thumbnails on a uniform panel (not crop)
- *(library)* fill cover-wall cells (crop) so thumbnails are uniform
- *(library)* render PDF covers (first page) so they show in grid/wall
- *(reader)* two-page PDF spread flips a whole leaf (no overlap)
- *(reader)* default PDF transmit back to inline (Ghostty blanked on t=t)
- *(reader)* throttle PDF flips to the display so holding j/k can't skip
- *(reader)* unique placement id per PDF page (left page was blank)
- *(reader)* correct page swap + direction-aware prefetch (PDF)
- *(reader)* page-flip navigation for PDF (kill the scroll-blank flash)
- *(reader)* drain the section loader during rendering (facing page)
- *(reader)* draw the facing page in a PDF two-page spread
- *(library)* responsive book list columns + grid cover sizing
- *(media)* real rainbow cause — theme_invert used HSL saturation near white
- *(image-viewer)* chapter label, faithful colours, jump to figure; responsive panes
- *(image-viewer)* exclude equation images; scale + center the figure
- *(reader)* don't paint inline images over an open overlay
- *(input)* search prompt captures keys before global shortcuts
- *(bookmarks)* gutter gap, flag icon, guaranteed margin
- *(reader)* link cursor treats a whole multi-word link as one stop
- *(render)* no link underline + compact ToC / definition lists
- *(reader)* strip regenerated markers + file-aware cross-ref jumps
- *(reader)* footnote-section nesting, quote color, open links in browser
- *(media)* equation legibility + precise overlay/image occlusion
- *(theme)* detect the real terminal background for the 'terminal' theme
- *(reader)* equation sizing + two-page image eviction
- *(media)* recolour math/line-art images to the theme (no more black-on-black)

### Other

- add README with screenshot carousel + MIT license ([#6](https://github.com/dilmun/delryn/pull/6))
- *(library)* run folder scans off the UI thread
- *(library)* route every book deletion through the OS trash
- *(reader)* blank line between chapters in cross-section flow
- Merge branch 'main' into feat/mobi-azw3
- *(reader)* carve jump-by-type element nav into elements.rs
- *(notes)* duplicate-bookmark guard + Bookmarks/Notes tabs in the overlay
- Merge branch 'main' into feat/notes-highlights
- *(status)* gauge-only by default (percent off) + theme-accent gauge fill
- *(reader)* transmit-once page deck — fast continuous scroll
- *(app)* split dispatch.rs (840) into a dispatch/ tree
- *(reader)* extract crop & crisp re-raster into crisp.rs
- *(reader)* extract paged-image nav & zoom/pan into paged.rs
- *(reader)* extract link-cursor & footnote nav into anchors.rs
- Merge main into feature/reader-continuous-scroll (crisp raster + constant PDF margin)
- *(reader)* Phase 7.1 — layout composition engine seam
- *(app)* move render-derived layout metrics out of LibraryState
- *(reader)* bundle image geometry into ImageGeom
- *(view)* paint every library + overlay surface in roles
- *(reader)* paint the reader view in roles
- *(status)* paint the status bar in roles
- *(dispatch)* extract reader navigation out of apply()
- *(editor)* migrate metadata-editor forms onto TextInput
- *(ui)* migrate five inputs onto the shared TextInput widget
- *(reader)* decompose the Reader god-object into sub-state
- *(app)* collapse 13 overlay Options into one Overlay enum
- *(app)* extract Session from the App god-object
- *(app)* extract LibraryState from the App god-object
- *(dedup)* match by TOC content only; drop metadata grouping
- *(reader)* remove the dead pre-direct-kitty full-page pipeline
- *(reader)* temp-file PDF transmit, now with the name Ghostty requires
- *(reader)* transmit PDF pages via temp file, not 2.5MB of base64
- *(reader)* async PDF page loads so fast j/k scrolls smoothly
- *(reader)* faster + correct direct-Kitty PDF rendering
- *(reader)* transmit pages to the terminal ahead of display
- Reapply "perf(reader): pre-upload look-ahead PDF pages (kill the scroll flash)"
- Revert "perf(reader): pre-upload look-ahead PDF pages (kill the scroll flash)"
- *(reader)* pre-upload look-ahead PDF pages (kill the scroll flash)
- *(reader)* keep spread pages warm + visible during scroll
- *(reader)* make the PDF spread's facing page first-class (no lag/flicker)
- Merge duplicate resolution (D)
- *(ui)* pad pane titles so the border never touches them
- *(image-viewer)* move pixel dimensions to a right-aligned title badge
- *(ui)* collapse side panes sooner (protect a comfortable main pane)
- *(ui)* rounded borders everywhere + wider two-page gap
- *(theme)* make Theme the single source of truth for colours
- *(app)* split input dispatch into app/dispatch.rs
- *(app)* split reader by concern into reader/{mod,images,sidebar,search}.rs
- *(app)* split editor into editor/{mod,lookup}.rs
- resolve the too_many_arguments lints
- *(view)* split the library and meta_edit view god-files
- *(app)* extract app/library.rs (library browse mode)
- *(app)* extract app/reader.rs (reading view model)
- *(app)* extract app/editor.rs (metadata editor + online lookup)
- *(app)* extract app/collections.rs (collections & shelves)
- *(app)* extract app/select.rs (library multi-selection)
- *(app)* extract app/rename.rs (rename mechanism + bulk popup)
- *(app)* extract app/mouse.rs (MouseHits + click/scroll routing)
- *(app)* extract app/settings.rs (Settings + items + handlers)
- *(app)* extract app/confirm.rs (PendingConfirm + handlers)
- *(app)* convert app.rs to app/ module directory (no code change)
- cargo fmt the workspace (canonical Rust 1.96 style)
- clippy-clean the workspace (let-chains, boolean simplify)
- extract delryn-library crate (scan/index layer)
- extract delryn-render crate (layout + highlighting)
- extract delryn-format crate (Document trait + EPUB)
- extract delryn-media crate (image protocols/decode)
- extract delryn-online crate (metadata/cover lookup)
- extract delryn-store crate (SQLite persistence)
- extract delryn-infra crate (paths + config + theme)
- move naming/ISBN heuristics into delryn-model::naming
- extract delryn-model crate (content/metadata/toc types)
- convert to a Cargo workspace (crate → crates/delryn)
# Changelog
