//! EPUB implementation of [`Document`], backed by the `epub` crate and
//! `html2text` for XHTML → text extraction.

use std::fs::File;
use std::io::BufReader;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use epub::doc::EpubDoc;

use super::{Block, Document, Metadata, OutlineItem, Section, SectionLoader, TocEntry};

mod content_meta;
mod nav;
use content_meta::title_from_html;
pub use content_meta::{ExtractedMeta, extract_book_metadata};

/// Width handed to html2text so paragraphs come back essentially unwrapped; our
/// own layout pass re-wraps them to the actual pane width.
const EXTRACT_WIDTH: usize = 10_000;

pub struct EpubDocument {
    doc: EpubDoc<BufReader<File>>,
    path: PathBuf,
    metadata: Metadata,
    toc: Vec<TocEntry>,
    outline: Vec<OutlineItem>,
    start_section: usize,
}

impl EpubDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let mut doc =
            EpubDoc::new(path).with_context(|| format!("opening EPUB {}", path.display()))?;

        let metadata = extract_metadata(&mut doc, size);
        // Navigation prefers the EPUB 3 nav document, falling back to NCX/spine.
        let navigation = nav::build(&mut doc);

        Ok(Self {
            doc,
            path: path.to_path_buf(),
            metadata,
            toc: navigation.toc,
            outline: navigation.outline,
            start_section: navigation.start_section,
        })
    }
}

/// Background loader: reopens its own `EpubDoc` lazily on first use.
struct EpubLoader {
    path: PathBuf,
    doc: Option<EpubDoc<BufReader<File>>>,
}

impl SectionLoader for EpubLoader {
    fn load(&mut self, index: usize) -> Vec<Block> {
        if self.doc.is_none() {
            self.doc = EpubDoc::new(&self.path).ok();
        }
        match self.doc.as_mut() {
            Some(doc) => load_blocks(doc, index).unwrap_or_default(),
            None => Vec::new(),
        }
    }
}

/// Load and parse one section's blocks from an open `EpubDoc`.
/// Shared by the foreground document and the background loader.
fn load_blocks(doc: &mut EpubDoc<BufReader<File>>, index: usize) -> Result<Vec<Block>> {
    if !doc.set_current_chapter(index) {
        anyhow::bail!("section index {index} out of range");
    }
    let (xhtml, _mime) = doc
        .get_current_str()
        .context("reading current section content")?;
    let mut blocks = super::html::parse_blocks(&xhtml);

    // Resolve each figure image's bytes from the archive.
    let dir = doc
        .get_current_path()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    for block in &mut blocks {
        if let Block::Image { src, data, .. } = block
            && let Some(bytes) = resolve_image(doc, &dir, src)
        {
            *data = bytes;
        }
    }
    Ok(blocks)
}

impl Document for EpubDocument {
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    fn toc(&self) -> &[TocEntry] {
        &self.toc
    }

    fn outline(&self) -> &[OutlineItem] {
        &self.outline
    }

    fn loader(&self) -> Box<dyn SectionLoader> {
        Box::new(EpubLoader {
            path: self.path.clone(),
            doc: None,
        })
    }

    fn section_count(&self) -> usize {
        self.doc.get_num_chapters()
    }

    fn start_section(&self) -> usize {
        self.start_section
    }

    fn load_section(&mut self, index: usize) -> Result<Section> {
        Ok(Section {
            index,
            blocks: load_blocks(&mut self.doc, index)?,
        })
    }

