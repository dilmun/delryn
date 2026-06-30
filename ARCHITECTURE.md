# delryn — Architecture

delryn is a terminal-first EPUB/PDF reader **and** digital-library platform for
programmers, researchers, and students. It is not a viewer: reading, technical
rendering (code/math/tables/figures), library management, search, annotation,
and statistics are all first-class. See `DESIGN.md` for the original spec and
this file for the structure that realises it.

## Principles (from the global engineering rules)

Correctness → maintainability → readability → scalability → performance →
memory → simplicity → testability → modularity. Design for growth, but
**abstract only when it earns its keep** (≥2 real implementations or a needed
test seam). State is centralized in `App`; business logic stays out of the view
layer; the view only displays state.

## Layering

Dependencies flow **downward only**:

```
model
  ├── format · render · store · online · media · infra
  │      (I/O + engines; depend only on model, + infra where needed)
  └── library            (management logic; depends on model/store/format/online)
          └── delryn (bin) (TUI: app state, views, input, command palette, loop)
```

## Workspace crates

A Cargo workspace under `crates/`. Each crate is one cohesive layer; promoting
a module to a crate is justified by a real boundary, not speculation.

| Crate | Responsibility | Key deps |
|-------|----------------|----------|
| `delryn-model`   | Pure domain types: content `Block`/`Inline`, `Metadata`, `Format`, `Toc`, book/annotation types, naming heuristics. No I/O. | serde |
| `delryn-format`  | Parse bytes → model, behind the `Document` trait. EPUB now; PDF/MOBI later. | epub, zip, scraper, html2text |
| `delryn-render`  | Model → laid-out terminal content: layout/reflow, pagination, syntax highlight, math (Unicode+graphical), tables. | ratatui, syntect |
| `delryn-store`   | SQLite persistence, one module per entity. | rusqlite |
| `delryn-online`  | Metadata/cover lookup (Open Library, Google Books). | ureq, serde |
| `delryn-media`   | Terminal image protocols, async decode, image cache. | ratatui-image, image |
| `delryn-infra`   | Cross-cutting plumbing: background tasks (threads+rayon), LRU cache, config, theme, export (MD/JSON/CSV/HTML). | rayon, lru |
| `delryn-library` | Management logic: scan/index, manual + smart collections, filter DSL, dedup, search orchestration, statistics. | model, store, format, online |
| `delryn` (bin)   | The TUI: `App` state, input dispatch, command palette, views, event loop. | ratatui, crossterm |

`vendor/ratatui-image` (the Kitty-PNG patch) stays at the workspace root; the
`[patch.crates-io]` is workspace-level.

## The content model (`delryn-model::content`) — the linchpin

Every format produces it; every renderer and every "jump to next X" consumes it.
First-class block kinds: `Para`, `Heading`, `Code{lang}`, `Math{inline|block}`,
`Table`, `Figure`, `Callout{kind}`, `Quote`, `Footnote`, `Citation`, `CrossRef`,
`List`. Navigation by block kind (next/prev code block, equation, table, figure,
footnote, reference) and technical search both fall out of this uniform model.

## Trait seams (only where they earn their keep)

- `Document` (EPUB/PDF/MOBI) — multiple real impls. ✅
- `Exporter` (MD/JSON/CSV/HTML) — 4 impls. ✅ (when built)
- `MathRenderer` (Unicode / graphical) — 2 backends + hybrid. ✅ (when built)
- `Paginator` (reflow / fixed-PDF / virtual) — distinct algorithms. ✅ (when built)
- Smart-collection **query**: an AST + evaluator, not a trait.
- Syntax highlighting: one engine (syntect); languages are data, not impls.
- Reading modes / library views: enum + match, not traits (layout variants).

## Concurrency & performance

No async runtime. Background work runs on **threads + channels** behind
`delryn-infra::task`, with a **rayon** pool for CPU-bound jobs (indexing,
syntax highlight, math render, image decode); results are polled in the event
loop (the pattern already used for online search and the image builder). An
LRU `delryn-infra::cache` memoises covers, decoded images, highlighted code
(key: content-hash+lang), and rendered math (key: src-hash). Content is parsed
lazily per section and shared via `Arc` (no cloning); rendering is virtualized
to the viewport; DB writes batch in transactions. Target: responsive on
libraries of tens of thousands of books and books with thousands of
code blocks / equations.

## Persistence (SQLite, `delryn-store`)

One module per entity. Current: books (incl. status/rating/tags), progress,
shelves, collections, annotations (bookmarks + notes), fts. Planned: highlights,
backlinks, history, smart-collection rules. Schema changes are **versioned**
behind SQLite's `PRAGMA user_version` (append-only steps in `migrate()` that run
once, not on every open).

## Dependency decisions (deferred to their phase)

- **PDF**: `pdfium-render` (fidelity) leaning over pure-Rust `lopdf`/`pdf`.
- **Syntax**: `syntect` (already vendored in the manifest).
- **Math graphical**: Unicode-first (`delryn-render::math`), graphical later via
  `typst` (pure Rust) or a shelled-out renderer → image.
- **Cache**: `lru` (already in the manifest). **Query DSL**: hand-written.

## Build phases

0. **Foundation (this refactor)** — workspace + module tree, fix all rule
   violations, modernize (let-chains, `cargo fmt`), these docs + `TODO.md`.
1. **Technical content** — rich block model, code blocks, callouts, tables,
   footnotes/cross-refs, Unicode math.
2. **Reading experience** — modes, pagination models, navigation history,
   study/research layouts.
3. **Library platform** — table/wall views, smart collections + query DSL,
   reading status, dedup, metadata diff.
4. **Knowledge & power tools** — highlights/notes/tags/backlinks, statistics,
   export, command palette.
5. **Formats** — PDF, then MOBI/AZW3.
6. **Graphical math + deep performance** — caches, virtualization (ongoing).
