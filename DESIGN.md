# delryn — Design

A terminal reader for EPUB and PDF, written in Rust. This document is the
source of truth for the product/UX design and the high-level architecture.

Status: design locked 2026-06-20. Implementation in progress, **EPUB-first**.

## 0. Locked decisions

These are settled; don't re-litigate without a reason.

- **Terminal-native, not app-GPU.** A TUI emits cells over a PTY; the *terminal
  emulator* owns the GPU, glyph atlas, and rasterization. We do **not** build a
  font atlas or GPU text renderer. We get speed by (a) minimal diffed cell
  updates, (b) **synchronized output** (DEC 2026) for atomic, tear-free frames,
  and (c) **graphics protocols** (Kitty / Sixel / iTerm2) for pixels — covers,
  diagrams, math, rasterized PDF pages — with a fallback ladder. "GPU where it
  helps" means *compute* (image/PDF rasterization), never text.
- **Performance is a feature.** Smooth scroll comes from the render pipeline
  (§2.1), not hardware: synchronized output + frame pacing + input coalescing +
  cached layout + background pre-wrap.
- **EPUB-first, exhaustively.** Nail every reading/library/search/annotation
  feature and the whole layout for EPUB *before* adding any other format. PDF,
  MOBI, AZW3, CBZ/CBR, Markdown, HTML, txt all come later behind the plugin
  trait — designed for, not built yet.
- **Stack:** Rust · `ratatui` + `crossterm` · `html2text` (EPUB text) ·
  `syntect` (code highlighting) · `rusqlite` + FTS5 (library/state/annotations)
  · graphics-protocol crate for images (later) · std threads + channels for
  background work (no async runtime unless a subsystem demands it).

## 1. Scope & roadmap (EPUB-first)

Sequenced so the app is fast and the reading experience wins before breadth.

- **Phase 0 — Perf foundation:** synchronized output + frame-paced/coalesced
  render loop (kills scroll jitter); section layout LRU + background neighbor
  pre-wrap (no chapter-boundary hitch).
- **Phase 1 — Reading that wins:** persistence/resume (SQLite); rich typography
  (heading hierarchy, emphasis, quotes, lists, tables, notes/tips/warnings);
  **code-block engine** (syntect highlight, preserved indentation, h-scroll ⇄
  soft-wrap, line numbers, copy/export) — the programming-book wedge; theme
  system; reading modes + layout controls; settings popup + status-bar config.
- **Phase 2 — Navigation & library:** collapsible TOC tree + nav history;
  library manager (table/grid/compact/cover, collections/tags/series/authors,
  filtering, metadata edit, duplicates, smart collections); cover thumbnails.
- **Phase 3 — Power features:** in-book search (regex/fuzzy); library + full-text
  search; annotations (bookmarks/highlights/notes) with reflow-stable anchors;
  reading stats.

**Deferred (same `Document` interface, designed-for not built):** PDF (hybrid —
text reflow default, key-toggle page image via `pdfium`/`mupdf` + graphics
protocol), then MOBI/AZW3/CBZ/CBR/Markdown/HTML/txt.

## 2. Architecture

Pipeline / layers. The only place formats differ is the Document model; every
layer above it is format-agnostic.

```
Source file
  → Document model      (format parsing; EPUB now, PDF later)
  → Layout / reflow     (content → wrapped lines, measure cap; or bitmap for PDF image mode)
  → View (ratatui)      (reader, library, settings, status bar — all shared)
  → State / store       (per-book session + library index + config, persisted)
```

### 2.1 Rendering & performance pipeline

The render loop is single-threaded and **frame-paced**; heavy work is offloaded
to background workers and delivered over channels.

```
┌── events ──────────┐   coalesce    ┌──── render loop (main, ~120fps cap) ────┐
│ key / mouse / resize│ ────────────▶ │ drain queued events → apply net delta   │
└─────────────────────┘   dirty flag  │ if dirty & budget elapsed:              │
                                       │   BSU → diff-draw viewport → ESU        │
   idle: block on poll (0% CPU) ──────▶│ (synchronized output = atomic present)  │
                                       └──────────────────────────────────────────┘
   workers: parse · pre-wrap next/prev section · index · thumbnails · search
```

