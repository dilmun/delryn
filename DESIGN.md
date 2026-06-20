# delryn — Design

A terminal reader for EPUB and PDF, written in Rust. This document is the
source of truth for the product/UX design and the high-level architecture.

Status: design locked 2026-06-20. Implementation in progress, **EPUB-first**.

## 1. Scope

**v1 (EPUB-first):**
- Polished reflowable EPUB reader, end to end.
- Library/shelf when run bare (`delryn`), with list and grid (cover) views.
- Resume reading position per book, TOC navigation, in-book full-text search,
  favorites/recents, mode-scoped settings.
- Vim-like keys + mouse.

**Later (same interfaces):**
- PDF behind the same `Document` trait. Hybrid rendering: text-extraction
  reflowable view by default, key-toggle to render the page as an image
  (Kitty / Sixel / iTerm2) via a native engine (`mupdf` or `pdfium-render`).

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
- **View modes** (`v` cycles): **Center** (measure-capped, centered, ~72 cols
  configurable), **Fill** (text fills the pane minus a thin gutter), **Two-page**
  (two side-by-side columns; the right continues from the left so scrolling
  flows left→right). The active mode shows in the status bar.
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

## 6. Library ↔ Reader

Two top-level views behave like **tabs**: bare `delryn` lands on the Library;
opening a book switches to the Reader; you can toggle back to the Library
(`q`) with the book still loaded, and reopen instantly. `Q` quits.

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
- **Library**: library paths (+ add), recursive scan, default view, thumbnail
  size, sort, visible columns.
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
