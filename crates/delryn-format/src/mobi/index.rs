//! Generic INDX index parsing. The NCX table of contents and the KF8 skeleton,
//! fragment, and guide tables all share this on-disk structure: an INDX *header*
//! record (magic `INDX` + a `TAGX` tag table), one or more INDX *data* records
//! (each an IDXT offset table plus control-byte-encoded entries), and trailing
//! CNCX / CTOC label-string records.
//!
//! Ported from KindleUnpack's `mobi_index.py` (`getIndexData`, `readTagSection`,
//! `getTagMap`, `readCTOC`, `getVariableWidthValue`). All reads are bounds-checked
//! so a malformed or adversarial index yields `None` rather than panicking; the
//! caller then falls back to heading-based navigation.

use std::collections::BTreeMap;

use super::pdb::{Pdb, be_u16, be_u32};

/// One decoded index entry: its raw name/prefix bytes (the length-prefixed text
/// before the control bytes) plus the decoded tag → values map.
pub(super) struct IndexEntry {
    pub name: Vec<u8>,
    pub tags: BTreeMap<u8, Vec<u32>>,
}

impl IndexEntry {
    /// First value of `tag`, if present.
    pub fn tag(&self, tag: u8) -> Option<u32> {
        self.tags.get(&tag).and_then(|v| v.first().copied())
    }

    /// `i`-th value of `tag`, if present.
    pub fn tag_at(&self, tag: u8, i: usize) -> Option<u32> {
        self.tags.get(&tag).and_then(|v| v.get(i).copied())
    }
}

/// A parsed INDX index: its entries plus the CNCX label strings, keyed by their
/// offset within the CNCX key space (each CNCX record occupies a 0x10000 band).
pub(super) struct Index {
    pub entries: Vec<IndexEntry>,
    pub cncx: BTreeMap<u32, Vec<u8>>,
}

impl Index {
    /// The raw CNCX label bytes at `offset`, if present.
    pub fn label(&self, offset: u32) -> Option<&[u8]> {
        self.cncx.get(&offset).map(Vec::as_slice)
    }
}

/// Guard against a crafted header advertising a huge record/entry count driving a
/// multi-gigabyte reservation (the records themselves bound the real work).
const MAX_INDEX_RECORDS: usize = 1 << 16;
const MAX_ENTRIES_PER_RECORD: usize = 1 << 20;

/// Parse the INDX index whose header record is at PDB index `idx`. `None` if the
/// structure is missing or malformed.
pub(super) fn read(pdb: &Pdb, idx: usize) -> Option<Index> {
    let header = pdb.record(idx)?;
    if header.get(0..4)? != b"INDX" {
        return None;
    }
    // 13 big-endian u32 words at offset 4; we use len (TAGX offset), count (number
    // of data records), and nctoc (number of CNCX records).
    let tagx_offset = be_u32(header, 4)? as usize;
    let record_count = (be_u32(header, 24)? as usize).min(MAX_INDEX_RECORDS);
    let cncx_count = (be_u32(header, 52)? as usize).min(MAX_INDEX_RECORDS);

    let (control_byte_count, tag_table) = read_tagx(header, tagx_offset)?;

    // CNCX records follow the data records; each record's local offsets live in a
    // distinct 0x10000 band so a tag offset selects both the record and the string.
    let mut cncx = BTreeMap::new();
    let cncx_start = idx + record_count + 1;
    for j in 0..cncx_count {
        let Some(cdata) = pdb.record(cncx_start + j) else {
            break;
        };
        read_ctoc(cdata, (j as u32) * 0x10000, &mut cncx);
    }

    let mut entries = Vec::new();
    for rec in (idx + 1)..(idx + 1 + record_count) {
        let Some(data) = pdb.record(rec) else { break };
        if data.get(0..4) != Some(b"INDX") {
            continue;
        }
        let idxt_pos = be_u32(data, 20)? as usize;
        let entry_count = (be_u32(data, 24)? as usize).min(MAX_ENTRIES_PER_RECORD);

        // IDXT: `entry_count` big-endian u16 entry offsets after the "IDXT" magic,
        // then the IDXT position itself bounds the last entry.
        let mut positions = Vec::with_capacity(entry_count + 1);
        for j in 0..entry_count {
            let Some(p) = be_u16(data, idxt_pos + 4 + 2 * j) else {
                break;
            };
            positions.push(p as usize);
        }
        positions.push(idxt_pos);

        for w in positions.windows(2) {
            let (start, end) = (w[0], w[1]);
            let Some(&text_len) = data.get(start) else {
                continue;
            };
            let name_start = start + 1;
            let name_end = name_start + text_len as usize;
            let Some(name) = data.get(name_start..name_end) else {
                continue;
            };
            let tags = get_tag_map(control_byte_count, &tag_table, data, name_end, end);
            entries.push(IndexEntry {
                name: name.to_vec(),
                tags,
            });
        }
    }

    Some(Index { entries, cncx })
}

/// A TAGX tag definition: `(tag, values_per_entry, mask, end_flag)`.
type TagDef = (u8, u8, u8, u8);

