//! Mobipocket / KF8 (`.mobi` / `.prc` / `.azw` / `.azw3` / `.kf8`) implementation
//! of [`Document`].
//!
//! A MOBI file is a PalmDB ([`pdb`]) whose record 0 holds a PalmDOC + MOBI header
//! (+ optional EXTH metadata; see [`header`]), followed by compressed text records
//! and then image/resource records. We parse the headers, strip each text record's
//! trailing entries, decompress ([`palmdoc`] / [`huffcdic`]), and hand each
//! section's HTML to the shared [`crate::html::parse_blocks`] pipeline — the same
//! one EPUB uses.
//!
//! Two renditions, chosen in [`parse`]:
//! - **MOBI6** (classic `.mobi`): the text is split on `<mbp:pagebreak>` into
//!   sections; the table of contents comes from the NCX index ([`index`], [`ncx`]),
//!   whose `filepos` targets map back to a section + locator; images are `recindex`.
//! - **KF8 / AZW3** (standalone, or the KF8 half of a hybrid file): real sections
//!   are rebuilt from the skeleton/fragment tables ([`kf8`]), the NCX resolves via
//!   `pos_fid`, and images use the `kindle:embed:` scheme. Preferred when present.
//!
//! Both converge on a `Vec<String>` of section HTML plus image ranges, so the
//! [`Document`] surface and everything downstream are rendition-agnostic. A book
//! with no NCX falls back to a per-section heading outline. HUFF/CDIC (type 17480)
//! is supported; DRM-encrypted books are reported unsupported.

use std::path::Path;
use std::sync::{Arc, LazyLock};

use anyhow::{Context, Result, anyhow};
use regex::Regex;

use super::{Block, Document, Metadata, OutlineItem, Section, SectionLoader, TocEntry};

mod header;
mod huffcdic;
mod index;
mod kf8;
mod ncx;
mod palmdoc;
mod pdb;

use header::{Headers, exth_records};
use huffcdic::HuffCdic;
use pdb::{Pdb, be_u32};

pub(super) const COMPRESSION_NONE: u16 = 1;
const COMPRESSION_PALMDOC: u16 = 2;
pub(super) const COMPRESSION_HUFF: u16 = 17480;
pub(super) const NO_INDEX: u32 = 0xFFFF_FFFF;

/// The parsed content shared between the foreground document and its background
/// loader: the per-section HTML, plus the whole file + image-record ranges so
/// image bytes are sliced lazily (no up-front copy of large image records).
struct MobiContent {
    file: Vec<u8>,
    sections: Vec<String>,
    /// `(start, end)` byte range of each image record, indexed by `recindex - 1`.
    image_ranges: Vec<(usize, usize)>,
}

impl MobiContent {
    /// Parse one section's HTML into blocks, filling each image's bytes from its
    /// record (via the `mobiimg:N` src the loader rewrote `recindex` into).
    fn blocks(&self, index: usize) -> Vec<Block> {
        let Some(html) = self.sections.get(index) else {
            return Vec::new();
        };
        let mut blocks = crate::html::parse_blocks(html);
        for block in &mut blocks {
            if let Block::Image { src, data, .. } = block
                && let Some(n) = src
                    .strip_prefix("mobiimg:")
                    .and_then(|s| s.parse::<usize>().ok())
                && n >= 1
                && let Some(&(start, end)) = self.image_ranges.get(n - 1)
                && let Some(bytes) = self.file.get(start..end)
            {
                *data = bytes.to_vec();
            }
        }
        blocks
    }
}

pub struct MobiDocument {
    content: Arc<MobiContent>,
    metadata: Metadata,
    toc: Vec<TocEntry>,
    outline: Vec<OutlineItem>,
}

impl MobiDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<MobiDocument> {
        let path = path.as_ref();
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let bytes =
            std::fs::read(path).with_context(|| format!("reading MOBI {}", path.display()))?;
        Self::from_bytes(bytes, size)
    }

    /// Parse an in-memory MOBI/KF8 file into a document. Shared by [`Self::open`]
    /// and the tests.
    fn from_bytes(bytes: Vec<u8>, size: u64) -> Result<MobiDocument> {
        // Everything that borrows the raw bytes is computed in `parse`, so the
        // `Pdb` borrow is released before the bytes move into `MobiContent`.
        let parsed = parse(&bytes, size)?;
        Ok(MobiDocument {
            content: Arc::new(MobiContent {
                file: bytes,
                sections: parsed.sections,
                image_ranges: parsed.image_ranges,
            }),
            metadata: parsed.metadata,
            toc: parsed.toc,
            outline: parsed.outline,
        })
    }
}

/// The parsed pieces of a MOBI/KF8 file, before the raw bytes move into the shared
/// [`MobiContent`].
struct Parsed {
    sections: Vec<String>,
    image_ranges: Vec<(usize, usize)>,
    metadata: Metadata,
    toc: Vec<TocEntry>,
    outline: Vec<OutlineItem>,
}

/// The rendition-agnostic output of a build path: per-section HTML, image ranges,
/// and navigation (shared by the MOBI6 and KF8 paths).
pub(super) type Rendition = (
    Vec<String>,
    Vec<(usize, usize)>,
    Vec<TocEntry>,
    Vec<OutlineItem>,
);

