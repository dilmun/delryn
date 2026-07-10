//! KF8 (AZW3) rendition: reconstruct the real per-file sections from the skeleton
//! and fragment indices, resolve the NCX `pos_fid` targets, and rewrite
//! `kindle:embed:` image references.
//!
//! A KF8 book stores its whole body as one concatenated markup *flow* (flow 0);
//! the **skeleton** index describes each output file's HTML shell (a byte range in
//! the flow) and how many **fragment** records splice into it, and the fragment
//! index gives each fragment's insert position and its own byte range. Rebuilding
//! those files (KindleUnpack `mobi_k8proc.py::buildParts`) restores the sections a
//! reader needs — without it a KF8 book collapses to a single section.
//!
//! A file may be a standalone AZW3 (record 0 is the KF8 header) or the KF8 half of
//! a hybrid MOBI/KF8 file (its header lives at the EXTH-121 boundary record, and
//! every record index inside it is relative to that boundary — hence `base`).

use std::sync::LazyLock;

use anyhow::Result;
use regex::{Captures, Regex};

use super::header::{Headers, kf8_boundary};
use super::index;
use super::pdb::{Pdb, be_u32};
use super::{OutlineItem, Rendition, TocEntry, decode_bytes, ncx};

/// One skeleton entry: the shell's byte range in flow 0 and how many fragments
/// belong to it (KindleUnpack skel table: `frag count`, `pos`, `len`).
struct Skeleton {
    frag_count: usize,
    pos: usize,
    len: usize,
}

/// One fragment entry: where it splices into the skeleton and its byte length
/// (KindleUnpack frag table: `insert pos` = entry name, `length` = tag 6[1]). The
/// fragment bytes follow the skeleton in flow 0, tracked by a running pointer.
struct Fragment {
    insert_pos: usize,
    length: usize,
}

/// Build the KF8 rendition if this file has one. Returns `None` for a plain MOBI6
/// file (the caller then uses the MOBI6 path), or if KF8 reconstruction fails.
pub(super) fn build(pdb: &Pdb, rec0: &[u8], primary: &Headers) -> Result<Option<Rendition>> {
    // Locate the KF8 header + its record base: rec0 itself for a standalone AZW3,
    // or the boundary record for the KF8 half of a hybrid file.
    let (kh, base) = if primary.is_kf8() {
        (Headers::read(rec0)?, 0usize)
    } else if let Some(boundary) = kf8_boundary(rec0, primary.mobi_header_len) {
        let Some(krec) = pdb.record(boundary) else {
            return Ok(None);
        };
        let kh = Headers::read(krec)?;
        if !kh.is_kf8() {
            return Ok(None);
        }
        (kh, boundary)
    } else {
        return Ok(None);
    };

    let (Some(skel_idx), Some(frag_idx)) = (kh.skel_index, kh.frag_index) else {
        return Ok(None);
    };

    // Decompress the KF8 markup (bytes: skeleton/fragment offsets are byte offsets)
    // and bound flow 0 via FDST.
    let huff = super::build_huff(pdb, &kh, base)?;
    let raw = super::extract_bytes(pdb, &kh, huff.as_ref(), base + 1);
    let (f0, f1) = flow0_bounds(pdb, &kh, base, raw.len());
    let flow0 = raw.get(f0..f1).unwrap_or(&raw);

    let skeltbl = read_skeleton(pdb, base + skel_idx);
    let fragtbl = read_fragment(pdb, base + frag_idx);
    if skeltbl.is_empty() {
        return Ok(None);
    }

    // Reassemble one section per skeleton, and record which section owns each
    // fragment (for `pos_fid` NCX resolution). Keep the decoded-but-not-rewritten
    // markup for heading extraction (byte offsets match the reassembled bytes).
    let (parts, frag_owner) = build_parts(flow0, &skeltbl, &fragtbl);
    let raw_sections: Vec<String> = parts.iter().map(|p| decode_bytes(p, kh.encoding)).collect();
    let sections: Vec<String> = raw_sections.iter().map(|s| rewrite_image_srcs(s)).collect();

    // Image/resource records are shared with the MOBI6 rendition in a hybrid file
    // and are located via record 0's `first_image` (the KF8 header's own value is
    // base-relative and points at KF8-only resources); for a standalone AZW3 the
    // two are the same record.
    let image_ranges = super::collect_image_ranges(pdb, primary.first_image);
    let (toc, outline) = build_nav(
        pdb,
        &kh,
        base,
        &raw_sections,
        &skeltbl,
        &fragtbl,
        &frag_owner,
    );
    Ok(Some((sections, image_ranges, toc, outline)))
}

