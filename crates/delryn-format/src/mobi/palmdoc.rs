//! PalmDOC (LZ77) decompression — the compression used by Mobipocket text
//! records (compression type 2). Uncompressed records (type 1) are copied
//! verbatim; HUFF/CDIC (type 17480) is not handled here.
//!
//! The byte-code, decoded left to right (see the MobileRead MOBI/PalmDOC spec):
//! - `0x00`            → a literal NUL.
//! - `0x01..=0x08`     → copy the next *n* bytes verbatim (a literal run).
//! - `0x09..=0x7F`     → a literal (printable ASCII) byte.
//! - `0x80..=0xBF`     → a length/distance back-reference (this byte + the next):
//!   from the 14 low bits, distance = bits>>3, length = (bits&7)+3.
//! - `0xC0..=0xFF`     → a space followed by `byte & 0x7F` (a common bigram).

/// Decompress one PalmDOC record. Malformed back-references are skipped rather
/// than panicking, so a corrupt record degrades gracefully instead of aborting.
pub fn decompress(input: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(input.len() * 4);
    let mut i = 0;
    while i < input.len() {
        let c = input[i];
        i += 1;
        match c {
            0x00 => out.push(0),
            0x01..=0x08 => {
                // The next `c` bytes are literal.
                let n = c as usize;
                let end = (i + n).min(input.len());
                out.extend_from_slice(&input[i..end]);
                i = end;
            }
            0x09..=0x7F => out.push(c),
            0x80..=0xBF => {
                // A two-byte length/distance pair.
                if i >= input.len() {
                    break;
                }
                let bits = ((c as usize) << 8 | input[i] as usize) & 0x3FFF;
                i += 1;
                let distance = bits >> 3;
                let length = (bits & 0x07) + 3;
                if distance == 0 || distance > out.len() {
                    continue; // corrupt reference — skip
                }
                // Copy `length` bytes from `distance` behind; may overlap (the copy
                // extends as it reads, per LZ77), so copy one byte at a time. `start`
                // is fixed; `start + k` trails the growing tail exactly.
                let start = out.len() - distance;
                for k in 0..length {
                    let b = out[start + k];
                    out.push(b);
                }
            }
            0xC0..=0xFF => {
                out.push(b' ');
                out.push(c & 0x7F);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_and_literal_runs() {
        // 'H','i' as plain literals; then a 2-byte literal run of "!!".
        let input = [b'H', b'i', 0x02, b'!', b'!'];
        assert_eq!(decompress(&input), b"Hi!!");
    }

    #[test]
    fn space_bigram_pair() {
        // 0xC0..=0xFF → space + (byte & 0x7f). 0xC1 → " A", 0xE5 → " e".
        assert_eq!(decompress(&[0xC1]), b" A");
        assert_eq!(decompress(&[0xE5]), b" e");
    }

    #[test]
    fn back_reference_repeats_prior_text() {
        // Emit "ab", then a back-reference distance=2 length=3 → copies "aba"
        // (overlapping), total "ababa". bits = distance<<3 | (length-3) = 16|0 = 16.
        // Encode into 0x80..=0xBF marker: value 0x8000 | 16 = 0x8010 → [0x80, 0x10].
        let input = [b'a', b'b', 0x80, 0x10];
        assert_eq!(decompress(&input), b"ababa");
    }

    #[test]
    fn corrupt_reference_is_skipped_not_panicked() {
        // A back-reference with distance beyond the output so far is ignored.
        let input = [0x80, 0x10, b'x'];
        assert_eq!(decompress(&input), b"x");
    }
}