/// Parse a MOBI/KF8 file: metadata, per-section HTML, image ranges, and navigation.
fn parse(bytes: &[u8], size: u64) -> Result<Parsed> {
    let pdb = Pdb::parse(bytes).ok_or_else(|| anyhow!("not a PalmDB/MOBI file"))?;
    let rec0 = pdb
        .record(0)
        .ok_or_else(|| anyhow!("MOBI has no record 0"))?;
    let h = Headers::read(rec0)?;
    let metadata = build_metadata(&pdb, rec0, &h, size);

    // Prefer the richer KF8/AZW3 rendition (real sections + NCX); fall back to the
    // MOBI6 / PalmDOC path for plain MOBI files or if KF8 reconstruction fails.
    let (sections, image_ranges, toc, outline) = match kf8::build(&pdb, rec0, &h)? {
        Some(kf8) => kf8,
        None => mobi6_content(&pdb, &h)?,
    };
    Ok(Parsed {
        sections,
        image_ranges,
        metadata,
        toc,
        outline,
    })
}

/// Build the sections + navigation for a MOBI6 / PalmDOC (or uncompressed / HUFF)
/// rendition: decompress the text, split on `<mbp:pagebreak>`, and derive the TOC
/// from the NCX index (falling back to headings).
fn mobi6_content(pdb: &Pdb, h: &Headers) -> Result<Rendition> {
    let huff = build_huff(pdb, h, 0)?;
    // Split the *original* decoded text (before `recindex` rewriting shifts byte
    // offsets), so NCX `filepos` values still map to the right section, then rewrite
    // each section's image refs for the block pipeline.
    let text = extract_text(pdb, h, huff.as_ref(), 1);
    let (raw_sections, ranges) = split_sections(&text);
    let sections: Vec<String> = raw_sections.iter().map(|s| rewrite_recindex(s)).collect();
    let image_ranges = collect_image_ranges(pdb, h.first_image);
    let (toc, outline) = mobi6_nav(pdb, h, &text, &sections, &ranges);
    Ok((sections, image_ranges, toc, outline))
}

/// Navigation for a MOBI6 rendition: prefer the sections' `<hN>` headings (the
/// reliable chapter titles); else parse the NCX index and resolve each entry's
/// `filepos` to a `(section, locator)`; else per-section headings.
fn mobi6_nav(
    pdb: &Pdb,
    h: &Headers,
    text: &str,
    sections: &[String],
    ranges: &[(usize, usize)],
) -> (Vec<TocEntry>, Vec<OutlineItem>) {
    if let Some(nav) = ncx::heading_scan(sections).filter(|(_, o)| o.len() >= 2) {
        return nav;
    }
    if let Some(ncx_idx) = h.ncx_index
        && let Some(idx) = index::read(pdb, ncx_idx)
    {
        let entries = ncx::parse(&idx, h.encoding);
        let resolve = |e: &ncx::NcxEntry| -> Option<ncx::Resolved> {
            let pos = e.pos? as usize;
            let section = section_for_offset(ranges, pos);
            // Prefer the actual heading at the `filepos` target (ground truth) over
            // the CNCX label, which some books store truncated or merged; fall back
            // to the label, then to the visible text at the target.
            let heading = ncx::heading_at(text, pos);
            let label = heading.clone().unwrap_or_else(|| e.label.clone());
            let locator = heading
                .or_else(|| (!e.label.is_empty()).then(|| e.label.clone()))
                .or_else(|| leading_text_at(text, pos));
            Some(ncx::Resolved {
                section,
                label,
                locator,
            })
        };
        if let Some(nav) = ncx::build(&entries, resolve) {
            return nav;
        }
    }
    ncx::heading_fallback(sections)
}

/// Build the HUFF/CDIC decoder if the rendition is HUFF-compressed. `base` offsets
/// the HUFF record index for the KF8 half of a hybrid file (0 otherwise).
fn build_huff(pdb: &Pdb, h: &Headers, base: usize) -> Result<Option<HuffCdic>> {
    if h.compression == COMPRESSION_HUFF {
        let idx = h
            .huff_record
            .context("HUFF/CDIC MOBI is missing its HUFF record index")?;
        Ok(Some(
            HuffCdic::from_records(pdb, base + idx, h.huff_count)
                .context("decoding HUFF/CDIC-compressed MOBI")?,
        ))
    } else {
        Ok(None)
    }
}

/// The section index containing byte offset `pos`: the last section whose range
/// starts at or before `pos` (so offsets in inter-section gaps map to the
/// preceding section), defaulting to the first.
fn section_for_offset(ranges: &[(usize, usize)], pos: usize) -> usize {
    ranges
        .iter()
        .rposition(|&(start, _)| start <= pos)
        .unwrap_or(0)
}