/// The `[start, end)` byte range of flow 0 (the main markup) within `raw`, from the
/// FDST table. Falls back to the whole buffer when FDST is absent or unusable.
fn flow0_bounds(pdb: &Pdb, kh: &Headers, base: usize, raw_len: usize) -> (usize, usize) {
    // KindleUnpack treats an FDST count <= 1 as "no FDST" (one flow = whole file).
    if kh.fdst_count <= 1 {
        return (0, raw_len);
    }
    let Some(fdst) = kh.fdst_index else {
        return (0, raw_len);
    };
    let Some(data) = pdb.record(base + fdst) else {
        return (0, raw_len);
    };
    if data.get(0..4) != Some(b"FDST") {
        return (0, raw_len);
    }
    // The section table at offset 12 is `(start, end)` u32 pairs; flow 0 runs from
    // the first start to the second start (the start of flow 1).
    let start0 = be_u32(data, 12).map(|v| v as usize).unwrap_or(0);
    let start1 = be_u32(data, 20).map(|v| v as usize).unwrap_or(raw_len);
    (
        start0.min(raw_len),
        start1.min(raw_len).max(start0.min(raw_len)),
    )
}

/// Read the skeleton table (tag 1 = fragment count, tag 6 = `[pos, len]`).
fn read_skeleton(pdb: &Pdb, idx: usize) -> Vec<Skeleton> {
    let Some(index) = index::read(pdb, idx) else {
        return Vec::new();
    };
    index
        .entries
        .iter()
        .filter_map(|e| {
            Some(Skeleton {
                frag_count: e.tag(1)? as usize,
                pos: e.tag_at(6, 0)? as usize,
                len: e.tag_at(6, 1)? as usize,
            })
        })
        .collect()
}

/// Read the fragment table (entry name = insert position, tag 6 = `[start, len]`).
fn read_fragment(pdb: &Pdb, idx: usize) -> Vec<Fragment> {
    let Some(index) = index::read(pdb, idx) else {
        return Vec::new();
    };
    index
        .entries
        .iter()
        .filter_map(|e| {
            let insert_pos = std::str::from_utf8(&e.name)
                .ok()?
                .trim()
                .parse::<usize>()
                .ok()?;
            Some(Fragment {
                insert_pos,
                length: e.tag_at(6, 1)? as usize,
            })
        })
        .collect()
}

/// Reassemble each skeleton's file by splicing its fragments' bytes into the shell,
/// returning the parts and a `fragment index → section index` map.
fn build_parts(
    flow0: &[u8],
    skeltbl: &[Skeleton],
    fragtbl: &[Fragment],
) -> (Vec<Vec<u8>>, Vec<usize>) {
    let mut parts = Vec::with_capacity(skeltbl.len());
    let mut frag_owner = vec![0usize; fragtbl.len()];
    let mut fragptr = 0;
    for (section, skel) in skeltbl.iter().enumerate() {
        let mut baseptr = skel.pos + skel.len;
        let mut part = slice(flow0, skel.pos, baseptr).to_vec();
        for _ in 0..skel.frag_count {
            let Some(frag) = fragtbl.get(fragptr) else {
                break;
            };
            if let Some(owner) = frag_owner.get_mut(fragptr) {
                *owner = section;
            }
            let piece = slice(flow0, baseptr, baseptr + frag.length).to_vec();
            let at = frag.insert_pos.saturating_sub(skel.pos).min(part.len());
            part.splice(at..at, piece);
            baseptr += frag.length;
            fragptr += 1;
        }
        parts.push(part);
    }
    (parts, frag_owner)
}