    fn section_for_href(&mut self, from: usize, href: &str) -> Option<usize> {
        // Resolve `href` relative to the linking section's directory → spine index.
        let base = self
            .doc
            .spine
            .get(from)
            .and_then(|item| self.doc.resources.get(&item.idref))
            .and_then(|r| r.path.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        nav::resolve_href(href, &base, &self.doc)
    }

    fn section_targets(&mut self, index: usize) -> Vec<(String, String)> {
        if self.doc.set_current_chapter(index)
            && let Some((xhtml, _mime)) = self.doc.get_current_str()
        {
            super::html::collect_targets(&xhtml)
        } else {
            Vec::new()
        }
    }
}

/// Resolve an image `src` (relative to the chapter dir) to its bytes, with a
/// filename-match fallback for base-path mismatches.
fn resolve_image(doc: &mut EpubDoc<BufReader<File>>, dir: &Path, src: &str) -> Option<Vec<u8>> {
    let src = src.split('#').next().unwrap_or(src);
    let joined = normalize_path(&dir.join(src));
    if let Some(bytes) = doc.get_resource_by_path(&joined) {
        return Some(bytes);
    }
    let fname = Path::new(src).file_name()?;
    let id = doc
        .resources
        .iter()
        .find(|(_, r)| r.path.file_name() == Some(fname))
        .map(|(k, _)| k.clone())?;
    doc.get_resource(&id).map(|(bytes, _)| bytes)
}

/// Resolve `.`/`..` components without touching the filesystem.
fn normalize_path(p: &Path) -> PathBuf {
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c.as_os_str().to_os_string()),
        }
    }
    out.iter().collect()
}

/// Read just the metadata (+ spine length) without parsing TOC/headings.
/// Cheap enough for scanning a large library.
pub fn read_metadata(path: impl AsRef<Path>) -> Result<(Metadata, usize)> {
    let path = path.as_ref();
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut doc = EpubDoc::new(path).with_context(|| format!("opening EPUB {}", path.display()))?;
    let meta = extract_metadata(&mut doc, size);
    let sections = doc.get_num_chapters();
    Ok((meta, sections))
}

/// Extract the whole book's plain text (for full-text indexing).
pub fn read_fulltext(path: impl AsRef<Path>) -> Result<String> {
    let mut doc = EpubDoc::new(path.as_ref())
        .with_context(|| format!("opening EPUB {}", path.as_ref().display()))?;
    let mut out = String::new();
    for i in 0..doc.get_num_chapters() {
        if doc.set_current_chapter(i)
            && let Some((xhtml, _)) = doc.get_current_str()
            && let Ok(text) = html2text::from_read(xhtml.as_bytes(), EXTRACT_WIDTH)
        {
            out.push_str(&text);
            out.push('\n');
        }
    }
    Ok(out)
}

/// The book's table-of-contents labels (chapter titles), flattened depth-first.
/// Clean structured text — no page numbers, images, or symbols — for content-based
/// duplicate detection. Empty if the book can't be opened or has no TOC.
pub fn toc_labels(path: impl AsRef<Path>) -> Vec<String> {
    let Ok(doc) = EpubDocument::open(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in doc.toc() {
        entry.collect_labels(&mut out);
    }
    out
}

/// Best-effort title (and subtitle) guessed from the book's *content* — for
/// converted files whose metadata title and filename are both opaque IDs. Reads
/// the first few content sections and picks the most prominent real heading; a
/// `.subtitle`-classed line or a following sub-heading becomes the subtitle.
pub fn extract_content_title(path: impl AsRef<Path>) -> Option<(String, Option<String>)> {
    let mut doc = EpubDoc::new(path.as_ref()).ok()?;
    let n = doc.get_num_chapters().min(6);
    for i in 0..n {
        if !doc.set_current_chapter(i) {
            continue;
        }
        let Some((xhtml, _)) = doc.get_current_str() else {
            continue;
        };
        if let Some(found) = title_from_html(&xhtml) {
            return Some(found);
        }
    }
    None
}

/// The book's cover image bytes (+ mime). Prefers the EPUB's *declared* cover;
/// for converted files that declare none, falls back to the first image in the
/// opening sections (their cover page), so the library shows a cover too.
pub fn extract_cover(path: impl AsRef<Path>) -> Option<(Vec<u8>, String)> {
    let mut doc = EpubDoc::new(path.as_ref()).ok()?;
    match doc.get_cover() {
        Some(cover) => Some(cover),
        None => first_content_image(&mut doc),
    }
}

/// First embedded image referenced by the book's opening sections (its cover
/// page), resolved to bytes + mime — the cover fallback for files with no
/// declared cover.
fn first_content_image(doc: &mut EpubDoc<BufReader<File>>) -> Option<(Vec<u8>, String)> {
    let n = doc.get_num_chapters().min(3);
    for i in 0..n {
        if !doc.set_current_chapter(i) {
            continue;
        }
        let dir = doc
            .get_current_path()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let Some((xhtml, _)) = doc.get_current_str() else {
            continue;
        };
        let Some(src) = first_img_src(&xhtml) else {
            continue;
        };
        if let Some(bytes) = resolve_image(doc, &dir, &src) {
            return Some((bytes, mime_from_ext(&src).to_string()));
        }
    }
    None
}

/// The source of the first `<img>` or SVG `<image>` in a section.
fn first_img_src(xhtml: &str) -> Option<String> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(xhtml);
    // `<img>` or the SVG `<image>` many EPUB cover pages use; the latter's source
    // is `xlink:href` (local name `href`, or the literal `xlink:href` when not
    // namespaced) rather than `src`.
    let sel = Selector::parse("img, image").ok()?;
    doc.select(&sel).find_map(|e| {
        let e = e.value();
        e.attr("src").map(str::to_string).or_else(|| {
            e.attrs()
                .find(|(k, _)| *k == "href" || k.ends_with(":href"))
                .map(|(_, v)| v.to_string())
        })
    })
}

