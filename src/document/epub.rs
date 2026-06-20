//! EPUB implementation of [`Document`], backed by the `epub` crate and
//! `html2text` for XHTML → text extraction.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use epub::doc::{EpubDoc, NavPoint};
use html2text::render::TrivialDecorator;

use super::{Block, Document, Metadata, Section, TocEntry};

/// Width handed to html2text so paragraphs come back essentially unwrapped; our
/// own layout pass re-wraps them to the actual pane width.
const EXTRACT_WIDTH: usize = 10_000;

pub struct EpubDocument {
    doc: EpubDoc<BufReader<File>>,
    metadata: Metadata,
    toc: Vec<TocEntry>,
}

impl EpubDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let mut doc = EpubDoc::new(path)
            .with_context(|| format!("opening EPUB {}", path.display()))?;

        let metadata = extract_metadata(&mut doc, size);
        let toc = doc.toc.iter().map(|np| convert_navpoint(np, &doc)).collect();

        Ok(Self { doc, metadata, toc })
    }
}

impl Document for EpubDocument {
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    fn toc(&self) -> &[TocEntry] {
        &self.toc
    }

    fn section_count(&self) -> usize {
        self.doc.get_num_chapters()
    }

    fn load_section(&mut self, index: usize) -> Result<Section> {
        if !self.doc.set_current_chapter(index) {
            anyhow::bail!("section index {index} out of range");
        }
        let (xhtml, _mime) = self
            .doc
            .get_current_str()
            .context("reading current section content")?;
        let text = html2text::from_read_with_decorator(
            xhtml.as_bytes(),
            EXTRACT_WIDTH,
            TrivialDecorator::new(),
        )
        .context("converting XHTML to text")?;
        Ok(Section {
            index,
            blocks: blocks_from_text(&text),
        })
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

fn blocks_from_text(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut last_blank = true;
    for line in text.lines() {
        if line.trim().is_empty() {
            if !last_blank {
                blocks.push(Block::Blank);
                last_blank = true;
            }
        } else {
            blocks.push(Block::Paragraph(line.trim_end().to_string()));
            last_blank = false;
        }
    }
    blocks
}
