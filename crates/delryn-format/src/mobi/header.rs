//! Record-0 header parsing: the PalmDOC + MOBI header fields delryn reads, the
//! EXTH metadata records, and the KF8 boundary lookup.
//!
//! All offsets are **record-0-absolute** (measured from the start of record 0,
//! i.e. including the 16-byte PalmDOC header), matching how KindleUnpack's
//! `MobiHeader.getInt` and the MobileRead field tables are numbered.

use anyhow::{Result, bail};

use super::pdb::{be_u16, be_u32};
use super::{COMPRESSION_NONE, NO_INDEX};

/// EXTH record type carrying the KF8 boundary record index in a hybrid MOBI/KF8
/// file (the record where the KF8 rendition's own MOBI header begins).
const EXTH_KF8_BOUNDARY: u32 = 121;

/// The PalmDOC + MOBI header fields we use, read from a record-0-shaped record
/// (record 0 for MOBI6 / standalone KF8, or the boundary record for the KF8 half
/// of a hybrid file).
pub(super) struct Headers {
    pub compression: u16,
    pub text_length: usize,
    pub record_count: usize,
    pub encoding: u32,
    pub mobi_header_len: usize,
    pub full_name: (usize, usize),
    pub first_image: Option<usize>,
    pub extra_flags: u16,
    /// HUFF record index + total HUFF/CDIC record count (only set for type 17480).
    pub huff_record: Option<usize>,
    pub huff_count: usize,
    /// MOBI format version (@0x24). KF8/AZW3 renditions report 8.
    pub version: u32,
    /// NCX index record (@0xF4): the first INDX record built from the book's NCX.
    pub ncx_index: Option<usize>,
    /// KF8 skeleton index (@0xFC) and fragment/divider index (@0xF8).
    pub skel_index: Option<usize>,
    pub frag_index: Option<usize>,
    /// KF8 FDST (flow divider) record index (@0xC0) and count (@0xC4).
    pub fdst_index: Option<usize>,
    pub fdst_count: usize,
}

impl Headers {
    pub(super) fn read(rec0: &[u8]) -> Result<Headers> {
        if rec0.len() < 132 || &rec0[16..20] != b"MOBI" {
            bail!("record 0 is not a MOBI header");
        }
        let compression = be_u16(rec0, 0).unwrap_or(COMPRESSION_NONE);
        let text_length = be_u32(rec0, 4).unwrap_or(0) as usize;
        let record_count = be_u16(rec0, 8).unwrap_or(0) as usize;
        // Encryption type is a u16 at offset 12 (0 = none).
        if be_u16(rec0, 12).unwrap_or(0) != 0 {
            bail!("This book is DRM-protected and can't be opened");
        }
        let mobi_header_len = be_u32(rec0, 20).unwrap_or(0) as usize;
        // Fields past the fixed prefix are only valid if the MOBI header actually
        // reaches them — otherwise the read would land in the EXTH block or the
        // title tail and yield a garbage record index.
        let header_end = 16 + mobi_header_len;
        let field = |off: usize| -> Option<usize> {
            if off + 4 <= header_end {
                match be_u32(rec0, off) {
                    Some(NO_INDEX) | Some(0) | None => None,
                    Some(n) => Some(n as usize),
                }
            } else {
                None
            }
        };

        let encoding = be_u32(rec0, 28).unwrap_or(1252);
        let version = be_u32(rec0, 36).unwrap_or(0);
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
        // HUFF record index (offset 0x70) + total HUFF/CDIC record count (0x74).
        // Only meaningful for compression type 17480; skip the reads otherwise.
        let (huff_record, huff_count) = if compression == super::COMPRESSION_HUFF {
            (
                be_u32(rec0, 112).map(|n| n as usize),
                be_u32(rec0, 116).unwrap_or(0) as usize,
            )
        } else {
            (None, 0)
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
            huff_record,
            huff_count,
            version,
            ncx_index: field(0xF4),
            skel_index: field(0xFC),
            frag_index: field(0xF8),
            fdst_index: field(0xC0),
            fdst_count: be_u32(rec0, 0xC4).unwrap_or(0) as usize,
        })
    }

    /// Does this header describe a KF8 rendition (its own text is KF8 HTML with a
    /// skeleton/fragment structure)? True for standalone AZW3 files.
    pub(super) fn is_kf8(&self) -> bool {
        self.version >= 8 && self.skel_index.is_some() && self.frag_index.is_some()
    }
}

/// Iterate the EXTH records (type, data) if an EXTH header follows the MOBI header.
pub(super) fn exth_records(rec0: &[u8], mobi_header_len: usize) -> Vec<(u32, &[u8])> {
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

/// The KF8 boundary record index (EXTH type 121) of a hybrid MOBI/KF8 file: the
/// record where the KF8 rendition's MOBI header begins. `None` for non-hybrid
/// files (standalone MOBI6 or standalone KF8).
pub(super) fn kf8_boundary(rec0: &[u8], mobi_header_len: usize) -> Option<usize> {
    for (kind, data) in exth_records(rec0, mobi_header_len) {
        if kind == EXTH_KF8_BOUNDARY {
            return match be_u32(data, 0) {
                Some(NO_INDEX) | Some(0) | None => None,
                Some(n) => Some(n as usize),
            };
        }
    }
    None
}
