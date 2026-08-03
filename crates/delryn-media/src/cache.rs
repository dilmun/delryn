//! Keeping the on-disk image caches from growing without limit.
//!
//! The caches are version-stamped directories under `<config>/rasters`
//! (`v7` for rasters, `ink-v1` for ink profiles). Versioning meant a pipeline
//! change stopped *reading* stale entries — but nothing ever deleted them, and
//! nothing capped the live one either. Measured on one real library: **1.7 GB**,
//! of which **1.03 GB was dead** — five superseded versions (`v1`, `v2`, `v3`,
//! `v5`, `v6`) left behind by past format bumps. Every future release that bumps
//! a version would have stranded another copy on every user's disk.
//!
//! [`sweep`] fixes both halves: drop directories no longer read at all, then hold
//! what remains under a byte budget by evicting the least recently used files.
//! Everything here is best-effort — a cache is rebuildable by definition, so a
//! permission error or a racing instance costs a re-render, never correctness.

use std::path::{Path, PathBuf};

/// Default ceiling for the live caches, in bytes. Generous enough that ordinary
/// reading never evicts (one real library sat at ~570 MB across the live
/// directories) while bounding the runaway case. Tunable via `cache_limit_mb`.
pub const DEFAULT_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// Remove cache directories that are no longer read, then evict from the ones
/// that are until they fit `budget_bytes`. `budget_bytes == 0` means unlimited,
/// and skips the eviction pass entirely (stale-version removal still happens —
/// that is dead weight at any budget).
///
/// `live` names the directories the current build reads, e.g. `["v7", "ink-v1"]`.
/// Anything else directly under `root` is from a superseded format and goes.
///
/// Returns the number of bytes reclaimed. Cheap on the common path: a directory
/// listing plus, only when over budget, one `stat` per file.
pub fn sweep(root: &Path, live: &[String], budget_bytes: u64) -> u64 {
    let mut freed = remove_stale_versions(root, live);
    if budget_bytes > 0 {
        freed += enforce_budget(root, live, budget_bytes);
    }
    freed
}

/// Delete every immediate subdirectory of `root` that isn't in `live`.
fn remove_stale_versions(root: &Path, live: &[String]) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut freed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Only ever touch our own version directories. A stray file, or a
        // directory shaped like something else, is left alone — this runs
        // against a path the user owns and we are deleting on their behalf.
        if !is_version_dir(&name) || live.iter().any(|l| l == name.as_ref()) {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let size = dir_size(&path);
        if std::fs::remove_dir_all(&path).is_ok() {
            freed += size;
        }
    }
    freed
}

