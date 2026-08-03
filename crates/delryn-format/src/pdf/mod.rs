//! PDF backend: a [`Document`] that renders each page to an image (macOS
//! Preview-style), one section per page — preserving the document's real
//! layout rather than reflowing extracted text (PDF v1, rejected).
//!
//! Pages are rasterized by PDFium (`pdfium-render`) and handed to the reader as
//! a single full-bleed [`Block::Image`] ([`ImageWidth::Full`]), so the existing
//! image pipeline does the rest: downscale to the pane, theme-adapt, transmit
//! via the patched Kitty PNG path, cache + neighbour-prefetch. Each page is
//! rasterized once at a generous fixed width, so a resize re-downscales rather
//! than re-rendering.
//!
//! The PDFium library is bound once per process, from the first of these that
//! answers: `DELRYN_PDFIUM_DIR`, beside the running executable (what a release
//! tarball provides), a sibling `../lib`, the per-user library directories, the
//! system library, and finally the copy **embedded in the binary** by `build.rs`
//! — unpacked to `<config>/lib` on first use. Release builds embed it so a
//! `delryn` copied out of its tarball keeps opening PDFs; an ordinary
//! `cargo build` embeds nothing and needs a library placed by hand.
//!
//! When none is found, [`PdfDocument::open`] fails cleanly rather than crashing,
//! and only PDF is affected: EPUB/MOBI still read. `docs/RELEASING.md` has the
//! pinned build and the setup. See also `DESIGN.md` §3.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use anyhow::{Result, anyhow};
use pdfium_render::prelude::{
    PdfBookmark, PdfDocument as PdfiumDoc, PdfDocumentMetadataTagType, PdfRenderConfig, Pdfium,
};

use crate::{
    Block, Document, ImageWidth, Metadata, OutlineItem, PageRasterizer, Section, SectionLoader,
    Span, TocEntry,
};

/// Width, in pixels, each page is rasterized to. Sized so the terminal
/// (which GPU-scales the placement to the display area) still gets a crisp page
/// — including on a hi-DPI display and after a margin trim upscales the content
/// region — while keeping the PNG small enough that the transmit (the cost on a
/// page turn) stays fast. Smaller ⇒ snappier fast navigation but softer text.
/// The quality/perf knob. A viewport-matched re-raster (via [`PageRasterizer`])
/// renders a *larger* crisp raster on top when a page is zoomed in or shown on a
/// large/hi-DPI viewport; this stays the generous baseline every page loads at.
/// Also drives margin-trim decode cost.
pub const PAGE_RASTER_WIDTH: i32 = 2000;

// ---------------------------------------------------------------------------
// PDFium binding (process-global, bound once)
// ---------------------------------------------------------------------------

/// The process-wide PDFium binding, initialized on first use. The `Err` carries
/// a message when no usable library was found, so callers can report cleanly.
static PDFIUM: LazyLock<std::result::Result<Pdfium, String>> = LazyLock::new(bind_pdfium);

/// The shared PDFium binding, or a clean error when the library is unavailable.
fn pdfium() -> Result<&'static Pdfium> {
    PDFIUM.as_ref().map_err(|e| anyhow!("{e}"))
}

/// The `libpdfium` compiled into this binary, or empty when built without one
/// (see `build.rs`). Release builds embed it so the executable keeps opening PDFs
/// after being moved away from the tarball that carried the loose library.
static EMBEDDED_PDFIUM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pdfium-embedded.bin"));

/// Bind to PDFium, trying every source in order of preference: a library already
/// on disk (cheapest and what an intact release tarball provides), then the
/// system library, then — only if none of those exist — the copy embedded in this
/// binary, unpacked to the data directory.
///
/// The embedded copy is deliberately *last*: an intact install never pays the
/// unpack, and someone who deliberately placed a specific build still wins.
fn bind_pdfium() -> std::result::Result<Pdfium, String> {
    let bindings = lib_search_dirs()
        .into_iter()
        .find_map(|dir| {
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&dir)).ok()
        })
        .or_else(|| Pdfium::bind_to_system_library().ok())
        .or_else(|| Pdfium::bind_to_library(unpack_embedded_pdfium()?).ok())
        .ok_or_else(|| {
            // Ordered for truncation: this reaches the user as a status-bar flash,
            // clipped to the width of the row, so the pointer to the fix comes
            // before the detail — at 80 columns "see docs/RELEASING.md" still
            // shows. The old text said to "install libpdfium" without naming a
            // build, a source, or `DELRYN_PDFIUM_DIR`.
            "libpdfium not found — PDF needs it. See docs/RELEASING.md; set \
             DELRYN_PDFIUM_DIR or put it beside the delryn binary."
                .to_string()
        })?;
    Ok(Pdfium::new(bindings))
}