/// Read the TAGX tag table: `(control_byte_count, tag definitions)`.
fn read_tagx(data: &[u8], start: usize) -> Option<(usize, Vec<TagDef>)> {
    if data.get(start..start + 4)? != b"TAGX" {
        return None;
    }
    let first_entry_offset = be_u32(data, start + 4)? as usize;
    let control_byte_count = be_u32(data, start + 8)? as usize;
    let mut tags = Vec::new();
    let mut i = 12;
    while i + 4 <= first_entry_offset {
        let pos = start + i;
        match (
            data.get(pos),
            data.get(pos + 1),
            data.get(pos + 2),
            data.get(pos + 3),
        ) {
            (Some(&t), Some(&v), Some(&m), Some(&e)) => tags.push((t, v, m, e)),
            _ => break,
        }
        i += 4;
    }
    Some((control_byte_count, tags))
}

/// Decode one index entry's tag values from its control bytes and the variable
/// width value stream (KindleUnpack `getTagMap`).
fn get_tag_map(
    control_byte_count: usize,
    tag_table: &[TagDef],
    data: &[u8],
    start_pos: usize,
    _end_pos: usize,
) -> BTreeMap<u8, Vec<u32>> {
    // Phase 1: walk the tag table against the control bytes to learn, per present
    // tag, how many values follow (either a value count or a raw byte count).
    struct Pending {
        tag: u8,
        value_count: Option<u32>,
        value_bytes: Option<u32>,
        values_per_entry: u8,
    }
    let mut pending: Vec<Pending> = Vec::new();
    let mut control_byte_index = 0usize;
    let mut cursor = start_pos + control_byte_count;

    for &(tag, values_per_entry, mask, end_flag) in tag_table {
        if end_flag == 0x01 {
            control_byte_index += 1;
            continue;
        }
        let Some(&cbyte) = data.get(start_pos + control_byte_index) else {
            continue;
        };
        let mut value = (cbyte & mask) as u32;
        if value == 0 {
            continue;
        }
        if value == mask as u32 {
            if count_set_bits(mask) > 1 {
                // A byte count (not a value count) follows after the control bytes.
                let (consumed, byte_count) = get_var_width(data, cursor);
                if consumed == 0 {
                    break;
                }
                cursor += consumed;
                pending.push(Pending {
                    tag,
                    value_count: None,
                    value_bytes: Some(byte_count),
                    values_per_entry,
                });
            } else {
                pending.push(Pending {
                    tag,
                    value_count: Some(1),
                    value_bytes: None,
                    values_per_entry,
                });
            }
        } else {
            let mut m = mask;
            while m & 0x01 == 0 {
                m >>= 1;
                value >>= 1;
            }
            pending.push(Pending {
                tag,
                value_count: Some(value),
                value_bytes: None,
                values_per_entry,
            });
        }
    }

    // Phase 2: read the variable-width values for each present tag.
    let mut out: BTreeMap<u8, Vec<u32>> = BTreeMap::new();
    for p in pending {
        let mut values = Vec::new();
        match p.value_count {
            Some(count) => {
                for _ in 0..(count as usize).saturating_mul(p.values_per_entry as usize) {
                    let (consumed, v) = get_var_width(data, cursor);
                    if consumed == 0 {
                        break;
                    }
                    cursor += consumed;
                    values.push(v);
                }
            }
            None => {
                let byte_count = p.value_bytes.unwrap_or(0) as usize;
                let mut total = 0;
                while total < byte_count {
                    let (consumed, v) = get_var_width(data, cursor);
                    if consumed == 0 {
                        break;
                    }
                    cursor += consumed;
                    total += consumed;
                    values.push(v);
                }
            }
        }
        out.insert(p.tag, values);
    }
    out
}

/// Decode a forward variable-width value: 7 bits per byte, big-endian, terminated
/// by the byte whose high bit is set. Returns `(bytes_consumed, value)`;
/// `consumed == 0` means the offset was out of range (callers must stop).
fn get_var_width(data: &[u8], offset: usize) -> (usize, u32) {
    let mut value: u32 = 0;
    let mut consumed = 0;
    while let Some(&b) = data.get(offset + consumed) {
        consumed += 1;
        value = (value << 7) | (b & 0x7f) as u32;
        if b & 0x80 != 0 {
            break;
        }
    }
    (consumed, value)
}

/// Parse a CNCX / CTOC record into `out`, keying each label by `base + its local
/// offset`. Each label is a forward-varint length followed by that many bytes.
fn read_ctoc(data: &[u8], base: u32, out: &mut BTreeMap<u32, Vec<u8>>) {
    let mut offset = 0usize;
    while offset < data.len() {
        if data[offset] == 0 {
            break;
        }
        let key = base + offset as u32;
        let (consumed, len) = get_var_width(data, offset);
        if consumed == 0 {
            break;
        }
        offset += consumed;
        let end = offset + len as usize;
        let Some(name) = data.get(offset..end) else {
            break;
        };
        out.insert(key, name.to_vec());
        offset = end;
    }
}

/// Number of set bits in a byte.
fn count_set_bits(mut v: u8) -> u32 {
    let mut count = 0;
    for _ in 0..8 {
        count += (v & 1) as u32;
        v >>= 1;
    }
    count
}
