//! HUFF/CDIC decompression — Mobipocket text compression type 17480 (Amazon's
//! Huffman coder, used by most Amazon-distributed MOBI/AZW). Compression types 1
//! (none) and 2 (PalmDOC) are handled elsewhere; see `palmdoc.rs`.
//!
//! The text records are a Huffman bit-stream over a phrase dictionary:
//!
//! - The **HUFF** record holds two tables. A 256-entry *cache* (`dict1`), indexed by
//!   the top byte of the next 32 bits, gives each prefix a code length + a "terminal"
//!   flag + a `maxcode` bound. A 64-entry *base* table gives, per code length 1..=32,
//!   the `mincode`/`maxcode` 32-bit code bounds for the canonical Huffman assignment.
//! - The **CDIC** record(s) hold the phrase dictionary: each entry is a byte string
//!   plus a "terminal" flag. A non-terminal entry is *itself* a HUFF stream that
//!   expands to more phrases — we expand every phrase once, up front.
//! - Decoding a text record reads 32-bit windows MSB-first; each Huffman symbol
//!   resolves to a dictionary index whose (expanded) bytes are appended.
//!
//! Ported from KindleUnpack's `mobi_huff` / Calibre's `mobihuff`. Every offset and
//! length is bounds-checked so a crafted file cannot panic; a corrupt stream degrades
//! to best-effort output, matching `palmdoc::decompress`. DRM is out of scope.

use anyhow::{Result, anyhow, ensure};

use super::pdb::{Pdb, be_u16, be_u32};

/// Guards against a crafted dictionary whose non-terminal phrases reference each
/// other in a cycle (real files nest only a few levels deep).
const MAX_PHRASE_DEPTH: usize = 32;

/// A prepared HUFF/CDIC decoder: the Huffman tables plus the fully-expanded phrase
/// dictionary. Built once per document, then applied to each text record.
pub struct HuffCdic {
    /// Per leading byte: `(code_len, terminal, maxcode)`.
    dict1: [(u32, bool, u32); 256],
    /// Per code length `1..=32`: lower/upper 32-bit code bound (index 0 unused).
    mincode: [u32; 33],
    maxcode: [u32; 33],
    /// Fully-expanded dictionary phrases (terminal byte sequences).
    phrases: Vec<Vec<u8>>,
}

impl HuffCdic {
    /// Build the decoder from the HUFF record at `huff_record` and the CDIC records
    /// that follow it. `huff_count` is the total number of HUFF/CDIC records (one
    /// HUFF + the rest CDIC), from MOBI-header offsets 0x70/0x74.
    pub fn from_records(pdb: &Pdb, huff_record: usize, huff_count: usize) -> Result<HuffCdic> {
        let huff = pdb
            .record(huff_record)
            .ok_or_else(|| anyhow!("HUFF record {huff_record} missing"))?;
        ensure!(
            huff.len() >= 24 && &huff[0..4] == b"HUFF",
            "not a HUFF record"
        );
        let off1 = be_u32(huff, 8).unwrap_or(0) as usize;
        let off2 = be_u32(huff, 12).unwrap_or(0) as usize;
        ensure!(
            off1.saturating_add(256 * 4) <= huff.len() && off2.saturating_add(64 * 4) <= huff.len(),
            "HUFF tables out of range"
        );

        // dict1: 256 cache entries. Each: code_len (low 5 bits), terminal (bit 7),
        // and a maxcode transformed to a left-aligned 32-bit bound.
        let mut dict1 = [(0u32, false, 0u32); 256];
        for (i, slot) in dict1.iter_mut().enumerate() {
            let e = be_u32(huff, off1 + i * 4).unwrap_or(0);
            let codelen = e & 0x1f;
            ensure!((1..=32).contains(&codelen), "bad HUFF code length");
            let term = e & 0x80 != 0;
            let maxcode = ((e >> 8).wrapping_add(1) << (32 - codelen)).wrapping_sub(1);
            *slot = (codelen, term, maxcode);
        }

        // dict2: 32 (mincode, maxcode) pairs, one per code length 1..=32.
        let mut mincode = [0u32; 33];
        let mut maxcode = [0u32; 33];
        for k in 1..=32usize {
            let mn = be_u32(huff, off2 + (k - 1) * 8).unwrap_or(0);
            let mx = be_u32(huff, off2 + (k - 1) * 8 + 4).unwrap_or(0);
            let shift = 32 - k as u32; // 0..=31, so `<<` never over-shifts
            mincode[k] = mn << shift;
            maxcode[k] = (mx.wrapping_add(1) << shift).wrapping_sub(1);
        }

        // CDIC records: the phrase dictionary (raw, not yet expanded).
        let mut raw: Vec<(Vec<u8>, bool)> = Vec::new();
        let mut total = 0usize;
        let mut per_record = 0usize;
        for r in (huff_record + 1)..(huff_record + huff_count) {
            let Some(cdic) = pdb.record(r) else { break };
            if cdic.len() < 16 || &cdic[0..4] != b"CDIC" {
                break; // stop at the first non-CDIC record (count may include a trailer)
            }
            if raw.is_empty() {
                total = be_u32(cdic, 8).unwrap_or(0) as usize;
                let code_length = be_u32(cdic, 12).unwrap_or(0);
                per_record = 1usize.checked_shl(code_length).unwrap_or(usize::MAX);
            }
            let n = per_record.min(total.saturating_sub(raw.len()));
            for j in 0..n {
                let Some(off) = be_u16(cdic, 16 + j * 2).map(|o| o as usize) else {
                    break;
                };
                let Some(w) = be_u16(cdic, 16 + off) else {
                    break;
                };
                let len = (w & 0x7fff) as usize;
                let term = w & 0x8000 != 0;
                let (start, end) = (18 + off, 18 + off + len);
                if end > cdic.len() {
                    break;
                }
                raw.push((cdic[start..end].to_vec(), term));
            }
        }
        ensure!(!raw.is_empty(), "no CDIC dictionary phrases");

        // Expand non-terminal phrases (each is itself a HUFF stream) into literal
        // bytes, memoized so shared sub-phrases are decoded once.
        let mut cache: Vec<Option<Vec<u8>>> = vec![None; raw.len()];
        for r in 0..raw.len() {
            expand(&dict1, &mincode, &maxcode, &raw, &mut cache, r, 0)?;
        }
        let phrases = cache.into_iter().map(Option::unwrap_or_default).collect();

        Ok(HuffCdic {
            dict1,
            mincode,
            maxcode,
            phrases,
        })
    }

