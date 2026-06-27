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
//! The PDFium library is bound once per process — a bundled `libpdfium` beside
//! the binary (the shipped configuration), then the system library. When it's
//! absent, [`PdfDocument::open`] fails cleanly rather than crashing. See
//! `DESIGN.md` §3 / the Phase 5 plan in `TODO.md`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use pdfium_render::prelude::{
    PdfBookmark, PdfDocument as PdfiumDoc, PdfDocumentMetadataTagType, PdfRenderConfig, Pdfium,
};

use crate::{
    Block, Document, ImageWidth, Metadata, OutlineItem, Section, SectionLoader, Span, TocEntry,
};

/// Width, in pixels, each page is rasterized to. Sized so the terminal
/// (which GPU-scales the placement to the display area) still gets a crisp
/// page, while keeping the PNG small enough that the transmit — the cost on a
/// page turn — is fast and reliable. Smaller ⇒ snappier fast navigation; a
/// half-screen spread page only displays ~700px wide, so 1400 super-samples it.
/// The v2 quality/perf knob.
const PAGE_RASTER_WIDTH: i32 = 1400;

/// Cap the rasterized height so a pathologically tall page can't allocate a
/// huge bitmap; 4× the width covers any real page aspect ratio.
const PAGE_RASTER_MAX_HEIGHT: i32 = PAGE_RASTER_WIDTH * 4;

// ---------------------------------------------------------------------------
// PDFium binding (process-global, bound once)
// ---------------------------------------------------------------------------

/// The process-wide PDFium binding, initialized on first use. The `Err` carries
/// a message when no usable library was found, so callers can report cleanly.
static PDFIUM: OnceLock<std::result::Result<Pdfium, String>> = OnceLock::new();

/// The shared PDFium binding, or a clean error when the library is unavailable.
fn pdfium() -> Result<&'static Pdfium> {
    PDFIUM
        .get_or_init(bind_pdfium)
        .as_ref()
        .map_err(|e| anyhow!("{e}"))
}

/// Bind to PDFium: a bundled `libpdfium` beside the executable first (the
/// shipped configuration), then the system-installed library.
fn bind_pdfium() -> std::result::Result<Pdfium, String> {
    let bindings = bundled_lib_dir()
        .and_then(|dir| {
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&dir)).ok()
        })
        .or_else(|| Pdfium::bind_to_system_library().ok())
        .ok_or_else(|| {
            "PDFium library not found — install libpdfium or place it beside the delryn binary"
                .to_string()
        })?;
    Ok(Pdfium::new(bindings))
}

/// Where to look for a bundled `libpdfium`: an explicit `DELRYN_PDFIUM_DIR`
/// override, else the directory containing the running executable.
fn bundled_lib_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DELRYN_PDFIUM_DIR") {
        return Some(PathBuf::from(dir));
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
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
        }],
        None => vec![render_failed(index)],
    }
}

/// Rasterize page `index` to PNG bytes at [`PAGE_RASTER_WIDTH`] (aspect
/// preserved). `None` if the page is out of range or rendering/encoding fails.
fn rasterize_page_png(doc: &PdfiumDoc, index: usize) -> Option<Vec<u8>> {
    let page = doc.pages().get(index as i32).ok()?;
    let config = PdfRenderConfig::new()
        .set_target_width(PAGE_RASTER_WIDTH)
        .set_maximum_height(PAGE_RASTER_MAX_HEIGHT);
    let bitmap = page.render_with_config(&config).ok()?;
    let image = bitmap.as_image().ok()?;
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
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

/// Build the table of contents + flattened sidebar outline from the PDF
/// bookmark tree, falling back to a flat "Page N" list when there is no outline
/// (so the sidebar can still jump to any page).
fn build_navigation(doc: &PdfiumDoc, page_count: usize) -> (Vec<TocEntry>, Vec<OutlineItem>) {
    let bookmarks = doc.bookmarks();
    let mut toc = Vec::new();
    // Top-level bookmarks: `root()` is the first; the rest follow via siblings.
    let mut node = bookmarks.root();
    while let Some(bm) = node {
        let next = bm.next_sibling();
        toc.push(bookmark_to_toc(&bm));
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
fn bookmark_to_toc(bm: &PdfBookmark) -> TocEntry {
    let section = bm
        .destination()
        .and_then(|d| d.page_index().ok())
        .map(|i| i as usize);
    let mut children = Vec::new();
    let mut child = bm.first_child();
    while let Some(c) = child {
        let next = c.next_sibling();
        children.push(bookmark_to_toc(&c));
        child = next;
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