Rules that keep it fast:
- **Never re-wrap on scroll.** Wrapped lines are cached per `(section, width)`;
  scrolling only slices the cached buffer.
- **LRU of wrapped sections** + **background pre-wrap** of neighbors so chapter
  transitions don't block on `html2text`.
- **Synchronized output** brackets every frame → no tearing/jitter.
- **Coalesce input**: holding a key applies the net scroll delta once per frame.
- **Idle = 0% CPU**: the loop blocks on `poll` until an event or a long timeout.

### Cache hierarchy

```
L1  in-memory LRU of wrapped sections (current ± neighbors)
L2  parsed document structure (spine, outline, headings) for the open book
L3  SQLite — library index, reading state, annotations  (~/.config/delryn)
    disk — cover thumbnails keyed by content hash
    disk — search index (FTS5; tantivy if escalated)
```

### Module layout

```
src/
├── main.rs            # entry, CLI args, terminal setup/teardown, run loop
├── app.rs             # App state, Mode (Library | Reader), event dispatch
├── config.rs          # settings model + TOML load/save (general + per-mode)
├── document/
│   ├── mod.rs         # Document trait + shared model types
│   └── epub.rs        # EPUB implementation
├── layout.rs          # reflow: content → wrapped lines, measure cap, centering
├── view/
│   ├── mod.rs         # top-level render dispatch by Mode
│   ├── reader.rs      # sidebar + content + status bar
│   ├── library.rs     # sections sidebar + list/grid
│   ├── settings.rs    # mode-scoped settings popup
│   └── widgets.rs     # shared widgets (status bar, progress gauge)
├── input.rs           # keymap (vim) + mouse mapping, context-aware
└── store/
    └── mod.rs         # SQLite (library + sessions) + thumbnails + paths
```

## 3. Document model

A trait both formats implement; the view layer only ever sees this.

```
trait Document {
    fn metadata(&self) -> &Metadata;        // title, author(s), year, cover, …
    fn toc(&self) -> &[TocEntry];           // nested table of contents
    fn sections(&self) -> &[SectionRef];    // ordered spine (chapters)
    fn load_section(&mut self, i: usize) -> Result<Section>;  // text content
}
```

- `Metadata`: title, authors, year, language, identifier, cover image, byte size.
- `TocEntry`: label, target section index, nested children (tree).
- `Section`: reflowable content as a sequence of blocks (paragraphs, headings,
  etc.) ready for the layout pass. EPUB derives these from XHTML via
  `html2text`; PDF later derives them from text extraction.

## 4. Reader view

Three regions; the **sidebar and status bar are independently toggleable**
(four states: both / sidebar / bar / immersive).

```
┌─ Contents ──────────┬──────────────────────────────────────────────┐
│ ▸ 1. Loomings       │                                               │
│ ▾ 2. The Carpet-Bag │      Chapter 3 · The Spouter-Inn              │
│   • 3. Spouter-Inn ◂│      Call me Ishmael. Some years ago—never    │
│   5. The Chapel     │      mind how long precisely—having little    │
├─────────────────────┴───────────────────────────────────────────────┤
│ Moby-Dick — Herman Melville          3/135 · 42%  ████████░░░░░░░░   │
└──────────────────────────────────────────────────────────────────────┘
```

- **Continuous scroll** within a chapter; reaching a chapter end flows into the
  next. `j/k`/arrows by line, `Space`/`PgDn` by screen.
- **View modes** (`v` cycles): **Center** (single centered column) and
  **Two-page** (two side-by-side columns; the right continues from the left so
  scrolling flows left→right). Both use the same configurable per-side edge
  padding (`side_padding` %); two-page adds a configurable inter-column gap
  (`page_gap`). The active mode shows in the status bar.
- **Sidebar hide**: `s` toggles the sidebar; `Tab` moves focus into it (showing
  it first if hidden) and back. When hidden, content reclaims width and re-centers.