/// Is `name` one of our version-stamped cache directories (`v7`, `ink-v1`)?
/// Deliberately strict so an unrelated directory can never match.
fn is_version_dir(name: &str) -> bool {
    let digits = name
        .strip_prefix("ink-v")
        .or_else(|| name.strip_prefix('v'));
    matches!(digits, Some(d) if !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// Evict least-recently-used files from the live directories until their total
/// is within `budget`.
///
/// Ordered by access time where the filesystem records it, falling back to
/// modification time — so the pages you actually re-read survive and the book you
/// opened once in March is what goes. Files are deleted individually rather than
/// by directory: an entry is independently rebuildable, so partial eviction is a
/// slower re-render, never a broken cache.
fn enforce_budget(root: &Path, live: &[String], budget: u64) -> u64 {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let mut total: u64 = 0;
    for dir in live {
        collect_files(&root.join(dir), &mut files, &mut total);
    }
    if total <= budget {
        return 0;
    }
    // Oldest first, so the tail of the vector is what we keep.
    files.sort_by_key(|(when, _, _)| *when);
    let mut freed = 0;
    for (_, size, path) in files {
        if total.saturating_sub(freed) <= budget {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            freed += size;
        }
    }
    freed
}

/// Recursively gather `(atime, size, path)` for every file under `dir`.
fn collect_files(
    dir: &Path,
    out: &mut Vec<(std::time::SystemTime, u64, PathBuf)>,
    total: &mut u64,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(md) = entry.metadata() else { continue };
        if md.is_dir() {
            collect_files(&path, out, total);
        } else if md.is_file() {
            let when = md
                .accessed()
                .or_else(|_| md.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            let size = disk_size(&md);
            *total += size;
            out.push((when, size, path));
        }
    }
}

/// Space a file actually occupies, not the length of its contents.
///
/// These caches are mostly small files, and a filesystem allocates whole blocks:
/// the ink cache measured 9,337 files holding under a megabyte of data but
/// **36 MB of disk**. Budgeting by `len()` would let the cache take several times
/// its stated limit of the space the user cares about. `blocks()` is in 512-byte
/// units by definition, whatever the filesystem's own block size.
#[cfg(unix)]
fn disk_size(md: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.blocks() * 512
}

#[cfg(not(unix))]
fn disk_size(md: &std::fs::Metadata) -> u64 {
    md.len()
}

/// Total disk space used by the files under `dir`.
fn dir_size(dir: &Path) -> u64 {
    let mut files = Vec::new();
    let mut total = 0;
    collect_files(dir, &mut files, &mut total);
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("delryn_cache_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &Path, name: &str, bytes: usize) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), vec![b'x'; bytes]).unwrap();
    }

    /// The 1.03 GB case: five superseded version directories, none of them read
    /// any more, none of them ever deleted.
    #[test]
    fn superseded_versions_are_removed_and_live_ones_kept() {
        let root = scratch("stale");
        for v in ["v1", "v2", "v3", "v5", "v6"] {
            write(&root.join(v), "old.png", 1000);
        }
        write(&root.join("v7"), "current.png", 500);
        write(&root.join("ink-v1"), "ink.bin", 100);

        let live = vec!["v7".to_string(), "ink-v1".to_string()];
        let freed = sweep(&root, &live, 0);

        // Blocks, not byte lengths — five 1000-byte files each occupy at least a block.
        assert!(
            freed >= 5000,
            "every superseded version reclaimed, freed {freed}"
        );
        for v in ["v1", "v2", "v3", "v5", "v6"] {
            assert!(!root.join(v).exists(), "{v} removed");
        }
        assert!(
            root.join("v7/current.png").exists(),
            "live raster cache kept"
        );
        assert!(root.join("ink-v1/ink.bin").exists(), "live ink cache kept");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Anything that isn't one of our version directories is never touched —
    /// this deletes from a directory the user owns.
    #[test]
    fn unrelated_entries_are_left_alone() {
        let root = scratch("unrelated");
        write(&root.join("v7"), "a.png", 10);
        write(&root.join("notes"), "keep.txt", 10);
        std::fs::write(root.join("stray.txt"), b"keep").unwrap();

        sweep(&root, &["v7".to_string()], 0);

        assert!(root.join("notes/keep.txt").exists(), "unrelated dir kept");
        assert!(root.join("stray.txt").exists(), "stray file kept");

        assert!(is_version_dir("v7") && is_version_dir("ink-v1"));
        assert!(!is_version_dir("notes") && !is_version_dir("v") && !is_version_dir("vX"));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Over budget, the oldest entries go and the newest survive.
    #[test]
    fn eviction_drops_the_least_recently_used_first() {
        let root = scratch("budget");
        let live = root.join("v7");
        // Sized well above a filesystem block so block rounding can't distort the
        // arithmetic the way it does for the tiny ink-cache entries.
        const EACH: usize = 8192;
        for (i, name) in ["oldest", "middle", "newest"].iter().enumerate() {
            write(&live, name, EACH);
            // Stamp distinct, increasing times so the ordering is deterministic
            // rather than dependent on filesystem timestamp granularity.
            let t =
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + i as u64 * 60);
            let f = std::fs::File::options()
                .write(true)
                .open(live.join(name))
                .unwrap();
            f.set_times(std::fs::FileTimes::new().set_accessed(t).set_modified(t))
                .unwrap();
        }

        // Budget fits one file but not two, so two must go.
        let freed = sweep(&root, &["v7".to_string()], (EACH + EACH / 2) as u64);

        assert!(
            freed >= 2 * EACH as u64,
            "evicted down to the budget, freed {freed}"
        );
        assert!(!live.join("oldest").exists(), "oldest evicted");
        assert!(live.join("newest").exists(), "newest survives");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A zero budget means unlimited: nothing live is evicted, however big.
    #[test]
    fn zero_budget_keeps_everything_live() {
        let root = scratch("unlimited");
        write(&root.join("v7"), "big.png", 100_000);
        write(&root.join("v1"), "dead.png", 10);

        let freed = sweep(&root, &["v7".to_string()], 0);

        assert!(
            freed >= 10,
            "only the superseded version went, freed {freed}"
        );
        assert!(root.join("v7/big.png").exists(), "live cache untouched");

        let _ = std::fs::remove_dir_all(&root);
    }
}
