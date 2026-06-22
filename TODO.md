# delryn — Roadmap

Living backlog. See `ARCHITECTURE.md` for the target structure and `DESIGN.md`
for the original spec. Phases are sequential; within a phase, items ship in
small green commits (build + `cargo test` + `cargo clippy` clean each step).

## Phase 0 — Foundation (in progress: `refactor/workspace`)

Migrate to a Cargo workspace and clear every dev docs violation.

- [ ] Workspace skeleton: root `[workspace]`, move crate to `crates/delryn`.
- [ ] Extract `delryn-model` (content/metadata/toc/book types + naming helpers).
- [ ] Extract `delryn-store` (split by entity: books/shelves/collections/…).
- [ ] Extract `delryn-online`, `delryn-media`, `delryn-infra` (config/theme/task/cache/export).
- [ ] Extract `delryn-format` (Document trait + epub: read/extract/cover/html).
- [ ] Extract `delryn-render` (layout/paginate/math; highlight/table later).
- [ ] Extract `delryn-library` (scan/collections/query/dedup/search/stats).
- [ ] Split `app.rs` (5.6k) into `app/` submodules; split `apply()`/`library_key()`.
- [ ] Split oversized views (`view/library`, `view/meta_edit`).
- [ ] Modernize: let-chains (kills the 28 collapsible_if), fix the 3 real clippy
      warnings (`layout`/`math`/`media`), drop `#[allow(too_many_arguments)]` via
      param structs, `cargo fmt` the repo.
- [ ] Reinstall path is now `cargo install --path crates/delryn`.

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