/// Guess an image mime from a filename extension (defaults to JPEG).
fn mime_from_ext(src: &str) -> &'static str {
    match Path::new(src)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "image/jpeg",
    }
}

fn extract_metadata(doc: &mut EpubDoc<BufReader<File>>, size: u64) -> Metadata {
    let title = doc.get_title().unwrap_or_else(|| "Untitled".to_string());
    let authors = doc
        .metadata
        .iter()
        .filter(|m| m.property == "creator")
        .map(|m| m.value.clone())
        .collect();
    let language = doc.mdata("language").map(|m| m.value.clone());
    let identifier = doc.mdata("identifier").map(|m| m.value.clone());
    // Prefer the publication date; many EPUB3 files only carry the last-modified
    // timestamp (dcterms:modified), so fall back to that for the year.
    let year = doc
        .mdata("date")
        .and_then(|m| parse_year(&m.value))
        .or_else(|| {
            doc.mdata("dcterms:modified")
                .and_then(|m| parse_year(&m.value))
        });
    let publisher = doc.mdata("publisher").map(|m| m.value.trim().to_string());
    let (series, series_index) = extract_series(doc);
    // EPUB has no standard subtitle; Calibre stores one as a refined title.
    let subtitle = doc
        .metadata
        .iter()
        .find(|m| {
            m.property == "title"
                && m.refinement("title-type")
                    .is_some_and(|r| r.value == "subtitle")
        })
        .map(|m| m.value.trim().to_string());
    let cover = doc.get_cover();
    let converted = detect_converted(doc);
    Metadata {
        title,
        subtitle,
        authors,
        year,
        language,
        identifier,
        series,
        series_index,
        publisher,
        cover,
        size,
        converted,
    }
}

/// Heuristic: does this EPUB look converted/repackaged rather than an original
/// publisher file? Signals (from OPF metadata): a calibre fingerprint (any
/// `calibre:*` meta, or calibre named as the book producer), another conversion
/// tool named in `generator`/`contributor`, or an Amazon `asin` identifier
/// (the file was made from a Kindle edition). A clean publisher EPUB — including
/// ones authored in InDesign/Sigil or with no generator tag — is not flagged.
fn detect_converted(doc: &EpubDoc<BufReader<File>>) -> bool {
    let calibre_ns = doc
        .metadata
        .iter()
        .any(|m| m.property.starts_with("calibre:"));
    converted_from(
        calibre_ns,
        doc.mdata("generator").map(|m| m.value.as_str()),
        doc.mdata("contributor").map(|m| m.value.as_str()),
        doc.mdata("identifier").map(|m| m.value.as_str()),
    )
}

/// Substrings that name a format-conversion / repackaging tool.
const CONVERTERS: [&str; 11] = [
    "calibre",
    "pandoc",
    "ebook-convert",
    "aspose",
    "kindlegen",
    "mobi",
    "abbyy",
    "able2extract",
    "ghostscript",
    "wkhtmltopdf",
    "pdftoepub",
];