/// Directories searched for a `libpdfium`, most specific first: an explicit
/// `DELRYN_PDFIUM_DIR`, the directory holding the running executable (what a
/// release tarball provides), a sibling `lib/` for a `bin/` + `lib/` install
/// layout, and the usual per-user library locations.
fn lib_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // An empty value means "unset" — `DELRYN_PDFIUM_DIR=` in a wrapper script
    // would otherwise resolve to the current directory and shadow the real ones.
    if let Some(dir) = std::env::var_os("DELRYN_PDFIUM_DIR").filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(bin) = exe.parent()
    {
        dirs.push(bin.to_path_buf());
        // `…/bin/delryn` alongside `…/lib/libpdfium.*`, the shape a package
        // manager or a `--prefix` install produces.
        if let Some(prefix) = bin.parent() {
            dirs.push(prefix.join("lib"));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/lib/delryn"));
    }
    dirs.push(delryn_infra::paths::config_dir().join("lib"));
    dirs
}

/// Unpack [`EMBEDDED_PDFIUM`] into `<config>/lib` and return its path, or `None`
/// when this build embedded nothing.
///
/// Written atomically and reused across runs: the filename carries the byte
/// length, so a delryn built against a different PDFium unpacks alongside rather
/// than racing to overwrite, and two instances starting together can't hand each
/// other a half-written library.
fn unpack_embedded_pdfium() -> Option<PathBuf> {
    if EMBEDDED_PDFIUM.is_empty() {
        return None;
    }
    let dir = delryn_infra::paths::config_dir().join("lib");
    let name = Pdfium::pdfium_platform_library_name();
    let path = dir.join(format!(
        "{}-{}",
        EMBEDDED_PDFIUM.len(),
        name.to_string_lossy()
    ));

    // Already unpacked by an earlier run — the length is in the name, so a
    // matching size means matching content for our purposes.
    let current = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if current != EMBEDDED_PDFIUM.len() as u64 {
        delryn_infra::paths::write_private_atomic(&path, EMBEDDED_PDFIUM).ok()?;
        // `dlopen`/`dyld` don't require the execute bit on a shared library, but
        // some hardened loaders and audit tools expect it; owner-only `rwx` costs
        // nothing and avoids the argument.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
        }
    }
    Some(path)
}