    /// Decompress one (already trailing-stripped) text record. Best-effort: an
    /// out-of-range symbol is skipped rather than panicking.
    pub fn decompress(&self, record: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in decode_indices(&self.dict1, &self.mincode, &self.maxcode, record) {
            if let Some(p) = self.phrases.get(r) {
                out.extend_from_slice(p);
            }
        }
        out
    }
}

/// Read 32 bits MSB-first starting at `bit_pos`, zero-padded past the end.
fn peek32(data: &[u8], bit_pos: usize) -> u32 {
    let byte = bit_pos / 8;
    let shift = (bit_pos % 8) as u32;
    // 5 bytes = 40 bits cover any 32-bit window that straddles a byte boundary.
    let mut acc: u64 = 0;
    for i in 0..5 {
        acc = (acc << 8) | *data.get(byte + i).unwrap_or(&0) as u64;
    }
    ((acc >> (8 - shift)) & 0xffff_ffff) as u32
}

/// Decode a HUFF bit-stream into the sequence of dictionary indices it encodes.
/// Pure over the Huffman tables — used both to decode text records and to expand
/// non-terminal dictionary phrases.
fn decode_indices(
    dict1: &[(u32, bool, u32); 256],
    mincode: &[u32; 33],
    maxcode: &[u32; 33],
    data: &[u8],
) -> Vec<usize> {
    let mut out = Vec::new();
    let mut bits_left = data.len() as isize * 8;
    let mut bit_pos = 0usize;
    while bits_left > 0 {
        let code = peek32(data, bit_pos);
        let (mut codelen, term, mut mx) = dict1[(code >> 24) as usize];
        if !term {
            while codelen < 32 && code < mincode[codelen as usize] {
                codelen += 1;
            }
            mx = maxcode[codelen as usize];
        }
        bits_left -= codelen as isize;
        if bits_left < 0 {
            break;
        }
        bit_pos += codelen as usize;
        out.push((mx.wrapping_sub(code) >> (32 - codelen)) as usize);
    }
    out
}

/// Recursively expand phrase `r` into its literal bytes, caching the result.
fn expand(
    dict1: &[(u32, bool, u32); 256],
    mincode: &[u32; 33],
    maxcode: &[u32; 33],
    raw: &[(Vec<u8>, bool)],
    cache: &mut [Option<Vec<u8>>],
    r: usize,
    depth: usize,
) -> Result<Vec<u8>> {
    let Some((bytes, term)) = raw.get(r) else {
        return Ok(Vec::new()); // out-of-range reference → empty
    };
    if let Some(done) = &cache[r] {
        return Ok(done.clone());
    }
    ensure!(depth < MAX_PHRASE_DEPTH, "CDIC phrase recursion too deep");
    let result = if *term {
        bytes.clone()
    } else {
        let mut acc = Vec::new();
        for idx in decode_indices(dict1, mincode, maxcode, bytes) {
            acc.extend_from_slice(&expand(
                dict1,
                mincode,
                maxcode,
                raw,
                cache,
                idx,
                depth + 1,
            )?);
        }
        acc
    };
    cache[r] = Some(result.clone());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peek32_reads_msb_first() {
        let data = [0xAB, 0xCD, 0xEF, 0x12, 0x34];
        assert_eq!(peek32(&data, 0), 0xABCD_EF12);
        assert_eq!(peek32(&data, 4), 0xBCDE_F123); // nibble-shifted window
        assert_eq!(peek32(&data, 8), 0xCDEF_1234);
    }

    #[test]
    fn peek32_zero_pads_past_end() {
        // Only two bytes: the window past them reads as zeros.
        assert_eq!(peek32(&[0xFF, 0x80], 0), 0xFF80_0000);
        assert_eq!(peek32(&[], 0), 0);
    }
}
