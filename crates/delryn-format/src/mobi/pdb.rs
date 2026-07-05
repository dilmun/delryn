//! PalmDB (PDB) container parsing — the record framing every MOBI/AZW3 file uses.
//! All PDB/MOBI integers are big-endian.

/// A parsed PalmDB: the byte ranges of each record within the file.
pub struct Pdb<'a> {
    data: &'a [u8],
    /// Record data start offsets, with the file length appended as the final bound.
    offsets: Vec<usize>,
}

impl<'a> Pdb<'a> {
    /// Parse the record table. `None` if the header is too short or the record
    /// offsets are inconsistent (so a non-PDB / truncated file fails cleanly).
    pub fn parse(data: &'a [u8]) -> Option<Pdb<'a>> {
        // 78-byte fixed header; record count is a u16 at offset 76; the record-info
        // list (8 bytes per record) starts at offset 78.
        if data.len() < 78 {
            return None;
        }
        let count = be_u16(data, 76)? as usize;
        let list_end = 78 + count.checked_mul(8)?;
        if count == 0 || data.len() < list_end {
            return None;
        }
        let mut offsets = Vec::with_capacity(count + 1);
        for i in 0..count {
            let off = be_u32(data, 78 + i * 8)? as usize;
            if off > data.len() {
                return None;
            }
            offsets.push(off);
        }
        // Offsets are non-decreasing; append the file end as the last record's bound.
        if offsets.windows(2).any(|w| w[1] < w[0]) {
            return None;
        }
        offsets.push(data.len());
        Some(Pdb { data, offsets })
    }

    /// Number of records.
    pub fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    /// Record `i`'s bytes (`None` if out of range).
    pub fn record(&self, i: usize) -> Option<&'a [u8]> {
        let (start, end) = self.record_range(i)?;
        self.data.get(start..end)
    }

    /// Record `i`'s `(start, end)` byte range in the file (`None` if out of range).
    pub fn record_range(&self, i: usize) -> Option<(usize, usize)> {
        Some((*self.offsets.get(i)?, *self.offsets.get(i + 1)?))
    }
}

/// Read a big-endian `u16` at `o` (`None` if out of range).
pub(super) fn be_u16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(o)?, *d.get(o + 1)?]))
}

/// Read a big-endian `u32` at `o` (`None` if out of range).
pub(super) fn be_u32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *d.get(o)?,
        *d.get(o + 1)?,
        *d.get(o + 2)?,
        *d.get(o + 3)?,
    ]))
}

/// Build a minimal PDB with the given record payloads (test helper, shared with
/// the `mobi` module's tests).
#[cfg(test)]
pub(super) fn build_pdb(records: &[&[u8]]) -> Vec<u8> {
    let count = records.len();
    let header_end = 78 + count * 8;
    let mut out = vec![0u8; header_end];
    // type/creator so type_creator() is meaningful.
    out[60..64].copy_from_slice(b"BOOK");
    out[64..68].copy_from_slice(b"MOBI");
    out[76..78].copy_from_slice(&(count as u16).to_be_bytes());
    let mut off = header_end;
    for (i, r) in records.iter().enumerate() {
        out[78 + i * 8..78 + i * 8 + 4].copy_from_slice(&(off as u32).to_be_bytes());
        off += r.len();
    }
    for r in records {
        out.extend_from_slice(r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records() {
        let data = build_pdb(&[b"record-zero", b"rec1", b""]);
        let pdb = Pdb::parse(&data).unwrap();
        assert_eq!(pdb.len(), 3);
        assert_eq!(pdb.record(0), Some(&b"record-zero"[..]));
        assert_eq!(pdb.record(1), Some(&b"rec1"[..]));
        assert_eq!(pdb.record(2), Some(&b""[..]));
        assert_eq!(pdb.record(3), None);
        assert_eq!(pdb.record_range(0), Some((102, 113)));
    }

    #[test]
    fn rejects_too_short_or_empty() {
        assert!(Pdb::parse(&[0u8; 10]).is_none());
        assert!(Pdb::parse(&build_pdb(&[])).is_none()); // zero records
    }
}