/// The first line or so of visible text at byte offset `pos` in `text`, tags
/// stripped — a locator the reader can match against a wrapped display line.
fn leading_text_at(text: &str, pos: usize) -> Option<String> {
    let mut start = pos.min(text.len());
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    let mut out = String::new();
    let mut in_tag = false;
    for c in text[start..].chars().take(600) {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let words: Vec<&str> = out.split_whitespace().take(12).collect();
    if words.is_empty() {
        None
    } else {
        Some(ncx::trim_to(&words.join(" "), 80))
    }
}

/// Background loader: shares the already-parsed content (MOBI is fully in memory),
/// so it re-parses blocks off-thread without touching the file again.
struct MobiLoader {
    content: Arc<MobiContent>,
}

impl SectionLoader for MobiLoader {
    fn load(&mut self, index: usize) -> Vec<Block> {
        self.content.blocks(index)
    }
}

impl Document for MobiDocument {
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
        Box::new(MobiLoader {
            content: Arc::clone(&self.content),
        })
    }

    fn section_count(&self) -> usize {
        self.content.sections.len()
    }

    fn load_section(&mut self, index: usize) -> Result<Section> {
        Ok(Section {
            index,
            blocks: self.content.blocks(index),
        })
    }

    fn section_targets(&mut self, index: usize) -> Vec<(String, String)> {
        self.content
            .sections
            .get(index)
            .map(|s| crate::html::collect_targets(s))
            .unwrap_or_default()
    }
}

/// Read just the metadata (no text decompression), for the library scan.
pub fn read_metadata(path: impl AsRef<Path>) -> Result<(Metadata, usize)> {
    let path = path.as_ref();
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let bytes = std::fs::read(path).with_context(|| format!("reading MOBI {}", path.display()))?;
    let pdb = Pdb::parse(&bytes).ok_or_else(|| anyhow!("not a PalmDB/MOBI file"))?;
    let rec0 = pdb
        .record(0)
        .ok_or_else(|| anyhow!("MOBI has no record 0"))?;
    let h = Headers::read(rec0)?;
    Ok((build_metadata(&pdb, rec0, &h, size), 1))
}

/// Cap the initial text-buffer reservation. `text_length` comes verbatim from the
/// record-0 header (an untrusted u32, up to ~4 GB), so reserving it directly lets a
/// tiny crafted MOBI force a multi-gigabyte allocation and abort the process. The
/// buffer still grows as needed for a legitimately large book — this only bounds
/// the up-front hint; the loop is bounded by the real record bytes.
const MAX_TEXT_PREALLOC: usize = 32 * 1024 * 1024;

/// Decompress the text records into the raw markup bytes. `first_record` is the
/// PDB index of the first text record — 1 for a standalone MOBI6/KF8 file, or
/// `boundary + 1` for the KF8 half of a hybrid file. KF8 reassembly needs the
/// undecoded bytes (skeleton/fragment offsets are byte offsets), so the decode is
/// split out into [`extract_text`].
fn extract_bytes(pdb: &Pdb, h: &Headers, huff: Option<&HuffCdic>, first_record: usize) -> Vec<u8> {
    let mut text: Vec<u8> = Vec::with_capacity(h.text_length.min(MAX_TEXT_PREALLOC));
    for i in first_record..first_record + h.record_count {
        let Some(rec) = pdb.record(i) else { break };
        // Strip the trailing multibyte/entry bytes before decompressing.
        let keep = rec.len() - trailing_size(rec, h.extra_flags);
        let body = &rec[..keep];
        match h.compression {
            COMPRESSION_PALMDOC => text.extend_from_slice(&palmdoc::decompress(body)),
            COMPRESSION_HUFF => {
                if let Some(d) = huff {
                    text.extend_from_slice(&d.decompress(body));
                }
            }
            _ => text.extend_from_slice(body), // uncompressed (type 1) or best-effort
        }
        if text.len() >= h.text_length {
            break;
        }
    }
    text.truncate(h.text_length);
    text
}

/// Decompress + decode the text records into one HTML string.
fn extract_text(pdb: &Pdb, h: &Headers, huff: Option<&HuffCdic>, first_record: usize) -> String {
    decode_bytes(&extract_bytes(pdb, h, huff, first_record), h.encoding)
}

/// The number of trailing bytes to strip from a text record, per the MOBI
/// "extra record data flags" (multibyte overlap + variable-size trailing entries).
fn trailing_size(rec: &[u8], flags: u16) -> usize {
    let mut num = 0usize;
    // Bits 1.. each mark a backward-length-encoded trailing entry.
    let mut f = flags >> 1;
    while f != 0 {
        if f & 1 != 0 {
            let end = rec.len().saturating_sub(num);
            num += size_of_trailing_entry(&rec[..end]);
        }
        f >>= 1;
    }
    // Bit 0: a multibyte character overlap of 1..=4 bytes.
    if flags & 1 != 0 {
        let end = rec.len().saturating_sub(num);
        if end > 0 {
            num += (rec[end - 1] & 0x03) as usize + 1;
        }
    }
    num.min(rec.len())
}

/// Decode a backward variable-length trailing-entry size (KindleUnpack's
/// `getSizeOfTrailingDataEntry`): the value is stored in the last up-to-4 bytes,
/// seven bits each, and includes the size bytes themselves.
fn size_of_trailing_entry(data: &[u8]) -> usize {
    let mut num = 0usize;
    let start = data.len().saturating_sub(4);
    for &v in &data[start..] {
        if v & 0x80 != 0 {
            num = 0;
        }
        num = (num << 7) | (v & 0x7F) as usize;
    }
    num.min(data.len())
}

