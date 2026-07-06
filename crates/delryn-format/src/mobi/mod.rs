//! Mobipocket / KF8 (`.mobi` / `.prc` / `.azw` / `.azw3` / `.kf8`) implementation
//! of [`Document`].
//!
//! A MOBI file is a PalmDB ([`pdb`]) whose record 0 holds a PalmDOC + MOBI header
//! (+ optional EXTH metadata), followed by PalmDOC-compressed HTML text records
//! and then image records. We parse the headers, strip each text record's trailing
//! entries, decompress ([`palmdoc`]), decode the text (UTF-8 or CP1252), split it
//! on `<mbp:pagebreak>` into sections, and hand each section's HTML to the shared
//! [`crate::html::parse_blocks`] pipeline — the same one EPUB uses. Embedded images
//! (`recindex="N"`) are resolved from the image records.
//!
//! Scope: PalmDOC (type 2) and uncompressed (type 1) text; HUFF/CDIC (type 17480)
//! and DRM-encrypted books are reported unsupported. AZW3/KF8 shares this backend —
//! its record 0 is parsed the same way (the legacy text for a hybrid file, the KF8
//! HTML for an AZW3-only file); the richer KF8 skeleton/fragment structure is not
//! yet reconstructed.

use std::path::Path;
use std::sync::{Arc, LazyLock};

use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;

use super::{Block, Document, Metadata, OutlineItem, Section, SectionLoader, TocEntry};

mod palmdoc;
mod pdb;

use pdb::{Pdb, be_u16, be_u32};

const COMPRESSION_NONE: u16 = 1;
const COMPRESSION_PALMDOC: u16 = 2;
const COMPRESSION_HUFF: u16 = 17480;
const NO_INDEX: u32 = 0xFFFF_FFFF;

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

        // Everything that borrows the raw bytes is computed in this block, so the
        // `Pdb` borrow is released before the bytes move into `MobiContent`.
        let (sections, image_ranges, metadata, toc, outline) = {
            let pdb = Pdb::parse(&bytes).ok_or_else(|| anyhow!("not a PalmDB/MOBI file"))?;
            let rec0 = pdb
                .record(0)
                .ok_or_else(|| anyhow!("MOBI has no record 0"))?;
            let h = Headers::read(rec0)?;
            if h.compression == COMPRESSION_HUFF {
                bail!("HUFF/CDIC-compressed MOBI is not supported yet");
            }
            let metadata = build_metadata(&pdb, rec0, &h, size);
            let html = rewrite_recindex(&extract_text(&pdb, &h));
            let sections = split_sections(&html);
            let image_ranges = collect_image_ranges(&pdb, h.first_image);
            let (toc, outline) = build_navigation(&sections);
            (sections, image_ranges, metadata, toc, outline)
        };

        Ok(MobiDocument {
            content: Arc::new(MobiContent {
                file: bytes,
                sections,
                image_ranges,
            }),
            metadata,
            toc,
            outline,
        })
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

/// The PalmDOC + MOBI header fields we use, read from record 0.
struct Headers {
    compression: u16,
    text_length: usize,
    record_count: usize,
    encoding: u32,
    mobi_header_len: usize,
    full_name: (usize, usize),
    first_image: Option<usize>,
    extra_flags: u16,
}

impl Headers {
    fn read(rec0: &[u8]) -> Result<Headers> {
        if rec0.len() < 132 || &rec0[16..20] != b"MOBI" {
            bail!("record 0 is not a MOBI header");
        }
        let compression = be_u16(rec0, 0).unwrap_or(COMPRESSION_NONE);
        let text_length = be_u32(rec0, 4).unwrap_or(0) as usize;
        let record_count = be_u16(rec0, 8).unwrap_or(0) as usize;
        // Encryption type is a u16 at offset 12 (0 = none).
        if be_u16(rec0, 12).unwrap_or(0) != 0 {
            bail!("this MOBI is DRM-encrypted");
        }
        let mobi_header_len = be_u32(rec0, 20).unwrap_or(0) as usize;
        let encoding = be_u32(rec0, 28).unwrap_or(1252);
        let full_name = (
            be_u32(rec0, 84).unwrap_or(0) as usize,
            be_u32(rec0, 88).unwrap_or(0) as usize,
        );
        let first_image = match be_u32(rec0, 108).unwrap_or(NO_INDEX) {
            NO_INDEX | 0 => None,
            n => Some(n as usize),
        };
        // The "extra record data flags" (trailing-entry bitfield) is a u16 at
        // record-0 offset 242, present only when the MOBI header reaches that far.
        let extra_flags = if mobi_header_len >= 0xE4 {
            be_u16(rec0, 242).unwrap_or(0)
        } else {
            0
        };
        Ok(Headers {
            compression,
            text_length,
            record_count,
            encoding,
            mobi_header_len,
            full_name,
            first_image,
            extra_flags,
        })
    }
}

/// Cap the initial text-buffer reservation. `text_length` comes verbatim from the
/// record-0 header (an untrusted u32, up to ~4 GB), so reserving it directly lets a
/// tiny crafted MOBI force a multi-gigabyte allocation and abort the process. The
/// buffer still grows as needed for a legitimately large book — this only bounds
/// the up-front hint; the loop is bounded by the real record bytes.
const MAX_TEXT_PREALLOC: usize = 32 * 1024 * 1024;

