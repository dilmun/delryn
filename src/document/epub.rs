//! EPUB implementation of [`Document`], backed by the `epub` crate and
//! `html2text` for XHTML → text extraction.

use std::fs::File;
use std::io::BufReader;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use epub::doc::{EpubDoc, NavPoint};

use super::{
    Block, Document, Metadata, OutlineItem, Section, SectionLoader, TocEntry,
};

/// Width handed to html2text so paragraphs come back essentially unwrapped; our
/// own layout pass re-wraps them to the actual pane width.
const EXTRACT_WIDTH: usize = 10_000;

pub struct EpubDocument {
    doc: EpubDoc<BufReader<File>>,
    path: PathBuf,
    metadata: Metadata,
    toc: Vec<TocEntry>,
    outline: Vec<OutlineItem>,
}

impl EpubDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let mut doc = EpubDoc::new(path)
            .with_context(|| format!("opening EPUB {}", path.display()))?;

        let metadata = extract_metadata(&mut doc, size);
        let toc: Vec<TocEntry> = doc.toc.iter().map(|np| convert_navpoint(np, &doc)).collect();

        // The navigable outline mirrors the book's own table of contents,
        // preserving its hierarchy (e.g. Part → Chapter → section). Works for
        // both multi-file books and single-file books (where every entry points
        // into one section at a different anchor).
        let mut outline = Vec::new();
        build_outline(&toc, 0, 0, &mut outline);
        if outline.is_empty() {
            // No usable TOC: fall back to one entry per spine section.
            outline = (0..doc.get_num_chapters())
                .map(|s| OutlineItem {
                    label: format!("Section {}", s + 1),
                    depth: 0,
                    section: s,
                    locator: None,
                })
                .collect();
        }

        Ok(Self {
            doc,
            path: path.to_path_buf(),
            metadata,
            toc,
            outline,
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
        if let Block::Image { src, data, .. } = block {
            if let Some(bytes) = resolve_image(doc, &dir, src) {
                *data = bytes;
            }
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

    fn load_section(&mut self, index: usize) -> Result<Section> {
        Ok(Section {
            index,
            blocks: load_blocks(&mut self.doc, index)?,
        })
    }

    fn section_images(&mut self, section: usize) -> Vec<Vec<u8>> {
        // Reuse the parsed blocks so the overlay sees exactly the figures the
        // reader renders inline (single source of truth for image selection).
        load_blocks(&mut self.doc, section)
            .map(|blocks| {
                blocks
                    .into_iter()
                    .filter_map(|b| match b {
                        Block::Image { data, .. } if !data.is_empty() => Some(data),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
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
    let mut doc =
        EpubDoc::new(path).with_context(|| format!("opening EPUB {}", path.display()))?;
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
        if doc.set_current_chapter(i) {
            if let Some((xhtml, _)) = doc.get_current_str() {
                if let Ok(text) = html2text::from_read(xhtml.as_bytes(), EXTRACT_WIDTH) {
                    out.push_str(&text);
                    out.push('\n');
                }
            }
        }
    }
    Ok(out)
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

/// Most leaf blocks a section can have and still be treated as a title page
/// rather than a chapter (so chapter body text is never mistaken for a title).
const TITLE_PAGE_MAX_BLOCKS: usize = 14;

/// Pull a title (+ optional subtitle) out of one XHTML section. A title page
/// keeps the title, subtitle and author in *separate* block elements — headings
/// in clean files, plain `<div>`/`<p>` lines in converted (calibre) ones. Take
/// the first real line as the title and the next as the subtitle, stopping at a
/// separator line (`—`, `by`, …) so the author isn't captured. Falls back to a
/// non-generic `<title>` element.
fn title_from_html(xhtml: &str) -> Option<(String, Option<String>)> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(xhtml);
    let collapse = |e: scraper::ElementRef| {
        e.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
    };

    // Leaf block lines in document order (a block with no block descendant), so
    // a wrapping <div> doesn't swallow every line into one.
    let block_sel = Selector::parse("h1,h2,h3,h4,h5,h6,p,div,li").ok()?;
    let lines: Vec<String> = doc
        .select(&block_sel)
        .filter(|e| e.select(&block_sel).next().is_none())
        .map(collapse)
        .filter(|s| !s.is_empty())
        .collect();

    // Only treat a short section as a title page; a long one is a chapter.
    if !lines.is_empty() && lines.len() <= TITLE_PAGE_MAX_BLOCKS {
        // Everything up to the first separator line is the title/subtitle block.
        let end = lines.iter().position(|l| is_separator_line(l)).unwrap_or(lines.len());
        let area = &lines[..end];
        if let Some(ti) = area.iter().position(|l| is_title_candidate(l)) {
            let title = area[ti].clone();
            let subtitle = area.get(ti + 1).filter(|l| is_title_candidate(l)).cloned();
            return Some((title, subtitle));
        }
    }

    let head_title = doc
        .select(&Selector::parse("title").ok()?)
        .map(collapse)
        .find(|s| is_title_candidate(s))?;
    Some((head_title, None))
}

/// A line that divides the title block from the author (`—`, `-`, `·`, `by`).
fn is_separator_line(l: &str) -> bool {
    let t = l.trim();
    t.eq_ignore_ascii_case("by") || (!t.is_empty() && t.chars().all(|c| !c.is_alphanumeric()))
}

/// Is `t` a plausible book title rather than an opaque ID or generic front-matter
/// label (Cover / Contents / Copyright …)?
fn is_title_candidate(t: &str) -> bool {
    let t = t.trim();
    // Needs a few real letters — rejects bare numbers / IDs.
    if t.chars().filter(|c| c.is_alphabetic()).count() < 3 {
        return false;
    }
    const GENERIC: [&str; 14] = [
        "cover",
        "title page",
        "title",
        "contents",
        "table of contents",
        "copyright",
        "copyright page",
        "dedication",
        "acknowledgments",
        "acknowledgements",
        "index",
        "preface",
        "foreword",
        "about the author",
    ];
    let low = t.to_lowercase();
    !GENERIC.contains(&low.as_str())
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
        .or_else(|| doc.mdata("dcterms:modified").and_then(|m| parse_year(&m.value)));
    let publisher = doc.mdata("publisher").map(|m| m.value.trim().to_string());
    let (series, series_index) = extract_series(doc);
    // EPUB has no standard subtitle; Calibre stores one as a refined title.
    let subtitle = doc
        .metadata
        .iter()
        .find(|m| m.property == "title" && m.refinement("title-type").is_some_and(|r| r.value == "subtitle"))
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
    let calibre_ns = doc.metadata.iter().any(|m| m.property.starts_with("calibre:"));
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
    identifier.map(|v| v.to_lowercase().contains("asin")).unwrap_or(false)
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

fn convert_navpoint(np: &NavPoint, doc: &EpubDoc<BufReader<File>>) -> TocEntry {
    TocEntry {
        label: np.label.clone(),
        section: resolve_section(&np.content, doc),
        children: np
            .children
            .iter()
            .map(|c| convert_navpoint(c, doc))
            .collect(),
    }
}

/// Map a navpoint resource path to a spine index, tolerating `#fragment`s and
/// base-path mismatches by falling back to a file-name match.
fn resolve_section(content: &Path, doc: &EpubDoc<BufReader<File>>) -> Option<usize> {
    let raw = content.to_string_lossy();
    let raw = raw.split('#').next().unwrap_or(&raw);
    let path = PathBuf::from(raw);

    if let Some(i) = doc.resource_uri_to_chapter(&path) {
        return Some(i);
    }

    let target = path.file_name()?;
    doc.spine.iter().position(|item| {
        doc.resources
            .get(&item.idref)
            .is_some_and(|res| res.path.file_name() == Some(target))
    })
}

/// Flatten the book's TOC tree into a depth-tagged outline, preserving its
/// hierarchy. Each entry carries the label as a locator so the reader can scroll
/// to the matching heading within the (possibly shared) section; entries with no
/// resolved section inherit their parent's.
fn build_outline(
    entries: &[TocEntry],
    depth: usize,
    parent_section: usize,
    out: &mut Vec<OutlineItem>,
) {
    for e in entries {
        let section = e.section.unwrap_or(parent_section);
        out.push(OutlineItem {
            label: e.label.clone(),
            depth,
            section,
            // Locate the entry's heading text within its section (handles
            // single-file books where many entries share one section). Falls
            // back to the section top when the text isn't found.
            locator: Some(e.label.clone()),
        });
        build_outline(&e.children, depth + 1, section, out);
    }
}

#[cfg(test)]
mod tests {
    use super::{converted_from, title_from_html};

    #[test]
    fn content_title_from_headings() {
        // h1 title + h2 subtitle.
        let html = "<html><body><h1>Building Chatbots with Python</h1>\
            <h2>Using NLP and Machine Learning</h2></body></html>";
        assert_eq!(
            title_from_html(html),
            Some(("Building Chatbots with Python".into(), Some("Using NLP and Machine Learning".into())))
        );

        // Calibre-style title page: plain <div> lines, title and subtitle split
        // into separate blocks, an em-dash separator before the author. Only the
        // title + subtitle are captured — never the author.
        let calibre = "<html><body><div class=\"c1\">\
            <div class=\"c2\">Building Chatbots with Python</div>\
            <div class=\"c2\">Using Natural Language Processing and Machine Learning</div>\
            <div class=\"c2\">—</div>\
            <div class=\"c2\">Sumit Raj</div></div></body></html>";
        assert_eq!(
            title_from_html(calibre),
            Some((
                "Building Chatbots with Python".into(),
                Some("Using Natural Language Processing and Machine Learning".into())
            ))
        );

        // A leading generic block (Cover) is skipped.
        let html = "<html><body><h1>Cover</h1><h1>Deep Learning for Data Architects</h1>\
            <p>Unleash the power of Python</p></body></html>";
        assert_eq!(
            title_from_html(html),
            Some((
                "Deep Learning for Data Architects".into(),
                Some("Unleash the power of Python".into())
            ))
        );

        // No usable block → fall back to a non-generic <title>.
        let html =
            "<html><head><title>A Real Book Title</title></head><body><p>x</p></body></html>";
        assert_eq!(title_from_html(html), Some(("A Real Book Title".into(), None)));

        // An ID-ish heading is not a title.
        assert_eq!(title_from_html("<html><body><h1>503392068</h1></body></html>"), None);
        // A long section is a chapter, not a title page → ignored.
        let chapter = format!("<html><body>{}</body></html>", "<p>line</p>".repeat(20));
        assert_eq!(title_from_html(&chapter), None);
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
        assert!(converted_from(false, Some("Aspose.Words for .NET"), None, None));
        assert!(converted_from(false, Some("pandoc"), None, None));
        // an Amazon ASIN identifier ⇒ made from a Kindle edition.
        assert!(converted_from(false, None, None, Some("urn:asin:B0CRR19Z5D")));
    }

    #[test]
    fn leaves_publisher_files_unflagged() {
        // Adobe InDesign / Sigil are authoring tools, not conversions.
        assert!(!converted_from(false, Some("Adobe InDesign 19.5.1"), None, None));
        assert!(!converted_from(false, None, Some("Sigil 2.2.1"), None));
        // No generator tag at all (typical academic/publisher EPUB) + a real ISBN.
        assert!(!converted_from(false, None, None, Some("urn:isbn:978-3-031-61037-0")));
        assert!(!converted_from(false, None, None, None));
    }
}