- **Left sidebar = navigable outline**: a flat list of sections (labeled from
  the book's TOC, else first heading, else "Section N") with their **headings**
  (`h1`–`h6`, scanned from the XHTML) nested beneath — so every header is
  selectable, consistently, even when the book's own TOC is chapter-level only.
  `j/k` move, `Enter`/`l` jump. Jumps to a heading **locate its text in the
  reflowed page** (preferring an exact-line match), which sidesteps fragile
  HTML-anchor→line mapping and degrades gracefully (a miss lands at section top).
  This flattens cross-chapter TOC grouping for now; grouping can be layered back
  later. (Future: switchable Search results / Bookmarks panels.)
- **Status bar**: book — author on the left; position `i/N`, percent, and a thin
  progress gauge on the right. `;` settings hint shown here.

## 5. Library view

Sections sidebar + a content area with two modes (`v` cycles them).

**Sections:** Recent (default landing) · All Books · Currently Reading ·
Favorites · Authors · Tags · Collections.

**List mode** — metadata columns, sortable (click header or keys):

```
┌─ Library ───────┬───────────────────────────────────────────────────────┐
│ ▸ Recent        │  Title              Author       Year  Pages  Size     │
│   All Books     │  Moby-Dick          H. Melville  1851   654   1.2M   ◂ │
│   Favorites  ★  │  The Odyssey        Homer        −800   541   890K     │
├─────────────────┴───────────────────────────────────────────────────────┤
│ 124 books · Recent ↓                            v list/grid              │
└───────────────────────────────────────────────────────────────────────────┘
```

**Grid mode** — dynamic cover flow, reflows to `width ÷ thumbnail size`:

```
┌─ Library ───────┬───────────────────────────────────────────────────────┐
│ ▸ Recent        │  ┌──────┐  ┌──────┐  ┌──────┐  ┌──────┐                │
│   All Books     │  │ cover│  │ cover│  │ cover│  │ cover│                │
│   Favorites  ★  │  └──────┘  └──────┘  └──────┘  └──────┘                │
│                 │  Moby-Dick Odyssey   Dune      Neuromancer             │
├─────────────────┴───────────────────────────────────────────────────────┤
│ 124 books · grid · 256px                  +/- size   v list/grid         │
└───────────────────────────────────────────────────────────────────────────┘
```

- **Thumbnails**: pixels, 64-px steps `128 → 192 → 256 → 320 → 384 → 448 → 512`
  (cap). Covers rendered via terminal image protocol.
- **No-image fallback**: terminals without Kitty/Sixel/iTerm2 show a bordered
  box with title/author instead of a cover.
- **Pages caveat**: EPUB is reflowable and has no real page count. The Pages
  column shows an **estimate** (chars→pages, fixed ratio); **% read** is always
  available regardless.
- **Delete to Trash**: `Delete` on the selected book (or the whole marked
  selection) moves the file(s) to the OS trash after a yes/no confirmation —
  recoverable, never an unlink. The library only holds books inside a configured
  source folder, so removing a source (or *Rescan now*) also sweeps orphans —
  including the bare row a one-off `delryn <file>` open leaves behind.

## 6. Library ↔ Reader

Two top-level views behave like **tabs**: bare `delryn` lands on the Library;
opening a book switches to the Reader; you can toggle back to the Library
(`q`) with the book still loaded, and reopen instantly. `Q` quits.

**CLI entry:** `delryn <file>` opens a book. `delryn <folder> [folder…]`
registers each folder as a library source (deduped), scans, and lands on the
Library — the ergonomic form of `delryn --add`. On **first run** (no source
folders configured) delryn opens straight on the Sources manager (§7) so a new
user's first action is adding a folder.

## 7. Settings — mode-scoped popup (`;`)

`;` opens a settings popup. It is **tabbed** (General · Reading · Library) and
lands on the tab for the current mode, but all tabs are reachable.

```
        ┌─ Settings ─────────────────────────────────┐
        │  General   [Reading]   Library             │
        │   Measure width        72 cols       ◂     │
        │   Center text          on                  │
        │   Theme                Sepia               │
        │   Line spacing         1                   │
        │   Sidebar default      shown               │
        │   Status bar fields    title · % · gauge   │
        │   PDF default view     text                │
        │   ↑↓ move   ←→ change   Esc close           │
        └─────────────────────────────────────────────┘
```

- **Reading**: measure width, center on/off, theme, line spacing,
  sidebar/bar defaults, status fields, PDF text-vs-image default.
- **Library** (tabs ordered by frequency: View · Columns · General · Sources ·
  Duplicates): the **Sources** tab manages scanned folders — one row per folder
  with `d`/Delete to remove it (which also drops its books), an *Add folder…* row
  (inline path input), and *Rescan now*; plus default view, thumbnail size, sort,
  and visible columns. Adding/removing a folder scans / prunes **in the
  background** (off the UI thread) and refreshes the list live.
- **General** (shared): theme, keybindings, paths, mouse on/off.

## 8. Persistence

Default root: **`~/.config/delryn/`** (single directory, configurable). All
state lives under it:

```
~/.config/delryn/
├── config.toml          # general + per-mode settings
├── library.db           # SQLite: scanned books, metadata, status, favorites, tags
├── sessions.db          # SQLite: per-book resume position, last-read, % read
└── thumbnails/          # rendered covers, keyed by content hash
    └── a1b2c3….png
```

- **Storage**: SQLite (`rusqlite`) for the library index + sessions (sorting,
  search, status flags scale cleanly); plain image files for thumbnails; TOML
  for config.
- **Library sources**: one or more user-specified paths (settings tab and/or
  `delryn --add-library <path>`); recursive scan; rescan on startup + on demand.
- **Note / optional XDG split**: thumbnails and the scanned index are
  regenerable; reading positions and favorites are precious. A future option
  may split per XDG (config → `~/.config/delryn`, precious state →
  `~/.local/state/delryn`, disposable `thumbnails/` → `~/.cache/delryn`) so a
  cache-clear never destroys progress. Root stays overridable either way.

## 9. Navigation — vim + mouse

A single context-aware keymap layer (reader / library / overlay), fully
rebindable in General settings. Defaults:

**Movement (all contexts):** `h j k l` · `gg`/`G` top/bottom ·
`Ctrl-d`/`Ctrl-u` half-page · `Ctrl-f`/`Space` page · `{count}` prefix.

**Reader:** `/` search, `n`/`N` next/prev · `Tab` toggle+focus sidebar ·
`z` immersive · `Enter`/`l` jump TOC, `h` collapse · `m`/`'` set/jump bookmark ·
`;` settings · `q` back to library · `Q` quit.

**Library:** `j k` rows, `h l` columns (grid) · `Enter`/`o` open ·
`v` cycle list/grid · `+`/`-` thumbnail size · `f` favorite · `/` filter ·
`Tab` toggle sections sidebar.

**Overlays/settings:** `j/k` move · `←/→` change value or collapse/expand ·
`Enter` activate · `Tab` switch tabs · `Esc` close.

**Mouse:** wheel scrolls content/list/grid; click — TOC entry → jump, sidebar
section → switch, book → select/open, list column header → sort, settings
tab/row, status-bar hotspots; drag the progress gauge → seek; `+`/`-` grid
control for thumbnail size.

**Mouse-capture tradeoff:** capturing mouse disables the terminal's native text
selection/copy. Mitigations shipped: hold **Shift** to bypass capture and
select natively, plus a **General → Mouse on/off** toggle.

## 10. Dependencies

`ratatui` 0.30.2 · `crossterm` 0.29.0 · `epub` 2.1.5 · `html2text` 0.17.1 ·
`anyhow` 1.0.102. To add: `rusqlite` (persistence), `dirs`/XDG helper (paths),
a terminal image crate for covers (later). EPUB/html2text/ratatui APIs are
version-sensitive — build and fix iteratively rather than trusting recalled
signatures.