/// Build [`Metadata`] from the MOBI full-name and EXTH records.
fn build_metadata(pdb: &Pdb, rec0: &[u8], h: &Headers, size: u64) -> Metadata {
    let mut meta = Metadata {
        size,
        ..Default::default()
    };
    // Full book name (a slice within record 0).
    let (off, len) = h.full_name;
    if len > 0
        && let Some(name) = rec0.get(off..off + len)
    {
        meta.title = decode_bytes(name, h.encoding);
    }

    let mut cover_offset: Option<usize> = None;
    for (kind, data) in exth_records(rec0, h.mobi_header_len) {
        match kind {
            100 => meta.authors.push(decode_bytes(data, h.encoding)),
            101 => meta.publisher = Some(decode_bytes(data, h.encoding)),
            104 => meta.identifier = Some(decode_bytes(data, h.encoding)),
            106 => meta.year = parse_year(&decode_bytes(data, h.encoding)),
            // Contributor: names the producer. Calibre (and other converters)
            // stamp themselves here, so it marks a repackaged/converted file.
            108 => {
                if crate::provenance::names_converter_tool(&decode_bytes(data, h.encoding)) {
                    meta.converted = true;
                }
            }
            // Source (EXTH 112): the format the file was built from. A foreign
            // document/e-book format (e.g. `docx`, `epub`) means it was converted —
            // this catches Kindle Create / kindlegen conversions that leave no
            // converter name in the contributor record.
            112 => {
                if crate::provenance::names_converted_source(&decode_bytes(data, h.encoding)) {
                    meta.converted = true;
                }
            }
            503 => meta.title = decode_bytes(data, h.encoding), // "updated title" wins
            201 => {
                cover_offset = be_u32(data, 0)
                    .filter(|&n| n != NO_INDEX)
                    .map(|n| n as usize)
            }
            _ => {}
        }
    }

    // The cover-offset EXTH is relative to the first image record.
    if let (Some(first), Some(co)) = (h.first_image, cover_offset)
        && let Some(bytes) = pdb.record(first + co)
        && let Some(mime) = image_mime(bytes)
    {
        meta.cover = Some((bytes.to_vec(), mime.to_string()));
    }
    meta
}

/// Collect the `(start, end)` ranges of the image records (from the first image
/// record to the end), so `recindex` can slice image bytes on demand.
pub(super) fn collect_image_ranges(pdb: &Pdb, first_image: Option<usize>) -> Vec<(usize, usize)> {
    let Some(first) = first_image else {
        return Vec::new();
    };
    (first..pdb.len())
        .filter_map(|i| pdb.record_range(i))
        .collect()
}

/// Rewrite MOBI `<img recindex="N">` to the `src="mobiimg:N"` the block pipeline
/// (and [`MobiContent::blocks`]) understands.
pub(super) fn rewrite_recindex(html: &str) -> String {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?i)recindex=['"]?0*(\d+)['"]?"#).unwrap());
    RE.replace_all(html, r#"src="mobiimg:$1""#).into_owned()
}

/// Split the concatenated MOBI HTML into sections on `<mbp:pagebreak>` markers
/// (the whole document as one section when there are none), returning each
/// section's HTML together with its `(start, end)` byte range in `html` so NCX
/// `filepos` offsets can be mapped back to a section.
fn split_sections(html: &str) -> (Vec<String>, Vec<(usize, usize)>) {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<mbp:pagebreak[^>]*>").unwrap());
    let mut sections = Vec::new();
    let mut ranges = Vec::new();
    let mut last = 0;
    for m in RE.find_iter(html) {
        push_section(html, last, m.start(), &mut sections, &mut ranges);
        last = m.end();
    }
    push_section(html, last, html.len(), &mut sections, &mut ranges);
    if sections.is_empty() {
        sections.push(html.to_string());
        ranges.push((0, html.len()));
    }
    (sections, ranges)
}

/// Push `html[start..end]` as a section (with its byte range) unless it is blank.
fn push_section(
    html: &str,
    start: usize,
    end: usize,
    sections: &mut Vec<String>,
    ranges: &mut Vec<(usize, usize)>,
) {
    let part = &html[start..end];
    if !part.trim().is_empty() {
        sections.push(part.to_string());
        ranges.push((start, end));
    }
}

/// Decode MOBI bytes to a `String` per the text-encoding field (65001 = UTF-8,
/// else Windows-1252).
pub(super) fn decode_bytes(bytes: &[u8], encoding: u32) -> String {
    if encoding == 65001 {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        bytes.iter().map(|&b| cp1252_char(b)).collect()
    }
}

/// Map a Windows-1252 byte to its Unicode character (Latin-1 outside 0x80–0x9F,
/// with the CP1252 punctuation block in between; undefined slots pass through).
fn cp1252_char(b: u8) -> char {
    match b {
        0x80 => '€',
        0x82 => '‚',
        0x83 => 'ƒ',
        0x84 => '„',
        0x85 => '…',
        0x86 => '†',
        0x87 => '‡',
        0x88 => 'ˆ',
        0x89 => '‰',
        0x8A => 'Š',
        0x8B => '‹',
        0x8C => 'Œ',
        0x8E => 'Ž',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '•',
        0x96 => '–',
        0x97 => '—',
        0x98 => '˜',
        0x99 => '™',
        0x9A => 'š',
        0x9B => '›',
        0x9C => 'œ',
        0x9E => 'ž',
        0x9F => 'Ÿ',
        other => other as char,
    }
}

/// The image MIME for a record's magic bytes, or `None` if it isn't an image.
fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        _ => None,
    }
}