/// The pure decision behind [`detect_converted`], from the few OPF fields that
/// carry a provenance signal. `calibre_ns` is whether any `calibre:*` metadata
/// is present.
fn converted_from(
    calibre_ns: bool,
    generator: Option<&str>,
    contributor: Option<&str>,
    identifier: Option<&str>,
) -> bool {
    if calibre_ns {
        return true;
    }
    let names_tool = |s: Option<&str>| {
        s.map(|v| {
            let v = v.to_lowercase();
            CONVERTERS.iter().any(|t| v.contains(t))
        })
        .unwrap_or(false)
    };
    if names_tool(generator) || names_tool(contributor) {
        return true;
    }
    // An Amazon ASIN identifier ⇒ the EPUB was made from a Kindle edition.
    identifier
        .map(|v| v.to_lowercase().contains("asin"))
        .unwrap_or(false)
}

/// Series name + position, from either Calibre's legacy `<meta name="calibre:series">`
/// (the common case in the wild) or EPUB3's `belongs-to-collection` with a
/// `group-position` refinement.
fn extract_series(doc: &EpubDoc<BufReader<File>>) -> (Option<String>, Option<f32>) {
    if let Some(s) = doc.mdata("calibre:series") {
        let idx = doc
            .mdata("calibre:series_index")
            .and_then(|m| m.value.trim().parse().ok());
        return (Some(s.value.trim().to_string()), idx);
    }
    if let Some(c) = doc
        .metadata
        .iter()
        .find(|m| m.property == "belongs-to-collection")
    {
        let idx = c
            .refinement("group-position")
            .and_then(|r| r.value.trim().parse().ok());
        return (Some(c.value.trim().to_string()), idx);
    }
    (None, None)
}

/// Dates look like "1851", "1851-01-01", or "-800"; take the leading integer.
fn parse_year(date: &str) -> Option<i32> {
    let s = date.trim();
    let end = s
        .char_indices()
        .skip(1) // tolerate a leading '-'
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    s[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{converted_from, first_img_src, mime_from_ext};

    #[test]
    fn cover_fallback_helpers() {
        assert_eq!(
            first_img_src("<html><body><img src=\"images/cover.jpeg\" class=\"c\"/></body></html>"),
            Some("images/cover.jpeg".into())
        );
        // SVG-wrapped cover (xlink:href) — common in EPUB cover pages.
        assert_eq!(
            first_img_src(
                "<html><body><svg><image xlink:href=\"css/cover.jpeg\"/></svg></body></html>"
            ),
            Some("css/cover.jpeg".into())
        );
        assert_eq!(
            first_img_src("<html><body><p>no image</p></body></html>"),
            None
        );
        assert_eq!(mime_from_ext("a/b/cover.PNG"), "image/png");
        assert_eq!(mime_from_ext("cover.jpg"), "image/jpeg");
        assert_eq!(mime_from_ext("cover"), "image/jpeg");
    }

    #[test]
    fn flags_calibre_and_conversion_tools() {
        // calibre namespace present → converted.
        assert!(converted_from(true, None, None, None));
        // calibre named as the book producer (contributor bkp).
        assert!(converted_from(
            false,
            None,
            Some("calibre (3.3.0) [https://calibre-ebook.com]"),
            None
        ));
        // other conversion tools in the generator.
        assert!(converted_from(
            false,
            Some("Aspose.Words for .NET"),
            None,
            None
        ));
        assert!(converted_from(false, Some("pandoc"), None, None));
        // an Amazon ASIN identifier ⇒ made from a Kindle edition.
        assert!(converted_from(
            false,
            None,
            None,
            Some("urn:asin:B0CRR19Z5D")
        ));
    }

    #[test]
    fn leaves_publisher_files_unflagged() {
        // Adobe InDesign / Sigil are authoring tools, not conversions.
        assert!(!converted_from(
            false,
            Some("Adobe InDesign 19.5.1"),
            None,
            None
        ));
        assert!(!converted_from(false, None, Some("Sigil 2.2.1"), None));
        // No generator tag at all (typical academic/publisher EPUB) + a real ISBN.
        assert!(!converted_from(
            false,
            None,
            None,
            Some("urn:isbn:978-3-031-61037-0")
        ));
        assert!(!converted_from(false, None, None, None));
    }
}
