//! Persistent (disk) cache of measured equation-image ink profiles.
//!
//! Sizing a publisher equation raster needs its **ink profile** — the tight ink bbox and
//! the median glyph-line height, measured from the decoded pixels (see
//! [`delryn_media::ink_profile`]). That measurement decodes the image, which for a
//! maths-dense chapter means decoding *hundreds* of tiny JPEGs — and it happens where the
//! section is decoded, including the **start section on the main thread at open**. Nothing
//! cached it, so every reopen paid the full decode cost again before the first paint (the
//! 2–3 s open freeze).
//!
//! The profile depends only on the image **bytes** (it's DPI/theme/geometry-independent),
//! so it is content-addressed: hash the bytes, and a byte-identical image measured in any
//! prior open — of this or another book — is served straight from disk without decoding.
//! The dir is version-stamped ([`VERSION`]); a change to the measurement would bump it so
//! old entries are ignored rather than serving a stale profile.
//!
//! Disabled until [`set_dir`] is called with a real directory (main sets it at startup);
//! tests never set it, so they do no disk I/O and always measure live.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use delryn_model::InkProfile;

/// Bump if the ink-measurement algorithm changes so a stored profile would differ — old
/// entries (under the previous version dir) are then simply ignored.
const VERSION: u32 = 1;

/// The cache directory (`<root>/ink-vN`), set once at startup. Unset ⇒ caching disabled.
static DIR: OnceLock<PathBuf> = OnceLock::new();

/// Point the ink cache at `<root>/ink-vN`, creating it. `None` (or a create failure)
/// leaves the cache disabled — every profile is then measured live, never stored. Called
/// once at startup, before any book (and its background loader) opens.
pub(crate) fn set_dir(root: Option<PathBuf>) {
    let Some(root) = root else { return };
    let dir = root.join(format!("ink-v{VERSION}"));
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = DIR.set(dir);
    }
}

/// Content hash of an image's bytes — the cache key. A byte-identical image (the same
/// glyph raster reused across a book) hashes the same, so it is measured once.
pub(crate) fn hash(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

fn entry_path(dir: &Path, key: u64) -> PathBuf {
    dir.join(format!("{key:016x}.ink"))
}

/// Serialise a measured result. Layout: `[tag u8]`, then when `tag == 1`:
/// `[x0 u32][y0 u32][x1 u32][y1 u32][line_px f32-bits u32][line_count u16]`, little-endian.
fn encode(profile: Option<InkProfile>) -> Vec<u8> {
    let mut data = Vec::with_capacity(23);
    match profile {
        None => data.push(0),
        Some(p) => {
            data.push(1);
            data.extend(p.x0.to_le_bytes());
            data.extend(p.y0.to_le_bytes());
            data.extend(p.x1.to_le_bytes());
            data.extend(p.y1.to_le_bytes());
            data.extend(p.line_px.to_bits().to_le_bytes());
            data.extend(p.line_count.to_le_bytes());
        }
    }
    data
}

/// Parse a stored result: `Some(inner)` where `inner` is the profile or `None` for
/// "measured, not a profiled equation"; the outer `None` means the bytes are corrupt /
/// truncated (treated by [`lookup`] as a miss).
fn decode(data: &[u8]) -> Option<Option<InkProfile>> {
    match data.first()? {
        0 => Some(None),
        1 if data.len() >= 23 => {
            let u32_at =
                |i: usize| u32::from_le_bytes(data[i..i + 4].try_into().ok().unwrap_or([0; 4]));
            Some(Some(InkProfile {
                x0: u32_at(1),
                y0: u32_at(5),
                x1: u32_at(9),
                y1: u32_at(13),
                line_px: f32::from_bits(u32_at(17)),
                line_count: u16::from_le_bytes([data[21], data[22]]),
            }))
        }
        _ => None,
    }
}

/// Look up a measured profile. `None` ⇒ cache miss (measure it); `Some(inner)` ⇒ a stored
/// result (the profile, or `None` for "measured, not a profiled equation").
pub(crate) fn lookup(key: u64) -> Option<Option<InkProfile>> {
    let dir = DIR.get()?;
    let data = std::fs::read(entry_path(dir, key)).ok()?;
    decode(&data)
}

/// Store a measured profile (best-effort — a failure just means a re-measure next time).
/// Written to a temp file then renamed, so a concurrent reader never sees a partial entry.
pub(crate) fn store(key: u64, profile: Option<InkProfile>) {
    let Some(dir) = DIR.get() else { return };
    let data = encode(profile);
    let mut tid = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut tid);
    let tmp = dir.join(format!("{key:016x}.{:x}.tmp", tid.finish()));
    if std::fs::write(&tmp, &data).is_ok() {
        let _ = std::fs::rename(&tmp, entry_path(dir, key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_measured_profile() {
        let p = InkProfile {
            x0: 3,
            y0: 5,
            x1: 128,
            y1: 44,
            line_px: 18.5,
            line_count: 2,
        };
        assert_eq!(
            decode(&encode(Some(p))),
            Some(Some(p)),
            "profile round-trips"
        );
    }

    #[test]
    fn round_trips_a_not_an_equation_result() {
        // A figure/photo measures to `None`; caching that avoids re-decoding it too.
        assert_eq!(
            decode(&encode(None)),
            Some(None),
            "the None marker round-trips"
        );
    }

    #[test]
    fn truncated_bytes_are_a_miss() {
        // A short/garbled entry must read as a miss (re-measure), never a bogus profile.
        assert_eq!(decode(&[]), None, "empty is a miss");
        assert_eq!(decode(&[1, 0, 0]), None, "a truncated Some is a miss");
    }
}
