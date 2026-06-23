# delryn — Roadmap

Living backlog. See `ARCHITECTURE.md` for the target structure and `DESIGN.md`
for the original spec. Phases are sequential; within a phase, items ship in
small green commits (build + `cargo test` + `cargo clippy` clean each step).

## Phase 0 — Foundation (`refactor/workspace`)

Migrate to a Cargo workspace and clear every dev docs violation.

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
- [~] Split `app.rs` into `app/` submodules (`refactor/app-split*`). Done:
      `confirm`, `settings`, `mouse`, `rename`, `select`, `collections`, `editor`
      (mod.rs 5.6k → ~3.5k; each a green commit). Remaining concerns: reader ·
      library; then split `apply()` (~180) and `library_key()` (~165). Pattern:
      child-module `impl App`, cross-module methods `pub(crate)`, types re-exported
      from `mod.rs`; concern tests stay in mod.rs (shared `key`/`ctrl`/`code` helpers).
- [ ] Sub-split `app/editor.rs` (~1.2k): carve the background online/cover
      execution (`online_search`/`apply_candidate`/`poll_online`/`tick_preview`/
      previews) into `app/editor/online.rs`, leaving the editor shell + dispatch.
- [ ] Split oversized views (`view/library` 714, `view/meta_edit` 524).
- [ ] Split `delryn-store` (925) by entity; `delryn-format::epub` (903) by concern.
- [ ] Drop the 4 `#[allow(too_many_arguments)]` via small param structs.

## Phase 1 — Technical content rendering

- [ ] Rich `Block` model (Code/Math/Table/Figure/Callout/Footnote/Citation/CrossRef).
- [ ] Code blocks: syntect highlighting, line numbers, wrap/h-scroll, copy, export,
      next/prev navigation, per-chapter code index.
- [ ] Callouts/admonitions (NOTE/TIP/WARNING/…), block quotes.
- [ ] Tables: inline / h-scroll / dedicated viewer; next/prev.
- [ ] Footnotes (jump/return/preview) + cross-references (See Chapter/Figure/…).
- [ ] Math: Unicode rendering + inline/block detection; next/prev + index.

## Phase 2 — Reading experience

- [ ] Reading modes: Continuous, Page, Chapter, Focus, Study, Research, Presentation.
- [ ] Pagination models: continuous / virtual pages / book pages (PDF) / reflowed.
- [ ] Navigation: reading history, navigation history (back/forward), jump-by-type.
- [ ] Bookmarks: named, quick, folders.

## Phase 3 — Library platform

- [ ] Views: Table (sortable columns), Cover Wall; refine Grid/List.
- [ ] Smart collections + filter DSL (`tag:rust AND rating>=4`, `status:reading`).
- [ ] Reading status (Unread/Reading/Paused/Finished/Dropped/Reference) + rating.
- [ ] Duplicate detection across formats; merge/keep/remove.
- [ ] Metadata diff view (current vs remote, selective apply).

## Phase 4 — Knowledge & power tools

- [ ] Highlights (colors), notes (selection/para/chapter/page), tags, backlinks.
- [ ] Statistics: books/pages/hours/streak/speed/authors/genres.
- [ ] Export: notes/highlights/bookmarks/metadata/stats → MD/JSON/CSV/HTML.
- [ ] Command palette (VSCode-style).

## Phase 5 — Formats

- [ ] PDF (`delryn-format::pdf`): page model, text extraction, figures.
- [ ] MOBI / AZW3.

## Phase 6 — Graphical math + deep performance

- [ ] Graphical math (typst/shell → image via terminal graphics), cached.
- [ ] Virtualized scrolling, incremental parsing, aggressive caching at scale.