/// Open a PDF through the shared PDFium binding. Each handle is independent (the
/// foreground document and the background loader each open their own), which the
/// default `thread_safe` feature sequences behind a mutex.
fn open_pdfium_doc(path: &Path) -> Result<PdfiumDoc<'static>> {
    let pdfium = pdfium()?;
    pdfium
        .load_pdf_from_file(path, None)
        .map_err(|e| anyhow!("opening PDF {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

/// A PDF opened for reading: one section per page, each rendered to an image.
pub struct PdfDocument {
    path: PathBuf,
    doc: PdfiumDoc<'static>,
    meta: Metadata,
    toc: Vec<TocEntry>,
    outline: Vec<OutlineItem>,
    page_count: usize,
}

impl PdfDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<PdfDocument> {
        let path = path.as_ref();
        let doc = open_pdfium_doc(path)?;
        let page_count = doc.pages().len().max(0) as usize;
        let meta = read_metadata(&doc, path);
        let (toc, outline) = build_navigation(&doc, page_count);
        Ok(PdfDocument {
            path: path.to_path_buf(),
            doc,
            meta,
            toc,
            outline,
            page_count,
        })
    }
}

impl Document for PdfDocument {
    fn metadata(&self) -> &Metadata {
        &self.meta
    }

    fn toc(&self) -> &[TocEntry] {
        &self.toc
    }

    fn outline(&self) -> &[OutlineItem] {
        &self.outline
    }

    fn loader(&self) -> Box<dyn SectionLoader> {
        Box::new(PdfLoader {
            path: self.path.clone(),
            doc: None,
        })
    }

    fn page_rasterizer(&self) -> Option<Box<dyn PageRasterizer>> {
        Some(Box::new(PdfRasterizer {
            path: self.path.clone(),
            doc: None,
        }))
    }

    fn section_count(&self) -> usize {
        self.page_count
    }

    fn paged_image(&self) -> bool {
        true
    }

    fn load_section(&mut self, index: usize) -> Result<Section> {
        Ok(Section {
            index,
            blocks: render_page(&self.doc, index),
        })
    }
}

/// Background loader: opens its own PDFium handle lazily on the loader thread
/// (so it is trivially `Send` at construction), then rasterizes pages on demand
/// for neighbour prefetch.
struct PdfLoader {
    path: PathBuf,
    doc: Option<PdfiumDoc<'static>>,
}

impl SectionLoader for PdfLoader {
    fn load(&mut self, index: usize) -> Vec<Block> {
        if self.doc.is_none() {
            self.doc = open_pdfium_doc(&self.path).ok();
        }
        match self.doc.as_ref() {
            Some(doc) => render_page(doc, index),
            None => vec![render_failed(index)],
        }
    }
}

/// Background rasterizer: re-renders a page at an arbitrary width for the reader's
/// viewport-matched crisp path. Like [`PdfLoader`] it opens its own PDFium handle
/// lazily on its thread (trivially `Send` at construction).
struct PdfRasterizer {
    path: PathBuf,
    doc: Option<PdfiumDoc<'static>>,
}

impl PageRasterizer for PdfRasterizer {
    fn rasterize(&mut self, index: usize, width: u32) -> Option<Vec<u8>> {
        if self.doc.is_none() {
            self.doc = open_pdfium_doc(&self.path).ok();
        }
        rasterize_page_png_at(self.doc.as_ref()?, index, width.min(i32::MAX as u32) as i32)
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render one page to a full-bleed image block; a placeholder paragraph if the
/// page can't be rasterized (out of range, render error, or PNG-encode error).
fn render_page(doc: &PdfiumDoc, index: usize) -> Vec<Block> {
    match rasterize_page_png(doc, index) {
        Some(data) => vec![Block::Image {
            src: String::new(),
            alt: format!("Page {}", index + 1),
            data,
            caption: Vec::new(),
            math: false,
            width: ImageWidth::Full,
            ink: None,
        }],
        None => vec![render_failed(index)],
    }
}

/// Rasterize page `index` to PNG bytes at [`PAGE_RASTER_WIDTH`] (aspect
/// preserved). `None` if the page is out of range or rendering/encoding fails.
fn rasterize_page_png(doc: &PdfiumDoc, index: usize) -> Option<Vec<u8>> {
    rasterize_page_png_at(doc, index, PAGE_RASTER_WIDTH)
}

/// Rasterize page `index` to PNG bytes at `width` px (aspect preserved, height
/// capped at 4×). `None` if the page is out of range or rendering/encoding fails.
fn rasterize_page_png_at(doc: &PdfiumDoc, index: usize, width: i32) -> Option<Vec<u8>> {
    let page = doc.pages().get(index as i32).ok()?;
    let config = PdfRenderConfig::new()
        .set_target_width(width)
        .set_maximum_height(width.saturating_mul(4));
    let bitmap = page.render_with_config(&config).ok()?;
    let image = bitmap.as_image().ok()?;
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// Width (px) for a rendered library cover thumbnail — ample for the largest
/// cover card; the terminal downscales it to the cell box.
const COVER_WIDTH: i32 = 480;

/// Render a PDF's first page to PNG bytes for use as a library cover (a PDF has
/// no embedded cover image — its first page *is* the cover). `None` when the file
/// can't be opened or rasterized (e.g. PDFium unavailable), so the caller can
/// fall back to a placeholder. Opens its own short-lived PDFium handle.
pub fn render_cover(path: impl AsRef<Path>) -> Option<Vec<u8>> {
    // PDFium is a single per-process binding and is not safe to call concurrently. The
    // library cover loader now decodes on a thread pool, so serialise the rasterisation
    // here — EPUB covers still decode in parallel; only PDF pages queue behind this lock.
    static COVER_RENDER_LOCK: Mutex<()> = Mutex::new(());
    let _guard = COVER_RENDER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let doc = open_pdfium_doc(path.as_ref()).ok()?;
    rasterize_page_png_at(&doc, 0, COVER_WIDTH)
}

/// The PDF's bookmark labels (chapter titles), flattened depth-first — clean
/// structured text for content-based duplicate detection. Empty when the PDF has no
/// real outline: a PDF without bookmarks falls back to a synthetic "Page N" TOC
/// (see `flat_page_toc`), which carries no identity, so that case returns nothing.
pub fn toc_labels(path: impl AsRef<Path>) -> Vec<String> {
    let Ok(doc) = PdfDocument::open(path) else {
        return Vec::new();
    };
    let toc = doc.toc();
    if toc.iter().all(|e| is_page_label(&e.label)) {
        return Vec::new(); // synthetic page list, not a real table of contents
    }
    let mut out = Vec::new();
    for entry in toc {
        entry.collect_labels(&mut out);
    }
    out
}

/// A synthetic `flat_page_toc` label, e.g. "Page 12".
fn is_page_label(label: &str) -> bool {
    label
        .strip_prefix("Page ")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Front-matter headings that look prominent but aren't the title — so a large
/// "Contents" or "Preface" heading isn't mistaken for one.
const NON_TITLE_HEADINGS: &[&str] = &[
    "contents",
    "table of contents",
    "preface",
    "introduction",
    "copyright",
    "index",
    "acknowledgments",
    "acknowledgements",
    "dedication",
    "about the author",
    "about the authors",
    "foreword",
    "chapter",
];

/// The book's printed title, read from the PDF's own content (not metadata): the
/// largest-font text run on the first couple of pages — i.e. the title page. `None`
/// for a scanned PDF with no text layer, or when no title-like run is found. Opens
/// its own short-lived PDFium handle.
pub fn extract_title(path: impl AsRef<Path>) -> Option<String> {
    let doc = open_pdfium_doc(path.as_ref()).ok()?;
    let pages = doc.pages();
    let count = pages.len().max(0) as usize;
    let mut best_size = 0.0f32;
    let mut best = String::new();
    for i in 0..count.min(2) {
        let Ok(page) = pages.get(i as i32) else {
            continue;
        };
        let Ok(text) = page.text() else { continue };
        // Walk the page's characters, grouping consecutive ones of the same font
        // size into runs; the largest title-like run across the page wins.
        let mut run_size = -1.0f32;
        let mut run = String::new();
        for ch in text.chars().iter() {
            let size = ch.scaled_font_size().value;
            if (size - run_size).abs() > 0.5 {
                consider_title_run(&mut best_size, &mut best, run_size, &run);
                run.clear();
                run_size = size;
            }
            run.push(ch.unicode_char().unwrap_or(' '));
        }
        consider_title_run(&mut best_size, &mut best, run_size, &run);
    }
    (!best.is_empty()).then_some(best)
}

/// Keep `run` as the best title candidate if it's the largest-font run so far and
/// looks title-like (enough letters, not an absurd paragraph, not a stock heading).
fn consider_title_run(best_size: &mut f32, best: &mut String, size: f32, run: &str) {
    let cleaned = run.split_whitespace().collect::<Vec<_>>().join(" ");
    let alnum = cleaned.chars().filter(|c| c.is_alphanumeric()).count();
    if !(4..=160).contains(&alnum) {
        return;
    }
    let lower = cleaned.to_lowercase();
    if NON_TITLE_HEADINGS
        .iter()
        .any(|h| lower == *h || lower.starts_with(&format!("{h} ")))
    {
        return;
    }
    if size > *best_size + 0.5 {
        *best_size = size;
        *best = cleaned;
    }
}

/// Placeholder shown in place of a page that couldn't be rendered.
fn render_failed(index: usize) -> Block {
    Block::Para {
        spans: vec![Span::plain(format!(
            "[page {} could not be rendered]",
            index + 1
        ))],
        indent: 0,
        quote: false,
        marker: None,
    }
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// Title/author from PDFium's Info dictionary; size from the file. Title falls
/// back to the cleaned file name when the PDF declares none.
fn read_metadata(doc: &PdfiumDoc, path: &Path) -> Metadata {
    let md = doc.metadata();
    let get = |which| {
        md.get(which)
            .map(|tag| tag.value().trim().to_string())
            .filter(|s| !s.is_empty())
    };
    Metadata {
        title: get(PdfDocumentMetadataTagType::Title).unwrap_or_else(|| file_title(path)),
        authors: get(PdfDocumentMetadataTagType::Author)
            .map(|a| vec![a])
            .unwrap_or_default(),
        size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        ..Default::default()
    }
}

/// A display title derived from the file name: stem with `_`/`-` turned to
/// spaces. Falls back to "PDF" for a nameless path.
fn file_title(path: &Path) -> String {
    path.file_stem()
        .map(|s| {
            s.to_string_lossy()
                .replace(['_', '-'], " ")
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "PDF".to_string())
}

// ---------------------------------------------------------------------------
// Navigation (outline)
// ---------------------------------------------------------------------------

/// Maximum PDF outline nesting depth walked, and the maximum siblings honoured at
/// one level. A real outline is shallow with a sane fan-out; these only bound a
/// malformed / malicious PDF whose deeply nested or (should PDFium ever surface
/// one) cyclic bookmark chain would otherwise overflow the stack or loop forever.
const MAX_PDF_OUTLINE_DEPTH: usize = 64;
const MAX_PDF_OUTLINE_SIBLINGS: usize = 100_000;

/// Build the table of contents + flattened sidebar outline from the PDF
/// bookmark tree, falling back to a flat "Page N" list when there is no outline
/// (so the sidebar can still jump to any page).
fn build_navigation(doc: &PdfiumDoc, page_count: usize) -> (Vec<TocEntry>, Vec<OutlineItem>) {
    let bookmarks = doc.bookmarks();
    let mut toc = Vec::new();
    // Top-level bookmarks: `root()` is the first; the rest follow via siblings.
    let mut node = bookmarks.root();
    let mut seen = 0usize;
    while let Some(bm) = node {
        seen += 1;
        if seen > MAX_PDF_OUTLINE_SIBLINGS {
            break; // guard against a pathological / cyclic sibling chain
        }
        let next = bm.next_sibling();
        toc.push(bookmark_to_toc(&bm, 0));
        node = next;
    }
    if toc.is_empty() {
        toc = flat_page_toc(page_count);
    }
    let mut outline = Vec::new();
    flatten_toc(&toc, 0, &mut outline);
    (toc, outline)
}

/// One bookmark (and its descendants) → a [`TocEntry`]. A bookmark with no
/// resolvable destination keeps `section = None` (it still groups its children).
fn bookmark_to_toc(bm: &PdfBookmark, depth: usize) -> TocEntry {
    let section = bm
        .destination()
        .and_then(|d| d.page_index().ok())
        .map(|i| i as usize);
    let mut children = Vec::new();
    // Past the depth cap, keep the entry but stop descending (bounds the stack).
    if depth < MAX_PDF_OUTLINE_DEPTH {
        let mut child = bm.first_child();
        let mut seen = 0usize;
        while let Some(c) = child {
            seen += 1;
            if seen > MAX_PDF_OUTLINE_SIBLINGS {
                break;
            }
            let next = c.next_sibling();
            children.push(bookmark_to_toc(&c, depth + 1));
            child = next;
        }
    }
    TocEntry {
        label: bm.title().unwrap_or_default(),
        section,
        children,
    }
}

/// A flat TOC of one entry per page — the fallback when a PDF has no outline.
fn flat_page_toc(page_count: usize) -> Vec<TocEntry> {
    (0..page_count)
        .map(|p| TocEntry {
            label: format!("Page {}", p + 1),
            section: Some(p),
            children: Vec::new(),
        })
        .collect()
}

/// Flatten a TOC tree into sidebar rows, carrying nesting depth. An entry with
/// no resolved destination still gets a row (anchored to its first page, or 0).
fn flatten_toc(entries: &[TocEntry], depth: usize, out: &mut Vec<OutlineItem>) {
    if depth >= MAX_PDF_OUTLINE_DEPTH {
        return; // the TOC tree is already depth-bounded; defensive stop
    }
    for e in entries {
        out.push(OutlineItem {
            label: e.label.clone(),
            depth,
            section: e.section.unwrap_or(0),
            locator: None,
        });
        flatten_toc(&e.children, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_title_cleans_separators() {
        assert_eq!(
            file_title(Path::new("/books/The_Rust-Book.pdf")),
            "The Rust Book"
        );
        assert_eq!(file_title(Path::new("plain.pdf")), "plain");
        // A path with no stem falls back to a constant rather than empty.
        assert_eq!(file_title(Path::new("")), "PDF");
    }

    #[test]
    fn flat_page_toc_one_entry_per_page() {
        let toc = flat_page_toc(3);
        assert_eq!(toc.len(), 3);
        assert_eq!(toc[0].label, "Page 1");
        assert_eq!(toc[2].section, Some(2));
        assert!(toc[0].children.is_empty());
    }

    #[test]
    fn flatten_toc_carries_depth_and_resolves_sections() {
        let toc = vec![TocEntry {
            label: "Part I".to_string(),
            section: None, // a grouping bookmark with no destination
            children: vec![TocEntry {
                label: "Chapter 1".to_string(),
                section: Some(4),
                children: Vec::new(),
            }],
        }];
        let mut out = Vec::new();
        flatten_toc(&toc, 0, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].depth, out[0].section), (0, 0)); // None → page 0
        assert_eq!((out[1].depth, out[1].section), (1, 4));
        assert_eq!(out[1].label, "Chapter 1");
    }
}