/// `data[start..end]`, clamped to the buffer (KF8 offset tables are untrusted).
fn slice(data: &[u8], start: usize, end: usize) -> &[u8] {
    let start = start.min(data.len());
    let end = end.clamp(start, data.len());
    &data[start..end]
}

/// Navigation from the KF8 NCX: resolve each entry's `pos_fid` to the section that
/// owns that fragment, and label it by the heading at the target (real chapter
/// title) rather than the often-mangled CNCX label. Falls back to per-section
/// headings when there is no NCX.
fn build_nav(
    pdb: &Pdb,
    kh: &Headers,
    base: usize,
    sections: &[String],
    skeltbl: &[Skeleton],
    fragtbl: &[Fragment],
    frag_owner: &[usize],
) -> (Vec<TocEntry>, Vec<OutlineItem>) {
    // The reassembled sections' `<hN>` headings are the most reliable chapter
    // titles — prefer them over the NCX, whose CNCX labels some books store
    // truncated or merged. Fall back to the NCX (then to per-section headings).
    if let Some(nav) = ncx::heading_scan(sections).filter(|(_, o)| o.len() >= 2) {
        return nav;
    }
    if let Some(ncx_idx) = kh.ncx_index
        && let Some(index) = index::read(pdb, base + ncx_idx)
    {
        let entries = ncx::parse(&index, kh.encoding);
        let resolve = |e: &ncx::NcxEntry| -> Option<ncx::Resolved> {
            let (fid, off) = e.pos_fid?;
            let section = *frag_owner.get(fid as usize)?;
            // Map the fragment target into the reassembled section and take the
            // heading there (the real chapter title).
            let heading = skeltbl
                .get(section)
                .zip(fragtbl.get(fid as usize))
                .and_then(|(skel, frag)| {
                    let local = frag.insert_pos.saturating_sub(skel.pos) + off as usize;
                    ncx::heading_at(sections.get(section)?, local)
                });
            let label = heading.clone().unwrap_or_else(|| e.label.clone());
            let locator = heading.or_else(|| (!e.label.is_empty()).then(|| e.label.clone()));
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

/// Rewrite KF8 image references to the `mobiimg:N` form the block pipeline resolves:
/// `kindle:embed:XXXX?mime=…` (base32 image index) → `mobiimg:N`, and any legacy
/// `recindex` too.
fn rewrite_image_srcs(html: &str) -> String {
    static EMBED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?i)kindle:embed:([0-9A-V]+)(?:\?[^"'\s>]*)?"#).unwrap());
    let embedded = EMBED.replace_all(html, |c: &Captures| match from_base32(&c[1]) {
        Some(n) => format!("mobiimg:{n}"),
        None => c[0].to_string(),
    });
    super::rewrite_recindex(&embedded)
}

/// Decode a KF8 base32 number (digits `0-9A-V`).
fn from_base32(s: &str) -> Option<usize> {
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUV";
    let mut value: usize = 0;
    for b in s.bytes() {
        let d = DIGITS.iter().position(|&x| x == b.to_ascii_uppercase())?;
        value = value.checked_mul(32)?.checked_add(d)?;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_decodes_kf8_indices() {
        assert_eq!(from_base32("0001"), Some(1));
        assert_eq!(from_base32("000A"), Some(10));
        assert_eq!(from_base32("0010"), Some(32));
        assert_eq!(from_base32("00V"), Some(31));
    }

    #[test]
    fn kindle_embed_and_recindex_become_mobiimg() {
        // `kindle:embed:XXXX?mime=…` → `mobiimg:N` (base32 decoded, query dropped).
        assert_eq!(
            rewrite_image_srcs(r#"<img src="kindle:embed:0002?mime=image/jpeg"/>"#),
            r#"<img src="mobiimg:2"/>"#
        );
        // Legacy `recindex` is still rewritten.
        assert_eq!(
            rewrite_image_srcs(r#"<img recindex="5"/>"#),
            r#"<img src="mobiimg:5"/>"#
        );
    }
}