/// The first 4-digit run in `s`, as a year.
fn parse_year(s: &str) -> Option<i32> {
    let bytes = s.as_bytes();
    bytes.windows(4).find_map(|w| {
        w.iter()
            .all(u8::is_ascii_digit)
            .then(|| std::str::from_utf8(w).ok().and_then(|d| d.parse().ok()))
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdb::build_pdb;

    /// Assemble record 0: PalmDOC header + MOBI header + EXTH, then a title tail.
    fn record0(
        text_len: usize,
        records: u16,
        encoding: u32,
        title: &str,
        author: &str,
        contributor: &str,
    ) -> Vec<u8> {
        let mobi_header_len = 232usize; // ≥ 0xE4 so extra-flags offset is valid
        let mut r0 = vec![0u8; 16 + mobi_header_len];
        // PalmDOC header.
        r0[0..2].copy_from_slice(&COMPRESSION_PALMDOC.to_be_bytes());
        r0[4..8].copy_from_slice(&(text_len as u32).to_be_bytes());
        r0[8..10].copy_from_slice(&records.to_be_bytes());
        // MOBI header.
        r0[16..20].copy_from_slice(b"MOBI");
        r0[20..24].copy_from_slice(&(mobi_header_len as u32).to_be_bytes());
        r0[28..32].copy_from_slice(&encoding.to_be_bytes());
        r0[108..112].copy_from_slice(&NO_INDEX.to_be_bytes()); // no images
        r0[128..132].copy_from_slice(&0x40u32.to_be_bytes()); // EXTH present
        // Full name appended after the header; point the offset/len at it.
        let name_off = r0.len();
        r0[84..88].copy_from_slice(&(name_off as u32).to_be_bytes());
        r0[88..92].copy_from_slice(&(title.len() as u32).to_be_bytes());
        // EXTH header sits right after the MOBI header (before the appended name).
        // EXTH records: author (100) plus an optional contributor (108).
        let mut recs: Vec<(u32, &str)> = vec![(100, author)];
        if !contributor.is_empty() {
            recs.push((108, contributor));
        }
        let body_len: usize = recs.iter().map(|(_, v)| 8 + v.len()).sum();
        let mut exth = Vec::new();
        exth.extend_from_slice(b"EXTH");
        exth.extend_from_slice(&((12 + body_len) as u32).to_be_bytes());
        exth.extend_from_slice(&(recs.len() as u32).to_be_bytes());
        for (kind, val) in &recs {
            exth.extend_from_slice(&kind.to_be_bytes());
            exth.extend_from_slice(&((8 + val.len()) as u32).to_be_bytes());
            exth.extend_from_slice(val.as_bytes());
        }
        // Splice the EXTH in at offset 16 + mobi_header_len (== r0.len() now).
        r0.extend_from_slice(&exth);
        // Fix the full-name offset to sit after the EXTH we just appended.
        let name_off = r0.len();
        r0[84..88].copy_from_slice(&(name_off as u32).to_be_bytes());
        r0.extend_from_slice(title.as_bytes());
        r0
    }

    #[test]
    fn parses_headers_metadata_and_sections() {
        // Two text sections separated by a pagebreak, uncompressed via a literal run.
        let html = "<h1>Chapter One</h1><p>Hello</p><mbp:pagebreak/><p>Second</p>";
        let compressed = compress_literal(html.as_bytes());
        let r0 = record0(
            html.len(),
            1,
            65001,
            "Test Book",
            "Ada Lovelace",
            "calibre (6.21.0) [https://calibre-ebook.com]",
        );
        let data = build_pdb(&[&r0, &compressed]);

        let doc = MobiDocument::open_bytes(data).unwrap();
        assert_eq!(doc.metadata().title, "Test Book");
        assert_eq!(doc.metadata().authors, vec!["Ada Lovelace".to_string()]);
        // The calibre contributor EXTH marks the file as a conversion.
        assert!(doc.metadata().converted, "calibre contributor ⇒ converted");
        assert_eq!(doc.section_count(), 2);
        // First section's first heading drives the outline label.
        assert_eq!(doc.outline()[0].label, "Chapter One");
        assert_eq!(doc.toc()[1].section, Some(1));
    }

    #[test]
    fn huge_text_length_does_not_over_allocate() {
        // A crafted MOBI advertising a ~4 GB text_length must not reserve that
        // buffer up front — that alloc would abort the process. The real text
        // still comes from the actual (tiny) record; the prealloc hint is capped
        // at MAX_TEXT_PREALLOC. Without the cap this test OOM-aborts.
        let html = "<p>tiny</p>";
        let compressed = compress_literal(html.as_bytes());
        let r0 = record0(u32::MAX as usize, 1, 65001, "Evil", "X", "");
        let data = build_pdb(&[&r0, &compressed]);
        let doc = MobiDocument::open_bytes(data).expect("opens without over-allocating");
        assert!(doc.section_count() >= 1, "still decodes the real text");
    }

    #[test]
    fn cp1252_and_utf8_decode() {
        assert_eq!(
            decode_bytes(&[0x93, b'h', b'i', 0x94], 1252),
            "\u{201C}hi\u{201D}"
        );
        assert_eq!(decode_bytes("café".as_bytes(), 65001), "café");
    }

    #[test]
    fn rejects_non_mobi() {
        let data = build_pdb(&[b"not a mobi header at all really"]);
        assert!(MobiDocument::open_bytes(data).is_err());
    }

    // ── INDX index synthesis (mirrors the KindleUnpack on-disk layout) ─────────

    /// Encode a forward variable-width integer (7 bits/byte, big-endian, high bit
    /// set on the final byte) — the inverse of `index::get_var_width`.
    fn vwi(v: u32) -> Vec<u8> {
        let mut groups = Vec::new();
        let mut x = v;
        loop {
            groups.push((x & 0x7f) as u8);
            x >>= 7;
            if x == 0 {
                break;
            }
        }
        groups.reverse();
        let last = groups.len() - 1;
        groups[last] |= 0x80;
        groups
    }

    /// Build a CNCX record from labels, returning the record bytes and each label's
    /// offset key.
    fn cncx_record(labels: &[&str]) -> (Vec<u8>, Vec<u32>) {
        let mut data = Vec::new();
        let mut offsets = Vec::new();
        for l in labels {
            offsets.push(data.len() as u32);
            data.extend(vwi(l.len() as u32));
            data.extend_from_slice(l.as_bytes());
        }
        data.push(0); // readCTOC terminates on a zero byte
        (data, offsets)
    }

    /// A TAGX table: `(tag, values_per_entry, mask)` tuples plus an end sentinel,
    /// with a single control byte.
    fn tagx(tags: &[(u8, u8, u8)]) -> Vec<u8> {
        let mut t = b"TAGX".to_vec();
        let first_entry_offset = 12 + (tags.len() + 1) * 4;
        t.extend_from_slice(&(first_entry_offset as u32).to_be_bytes());
        t.extend_from_slice(&1u32.to_be_bytes()); // controlByteCount
        for &(tag, vpe, mask) in tags {
            t.extend_from_slice(&[tag, vpe, mask, 0]);
        }
        t.extend_from_slice(&[0, 0, 0, 1]); // end sentinel
        t
    }

    /// One INDX entry: length-prefixed name, a control byte, then each present
    /// tag's values (in tag-table order) as forward varints.
    fn indx_entry(name: &[u8], control: u8, values: &[&[u32]]) -> Vec<u8> {
        let mut e = vec![name.len() as u8];
        e.extend_from_slice(name);
        e.push(control);
        for vs in values {
            for &v in *vs {
                e.extend(vwi(v));
            }
        }
        e
    }

    /// A 192-byte INDX header carrying `tagx`, with the data-record and CNCX-record
    /// counts set.
    fn indx_header(tagx: &[u8], data_records: u32, cncx_records: u32) -> Vec<u8> {
        let mut h = vec![0u8; 192];
        h[0..4].copy_from_slice(b"INDX");
        h[4..8].copy_from_slice(&192u32.to_be_bytes()); // len → TAGX offset
        h[24..28].copy_from_slice(&data_records.to_be_bytes()); // count
        h[52..56].copy_from_slice(&cncx_records.to_be_bytes()); // nctoc
        h.extend_from_slice(tagx);
        h
    }

    /// An INDX data record: a 192-byte header, the entries, then the IDXT table.
    fn indx_data(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut rec = vec![0u8; 192];
        rec[0..4].copy_from_slice(b"INDX");
        let mut positions = Vec::new();
        for e in entries {
            positions.push(rec.len() as u16);
            rec.extend_from_slice(e);
        }
        let idxt_pos = rec.len();
        rec[20..24].copy_from_slice(&(idxt_pos as u32).to_be_bytes()); // start
        rec[24..28].copy_from_slice(&(entries.len() as u32).to_be_bytes()); // count
        rec.extend_from_slice(b"IDXT");
        for p in positions {
            rec.extend_from_slice(&p.to_be_bytes());
        }
        rec
    }

    #[test]
    fn builds_hierarchical_toc_from_headings() {
        // Headings are the primary TOC source (reliable even when the NCX is bad):
        // two chapters, with "Sub One" (an <h2>) nested under "Chapter One" (<h1>).
        let html = "<h1>Chapter One</h1><p>Alpha.</p><h2>Sub One</h2><p>beta.</p>\
                    <mbp:pagebreak/><h1>Chapter&nbsp;Two</h1><p>Gamma.</p>";
        let r0 = record0(html.len(), 1, 65001, "Test Book", "Author", "");
        let data = build_pdb(&[&r0, &compress_literal(html.as_bytes())]);

        let doc = MobiDocument::open_bytes(data).unwrap();
        assert_eq!(doc.section_count(), 2);

        let toc = doc.toc();
        assert_eq!(toc.len(), 2);
        assert_eq!(toc[0].label, "Chapter One");
        assert_eq!(toc[0].children.len(), 1);
        assert_eq!(toc[0].children[0].label, "Sub One");
        // Entities are decoded in heading labels.
        assert_eq!(toc[1].label, "Chapter Two");

        let outline = doc.outline();
        assert_eq!(outline.len(), 3);
        assert_eq!((outline[0].depth, outline[0].section), (0, 0));
        assert_eq!((outline[1].depth, outline[1].section), (1, 0));
        assert_eq!((outline[2].depth, outline[2].section), (0, 1));
        assert_eq!(outline[1].label, "Sub One");
        assert_eq!(outline[2].locator.as_deref(), Some("Chapter Two"));
    }

    #[test]
    fn parses_ncx_filepos_when_no_headings() {
        // No <hN> headings, so the NCX index drives the TOC: filepos → section,
        // hlvl → depth, CNCX offset → label.
        let html = "<p>Chapter one body.</p><p>a sub part.</p>\
                    <mbp:pagebreak/><p>Chapter two body.</p>";
        let sub = html.find("<p>a sub").unwrap() as u32;
        let ch2 = html.find("<p>Chapter two").unwrap() as u32;

        let (cncx, off) = cncx_record(&["Chapter One", "Sub One", "Chapter Two"]);
        let tx = tagx(&[(1, 1, 0x01), (4, 1, 0x02), (3, 1, 0x04)]); // pos, hlvl, noffs
        let ncx_hdr = indx_header(&tx, 1, 1);
        let ncx_dat = indx_data(&[
            indx_entry(b"", 0x07, &[&[0], &[0], &[off[0]]]),
            indx_entry(b"", 0x07, &[&[sub], &[1], &[off[1]]]),
            indx_entry(b"", 0x07, &[&[ch2], &[0], &[off[2]]]),
        ]);

        let mut r0 = record0(html.len(), 1, 65001, "Test Book", "Author", "");
        r0[244..248].copy_from_slice(&2u32.to_be_bytes()); // NCX header @ record 2
        let text = compress_literal(html.as_bytes());
        let data = build_pdb(&[&r0, &text, &ncx_hdr, &ncx_dat, &cncx]);

        let doc = MobiDocument::open_bytes(data).unwrap();
        assert_eq!(doc.section_count(), 2);
        let toc = doc.toc();
        assert_eq!(toc.len(), 2);
        assert_eq!(toc[0].label, "Chapter One");
        assert_eq!(toc[0].children[0].label, "Sub One");
        assert_eq!(toc[1].label, "Chapter Two");
        let outline = doc.outline();
        assert_eq!((outline[1].depth, outline[1].section), (1, 0));
        assert_eq!((outline[2].depth, outline[2].section), (0, 1));
    }

    #[test]
    fn ncx_child_pointers_drive_the_tree() {
        // The NCX lists both chapters flat (entries 0,1) then both of chapter A's
        // sub-entries (2,3) — the tree must come from the child pointers, not from
        // document order (which would nest the subs under chapter B).
        let html = "<p>Chapter A intro.</p><p>sub a one.</p><p>sub a two.</p>\
                    <mbp:pagebreak/><p>Chapter B here.</p>";
        let sub1 = html.find("<p>sub a one").unwrap() as u32;
        let sub2 = html.find("<p>sub a two").unwrap() as u32;
        let chb = html.find("<p>Chapter B").unwrap() as u32;

        let (cncx, off) = cncx_record(&["Chapter A", "Chapter B", "Sub A1", "Sub A2"]);
        // pos(1), hlvl(4), noffs(3), child1(22), childn(23).
        let tx = tagx(&[
            (1, 1, 0x01),
            (4, 1, 0x02),
            (3, 1, 0x04),
            (22, 1, 0x08),
            (23, 1, 0x10),
        ]);
        let ncx_hdr = indx_header(&tx, 1, 1);
        let ncx_dat = indx_data(&[
            indx_entry(b"", 0x1F, &[&[0], &[0], &[off[0]], &[2], &[3]]), // A → children 2..3
            indx_entry(b"", 0x07, &[&[chb], &[0], &[off[1]]]),
            indx_entry(b"", 0x07, &[&[sub1], &[1], &[off[2]]]),
            indx_entry(b"", 0x07, &[&[sub2], &[1], &[off[3]]]),
        ]);

        let mut r0 = record0(html.len(), 1, 65001, "Test", "Author", "");
        r0[244..248].copy_from_slice(&2u32.to_be_bytes());
        let data = build_pdb(&[
            &r0,
            &compress_literal(html.as_bytes()),
            &ncx_hdr,
            &ncx_dat,
            &cncx,
        ]);

        let doc = MobiDocument::open_bytes(data).unwrap();
        let toc = doc.toc();
        assert_eq!(toc.len(), 2);
        assert_eq!(toc[0].label, "Chapter A");
        assert_eq!(toc[0].children.len(), 2);
        assert_eq!(toc[0].children[0].label, "Sub A1");
        assert_eq!(toc[0].children[1].label, "Sub A2");
        assert_eq!(toc[1].label, "Chapter B");
        // Outline is preorder: A, its subs, then B.
        let labels: Vec<&str> = doc.outline().iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Chapter A", "Sub A1", "Sub A2", "Chapter B"]);
        assert_eq!(doc.outline()[3].section, 1); // Chapter B is in section 1
    }

    #[test]
    fn no_ncx_falls_back_to_headings() {
        // A book with no NCX index (record-0 @0xF4 left zero) and a single heading
        // still gets a per-section heading outline.
        let html = "<h1>Only Chapter</h1><p>Body.</p>";
        let r0 = record0(html.len(), 1, 65001, "Test", "Author", "");
        let data = build_pdb(&[&r0, &compress_literal(html.as_bytes())]);
        let doc = MobiDocument::open_bytes(data).unwrap();
        assert_eq!(doc.outline().len(), 1);
        assert_eq!(doc.outline()[0].label, "Only Chapter");
        assert_eq!(doc.outline()[0].locator, None);
    }

    /// A standalone KF8 record-0 header: PalmDOC, UTF-8, version 8, one text record,
    /// with the skeleton/fragment/NCX index fields left for the caller to set.
    fn kf8_record0(text_len: usize) -> Vec<u8> {
        let mobi_header_len = 248usize; // reaches the skeleton field at 0xFC
        let mut r0 = vec![0u8; 16 + mobi_header_len];
        r0[0..2].copy_from_slice(&COMPRESSION_PALMDOC.to_be_bytes());
        r0[4..8].copy_from_slice(&(text_len as u32).to_be_bytes());
        r0[8..10].copy_from_slice(&1u16.to_be_bytes()); // one text record
        r0[16..20].copy_from_slice(b"MOBI");
        r0[20..24].copy_from_slice(&(mobi_header_len as u32).to_be_bytes());
        r0[28..32].copy_from_slice(&65001u32.to_be_bytes()); // UTF-8
        r0[36..40].copy_from_slice(&8u32.to_be_bytes()); // version 8 → KF8
        r0[108..112].copy_from_slice(&NO_INDEX.to_be_bytes()); // no images
        r0
    }

    #[test]
    fn kf8_rebuilds_sections_from_skeleton_and_fragments() {
        // flow 0 = [shell0][frag0][shell1][frag1]; each fragment splices into its
        // shell right after `<body>` (byte 12). No headings, so the NCX `pos_fid`
        // path (not the heading scan) resolves the labels.
        let shell = b"<html><body></body></html>";
        let insert = 12u32;
        let frag0 = b"<p>Chapter one body.</p>";
        let frag1 = b"<p>Chapter two body.</p>";
        let mut flow0 = Vec::new();
        let s0 = flow0.len() as u32;
        flow0.extend_from_slice(shell);
        flow0.extend_from_slice(frag0);
        let s1 = flow0.len() as u32;
        flow0.extend_from_slice(shell);
        flow0.extend_from_slice(frag1);

        // Skeleton index: frag count (tag 1) + [pos, len] (tag 6).
        let skel_tx = tagx(&[(1, 1, 0x01), (6, 2, 0x02)]);
        let skel_hdr = indx_header(&skel_tx, 1, 0);
        let skel_dat = indx_data(&[
            indx_entry(b"S0", 0x03, &[&[1], &[s0, shell.len() as u32]]),
            indx_entry(b"S1", 0x03, &[&[1], &[s1, shell.len() as u32]]),
        ]);

        // Fragment index: name = insert position, [start, len] (tag 6).
        let frag_tx = tagx(&[(6, 2, 0x01)]);
        let frag_hdr = indx_header(&frag_tx, 1, 0);
        let ins0 = (s0 + insert).to_string();
        let ins1 = (s1 + insert).to_string();
        let frag_dat = indx_data(&[
            indx_entry(ins0.as_bytes(), 0x01, &[&[0, frag0.len() as u32]]),
            indx_entry(ins1.as_bytes(), 0x01, &[&[0, frag1.len() as u32]]),
        ]);

        // NCX: pos_fid (tag 6 = [fragment row, offset]), hlvl (4), noffs (3).
        let (cncx, off) = cncx_record(&["Chapter One", "Chapter Two"]);
        let ncx_tx = tagx(&[(6, 2, 0x01), (4, 1, 0x02), (3, 1, 0x04)]);
        let ncx_hdr = indx_header(&ncx_tx, 1, 1);
        let ncx_dat = indx_data(&[
            indx_entry(b"", 0x07, &[&[0, 0], &[0], &[off[0]]]), // fragment 0 → section 0
            indx_entry(b"", 0x07, &[&[1, 0], &[0], &[off[1]]]), // fragment 1 → section 1
        ]);

        // Records: 0 header, 1 text, 2/3 skeleton, 4/5 fragment, 6/7 NCX, 8 CNCX.
        let mut r0 = kf8_record0(flow0.len());
        r0[0xF4..0xF8].copy_from_slice(&6u32.to_be_bytes()); // ncx index
        r0[0xF8..0xFC].copy_from_slice(&4u32.to_be_bytes()); // fragment index
        r0[0xFC..0x100].copy_from_slice(&2u32.to_be_bytes()); // skeleton index
        let text = compress_literal(&flow0);
        let data = build_pdb(&[
            &r0, &text, &skel_hdr, &skel_dat, &frag_hdr, &frag_dat, &ncx_hdr, &ncx_dat, &cncx,
        ]);

        let doc = MobiDocument::open_bytes(data).unwrap();
        // KF8 reconstructed two real sections (not one collapsed blob).
        assert_eq!(doc.section_count(), 2);
        // NCX via pos_fid: each entry lands in its fragment's section.
        let outline = doc.outline();
        assert_eq!(outline.len(), 2);
        assert_eq!(outline[0].label, "Chapter One");
        assert_eq!(outline[0].section, 0);
        assert_eq!(outline[1].label, "Chapter Two");
        assert_eq!(outline[1].section, 1);
    }

    /// PalmDOC-"compress" bytes as literal runs (no back-references) — a valid
    /// type-2 stream the decompressor round-trips, for header/section tests.
    fn compress_literal(input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in input.chunks(8) {
            out.push(chunk.len() as u8); // 0x01..=0x08: literal run
            out.extend_from_slice(chunk);
        }
        out
    }

    impl MobiDocument {
        /// Test helper: parse from in-memory bytes (mirrors `open` without a file).
        fn open_bytes(bytes: Vec<u8>) -> Result<MobiDocument> {
            Self::from_bytes(bytes, 0)
        }
    }
}
