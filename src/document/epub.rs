//! EPUB implementation of [`Document`], backed by the `epub` crate and
//! `html2text` for XHTML → text extraction.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use epub::doc::{EpubDoc, NavPoint};

use super::{
    Block, Document, Metadata, OutlineItem, Section, SectionLoader, TocEntry, normalize_label,
};

/// A heading found in a section's XHTML.
struct Heading {
    level: u8,
    text: String,
}

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

        // Curated section labels from the book's TOC, plus in-document headings
        // for every section; merged into one flat, navigable outline.
        let mut labels = HashMap::new();
        collect_labels(&toc, &mut labels);
        let headings = collect_headings(&mut doc);
        let outline = build_outline(doc.get_num_chapters(), &labels, &headings);

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
    Ok(super::html::parse_blocks(&xhtml))
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
    let cover = doc.get_cover();
    Metadata {
        title,
        authors,
        year,
        language,
        identifier,
        cover,
        size,
    }
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

/// First (shallowest) TOC label for each section it resolves to.
fn collect_labels(entries: &[TocEntry], out: &mut HashMap<usize, String>) {
    for e in entries {
        if let Some(s) = e.section {
            out.entry(s).or_insert_with(|| e.label.clone());
        }
        collect_labels(&e.children, out);
    }
}

/// Cheap heading scan for every section, in spine order.
fn collect_headings(doc: &mut EpubDoc<BufReader<File>>) -> Vec<Vec<Heading>> {
    let n = doc.get_num_chapters();
    let mut all = Vec::with_capacity(n);
    for i in 0..n {
        let headings = if doc.set_current_chapter(i) {
            doc.get_current_str()
                .map(|(xhtml, _)| scan_headings(&xhtml))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        all.push(headings);
    }
    all
}

/// Find `<h1>…<h6>` elements and their (cleaned) text in document order.
fn scan_headings(xhtml: &str) -> Vec<Heading> {
    let lower = xhtml.to_ascii_lowercase();
    let lb = lower.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(rel) = lower[i..].find("<h") {
        let p = i + rel;
        let level = lb.get(p + 2).copied().unwrap_or(0);
        let after = lb.get(p + 3).copied().unwrap_or(b' ');
        let is_heading = (b'1'..=b'6').contains(&level)
            && matches!(after, b'>' | b' ' | b'\t' | b'\n' | b'\r' | b'/');
        if is_heading {
            if let Some(gt) = lower[p..].find('>') {
                let content_start = p + gt + 1;
                let close = format!("</h{}", level as char);
                if let Some(crel) = lower[content_start..].find(&close) {
                    let text = heading_text(&xhtml[content_start..content_start + crel]);
                    if !text.is_empty() {
                        out.push(Heading {
                            level: level - b'0',
                            text,
                        });
                    }
                    i = content_start + crel;
                    continue;
                }
            }
        }
        i = p + 2;
    }
    out
}

/// Strip tags / decode entities from a heading's inner HTML via html2text.
fn heading_text(inner: &str) -> String {
    html2text::from_read(inner.as_bytes(), EXTRACT_WIDTH)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// One flat outline: each section as a top-level row (labeled from the TOC,
/// else its first heading, else "Section N"), with its headings nested beneath
/// by relative level. Jumping to a heading locates its text in the page.
fn build_outline(
    section_count: usize,
    labels: &HashMap<usize, String>,
    headings: &[Vec<Heading>],
) -> Vec<OutlineItem> {
    let mut out = Vec::new();
    for s in 0..section_count {
        let section_headings = headings.get(s).map(Vec::as_slice).unwrap_or(&[]);
        let label = labels
            .get(&s)
            .cloned()
            .or_else(|| section_headings.first().map(|h| h.text.clone()))
            .unwrap_or_else(|| format!("Section {}", s + 1));

        out.push(OutlineItem {
            label: label.clone(),
            depth: 0,
            section: s,
            locator: None,
        });

        let norm_label = normalize_label(&label);
        let min_level = section_headings.iter().map(|h| h.level).min().unwrap_or(1);
        for h in section_headings {
            // Skip a heading that just repeats the section title.
            if normalize_label(&h.text) == norm_label {
                continue;
            }
            out.push(OutlineItem {
                label: h.text.clone(),
                depth: 1 + (h.level.saturating_sub(min_level)) as usize,
                section: s,
                locator: Some(h.text.clone()),
            });
        }
    }
    out
}
