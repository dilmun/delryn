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

/// All metadata delryn can recover from the book's *own content* — for converted
/// files whose OPF metadata is junk and that aren't findable online. Every field
/// is best-effort and meant to be reviewed before saving.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ExtractedMeta {
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub author: Option<String>,
    pub year: Option<i32>,
    pub publisher: Option<String>,
    pub isbn: Option<String>,
}

/// Scan the book's front matter for as much metadata as possible: the title page
/// gives title/subtitle/author; the copyright/imprint page gives year, publisher
/// and ISBN. Reads only the first few sections.
pub fn extract_book_metadata(path: impl AsRef<Path>) -> ExtractedMeta {
    let mut out = ExtractedMeta::default();
    let Ok(mut doc) = EpubDoc::new(path.as_ref()) else {
        return out;
    };
    let n = doc.get_num_chapters().min(8);
    for i in 0..n {
        if !doc.set_current_chapter(i) {
            continue;
        }
        let Some((xhtml, _)) = doc.get_current_str() else {
            continue;
        };
        if out.title.is_none() {
            let page = parse_title_page(&xhtml);
            if let Some((t, s, a)) = page {
                out.title = Some(t);
                out.subtitle = s;
                out.author = a;
            }
        }
        if out.year.is_none() || out.publisher.is_none() || out.isbn.is_none() {
            let text = html2text::from_read(xhtml.as_bytes(), EXTRACT_WIDTH).unwrap_or_default();
            out.isbn = out.isbn.or_else(|| find_isbn(&text));
            out.year = out.year.or_else(|| find_year(&text));
            out.publisher = out.publisher.or_else(|| find_publisher(&text));
        }
        if out.title.is_some() && out.isbn.is_some() && out.year.is_some() {
            break;
        }
    }
    out
}

/// Leaf block-element lines of a section in document order — a block with no
/// block descendant, so a wrapping `<div>` doesn't swallow every line into one.
fn leaf_block_lines(xhtml: &str) -> Vec<String> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(xhtml);
    let Ok(sel) = Selector::parse("h1,h2,h3,h4,h5,h6,p,div,li") else {
        return Vec::new();
    };
    doc.select(&sel)
        .filter(|e| e.select(&sel).next().is_none())
        .map(|e| e.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse a title-page-like section into (title, subtitle, author). A title page
/// keeps each in a *separate* block — headings in clean files, plain `<div>`/`<p>`
/// lines in converted (calibre) ones. The first real line is the title and the
/// next the subtitle; a separator line (`—`, `by`, …) ends the title block, and
/// the first real line after it is the author.
fn parse_title_page(xhtml: &str) -> Option<(String, Option<String>, Option<String>)> {
    let lines = leaf_block_lines(xhtml);
    // Only a short section is a title page; a long one is a chapter.
    if lines.is_empty() || lines.len() > TITLE_PAGE_MAX_BLOCKS {
        return None;
    }
    let sep = lines.iter().position(|l| is_separator_line(l));
    let area = &lines[..sep.unwrap_or(lines.len())];
    let ti = area.iter().position(|l| is_title_candidate(l))?;
    let title = area[ti].clone();
    let subtitle = area.get(ti + 1).filter(|l| is_title_candidate(l)).cloned();
    let author = sep.and_then(|s| lines[s + 1..].iter().find(|l| is_title_candidate(l)).cloned());
    Some((title, subtitle, author))
}

/// Pull a title (+ optional subtitle) out of one XHTML section: the title page if
/// there is one, else a non-generic `<title>` element.
fn title_from_html(xhtml: &str) -> Option<(String, Option<String>)> {
    use scraper::{Html, Selector};
    if let Some((title, subtitle, _)) = parse_title_page(xhtml) {
        return Some((title, subtitle));
    }
    let doc = Html::parse_document(xhtml);
    let head_title = doc
        .select(&Selector::parse("title").ok()?)
        .map(|e| e.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|s| is_title_candidate(s))?;
    Some((head_title, None))
}

/// First labelled ISBN in `text`, normalized to a bare ISBN-10/13. Tolerates the
/// usual clutter between the label and the number (`ISBN-13 (pbk): 978-…`).
fn find_isbn(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?i)ISBN.{0,30}?([0-9][0-9\- ]{8,18}[0-9Xx])").ok()?;
    re.captures_iter(text).find_map(|c| crate::online::normalize_isbn(&c[1]))
}

/// A publication year near a copyright / "published" marker.
fn find_year(text: &str) -> Option<i32> {
    let re = regex::Regex::new(
        r"(?i)(?:copyright|©|\(c\)|first published|published)\D{0,24}((?:19|20)\d{2})",
    )
    .ok()?;
    re.captures(text).and_then(|c| c[1].parse().ok())
}

/// The publisher named on the imprint page. A well-known publisher mentioned in
/// the front matter wins (most reliable); else an explicit "Published by X" line
/// — but only when it reads like a name (Title-Case, no sentence verbs), so the
/// boilerplate "Neither the publisher … can accept responsibility" is ignored.
fn find_publisher(text: &str) -> Option<String> {
    const PUBS: [&str; 12] = [
        "Apress",
        "O'Reilly Media",
        "O'Reilly",
        "Springer",
        "Packt Publishing",
        "Packt",
        "Manning",
        "No Starch Press",
        "BPB",
        "Wiley",
        "Pearson",
        "Addison-Wesley",
    ];
    let low = text.to_lowercase();
    if let Some(p) = PUBS.iter().find(|p| low.contains(&p.to_lowercase())) {
        return Some(p.to_string());
    }
    let labelled = regex::Regex::new(r"(?i)published by\s+([^\n\r.,;]{2,50})")
        .ok()
        .and_then(|re| re.captures(text).map(|c| c[1].to_string()))?;
    let p = labelled.trim().to_string();
    // Reject sentence fragments (a publisher name has no lowercase sentence words).
    let looks_like_name = !p.is_empty()
        && p.split_whitespace().count() <= 6
        && !p.split_whitespace().any(|w| matches!(w, "can" | "the" | "and" | "any" | "for" | "nor" | "of"));
    looks_like_name.then_some(p)
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
    use super::{converted_from, find_isbn, find_publisher, find_year, title_from_html};

    #[test]
    fn content_metadata_scanners() {
        // ISBN: tolerate label clutter, normalize to bare digits.
        assert_eq!(find_isbn("ISBN-13 (pbk): 978-1-4842-4096-0"), Some("9781484240960".into()));
        assert_eq!(find_isbn("eISBN: 9789355515391"), Some("9789355515391".into()));
        assert_eq!(find_isbn("no number here"), None);
        // Year: only near a copyright / publication marker.
        assert_eq!(find_year("Copyright © 2019 by Sumit Raj"), Some(2019));
        assert_eq!(find_year("First published 2024 by BPB"), Some(2024));
        assert_eq!(find_year("see you in 2030 maybe"), None);
        // Publisher: known names win; sentence boilerplate is rejected.
        assert_eq!(find_publisher("© 2019 Apress Media LLC"), Some("Apress".into()));
        assert_eq!(find_publisher("Published by Acme Press, London"), Some("Acme Press".into()));
        assert_eq!(
            find_publisher("Neither the publisher nor the author can accept responsibility"),
            None
        );
    }

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