/// Decompress + decode the text records into one HTML string.
fn extract_text(pdb: &Pdb, h: &Headers) -> String {
    let mut text: Vec<u8> = Vec::with_capacity(h.text_length.min(MAX_TEXT_PREALLOC));
    for i in 1..=h.record_count {
        let Some(rec) = pdb.record(i) else { break };
        // Strip the trailing multibyte/entry bytes before decompressing.
        let keep = rec.len() - trailing_size(rec, h.extra_flags);
        let body = &rec[..keep];
        match h.compression {
            COMPRESSION_PALMDOC => text.extend_from_slice(&palmdoc::decompress(body)),
            _ => text.extend_from_slice(body), // uncompressed (type 1) or best-effort
        }
        if text.len() >= h.text_length {
            break;
        }
    }
    text.truncate(h.text_length);
    decode_bytes(&text, h.encoding)
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

/// Iterate the EXTH records (type, data) if an EXTH header follows the MOBI header.
fn exth_records(rec0: &[u8], mobi_header_len: usize) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    let start = 16 + mobi_header_len;
    if rec0.len() < start + 12 || rec0.get(start..start + 4) != Some(b"EXTH") {
        return out;
    }
    let count = be_u32(rec0, start + 8).unwrap_or(0) as usize;
    let mut off = start + 12;
    for _ in 0..count {
        let Some(kind) = be_u32(rec0, off) else { break };
        let Some(rlen) = be_u32(rec0, off + 4).map(|n| n as usize) else {
            break;
        };
        if rlen < 8 || off + rlen > rec0.len() {
            break;
        }
        out.push((kind, &rec0[off + 8..off + rlen]));
        off += rlen;
    }
    out
}

/// Collect the `(start, end)` ranges of the image records (from the first image
/// record to the end), so `recindex` can slice image bytes on demand.
fn collect_image_ranges(pdb: &Pdb, first_image: Option<usize>) -> Vec<(usize, usize)> {
    let Some(first) = first_image else {
        return Vec::new();
    };
    (first..pdb.len())
        .filter_map(|i| pdb.record_range(i))
        .collect()
}

/// Rewrite MOBI `<img recindex="N">` to the `src="mobiimg:N"` the block pipeline
/// (and [`MobiContent::blocks`]) understands.
fn rewrite_recindex(html: &str) -> String {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?i)recindex=['"]?0*(\d+)['"]?"#).unwrap());
    RE.replace_all(html, r#"src="mobiimg:$1""#).into_owned()
}

/// Split the concatenated MOBI HTML into sections on `<mbp:pagebreak>` markers
/// (the whole document as one section when there are none).
fn split_sections(html: &str) -> Vec<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<mbp:pagebreak[^>]*>").unwrap());
    let parts: Vec<String> = RE
        .split(html)
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .collect();
    if parts.is_empty() {
        vec![html.to_string()]
    } else {
        parts
    }
}

/// Build a flat TOC/outline: one entry per section, labelled by its first heading
/// (falling back to "Section N"). MOBI's `filepos` NCX is not yet reconstructed.
fn build_navigation(sections: &[String]) -> (Vec<TocEntry>, Vec<OutlineItem>) {
    static HEADING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<h[1-6][^>]*>(.*?)</h[1-6]>").unwrap());
    static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]+>").unwrap());
    let heading = &*HEADING;
    let tags = &*TAGS;
    let mut toc = Vec::with_capacity(sections.len());
    let mut outline = Vec::with_capacity(sections.len());
    for (i, s) in sections.iter().enumerate() {
        let label = heading
            .captures(s)
            .and_then(|c| c.get(1))
            .map(|m| collapse_ws(&tags.replace_all(m.as_str(), " ")))
            .filter(|t| !t.is_empty())
            .map(|t| trim_to(&t, 80))
            .unwrap_or_else(|| format!("Section {}", i + 1));
        toc.push(TocEntry {
            label: label.clone(),
            section: Some(i),
            children: Vec::new(),
        });
        outline.push(OutlineItem {
            label,
            depth: 0,
            section: i,
            locator: None,
        });
    }
    (toc, outline)
}

/// Decode MOBI bytes to a `String` per the text-encoding field (65001 = UTF-8,
/// else Windows-1252).
fn decode_bytes(bytes: &[u8], encoding: u32) -> String {
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

/// Collapse runs of whitespace into single spaces and trim.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to `max` chars, adding an ellipsis when cut.
fn trim_to(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
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
            let (sections, image_ranges, metadata, toc, outline) = {
                let pdb = Pdb::parse(&bytes).ok_or_else(|| anyhow!("not a PalmDB/MOBI file"))?;
                let rec0 = pdb
                    .record(0)
                    .ok_or_else(|| anyhow!("MOBI has no record 0"))?;
                let h = Headers::read(rec0)?;
                let metadata = build_metadata(&pdb, rec0, &h, 0);
                let html = rewrite_recindex(&extract_text(&pdb, &h));
                let sections = split_sections(&html);
                let image_ranges = collect_image_ranges(&pdb, h.first_image);
                let (toc, outline) = build_navigation(&sections);
                (sections, image_ranges, metadata, toc, outline)
            };
            Ok(MobiDocument {
                content: Arc::new(MobiContent {
                    file: bytes,
                    sections,
                    image_ranges,
                }),
                metadata,
                toc,
                outline,
            })
        }
    }
}
