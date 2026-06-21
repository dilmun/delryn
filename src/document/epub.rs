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
    let year = doc.mdata("date").and_then(|m| parse_year(&m.value));
    let publisher = doc.mdata("publisher").map(|m| m.value.trim().to_string());
    let (series, series_index) = extract_series(doc);
    let cover = doc.get_cover();
    Metadata {
        title,
        authors,
        year,
        language,
        identifier,
        series,
        series_index,
        publisher,
        cover,
        size,
    }
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
