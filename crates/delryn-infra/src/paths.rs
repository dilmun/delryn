//! Where delryn keeps its data — the single config/data directory shared by the
//! config file, the SQLite store, and the cover cache.

use std::io;
use std::path::{Path, PathBuf};

/// The delryn config/data directory: `$XDG_CONFIG_HOME/delryn` or `~/.config/delryn`
/// (per the project's single-dir decision), with a Windows fallback.
pub fn config_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME")
        && !x.is_empty()
    {
        return PathBuf::from(x).join("delryn");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("delryn");
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        return PathBuf::from(appdata).join("delryn");
    }
    PathBuf::from(".delryn")
}

/// Directory holding user-authored theme files (`<config>/themes/*.toml`). Loaded
/// alongside the built-in themes at startup.
pub fn themes_dir() -> PathBuf {
    config_dir().join("themes")
}

/// Scratch directory for this user's transient files — logs, crash reports, and
/// the page rasters handed to the terminal: `$TMPDIR/delryn-<uid>`.
///
/// Everything transient used to sit directly in `$TMPDIR` under a fixed name
/// (`delryn.log`, `delryn-crash.log`, `delryn-page-N.png`). That is fine on macOS,
/// where each user gets a private `$TMPDIR`, but on Linux `$TMPDIR` is usually the
/// shared, world-writable `/tmp`: a predictable name there can be pre-created as a
/// symlink by another local user, and a truncating open would then follow it into
/// one of our own files. One owner-only (`0700`) directory removes the whole class
/// — nobody else can create, replace, or read anything inside it — and keeps the
/// scratch files out of the way of whatever else is in `/tmp`.
///
/// The uid in the name only separates users who share a `/tmp`; it is not what
/// makes this safe on its own, since anyone can create `delryn-1000` before user
/// 1000 does. [`owned_private_dir`] is the actual check — we use the directory
/// only once we've confirmed we own it and nobody else can write to it.
///
/// Falls back to the bare temp dir if the directory can't be created or fails
/// that check, so logging degrades rather than disappearing.
pub fn runtime_dir() -> PathBuf {
    let base = std::env::temp_dir();
    #[cfg(unix)]
    let dir = {
        // SAFETY: `getuid` reads the calling process's own real user id. It takes
        // no arguments, touches no memory, and cannot fail.
        let uid = unsafe { libc::getuid() };
        base.join(format!("delryn-{uid}"))
    };
    #[cfg(not(unix))]
    let dir = base.join("delryn");
    if create_private_dir(&dir).is_ok() && owned_private_dir(&dir) {
        dir
    } else {
        base
    }
}

/// Is `dir` a real directory (not a symlink), owned by us, and writable by
/// nobody else? The guarantee [`runtime_dir`] rests on: a scratch directory
/// somebody else planted first fails this and is not used.
#[cfg(unix)]
fn owned_private_dir(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // `symlink_metadata` deliberately does not follow links, so a symlink
    // pointing at a directory we *do* own is still rejected.
    let Ok(md) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    // SAFETY: `getuid` reads our own real user id; no arguments, cannot fail.
    let uid = unsafe { libc::getuid() };
    md.is_dir() && md.uid() == uid && md.mode() & 0o077 == 0
}

#[cfg(not(unix))]
fn owned_private_dir(_dir: &Path) -> bool {
    true
}

/// Permissions for anything delryn writes into its data directory: owner-only.
/// The store holds reading history, notes and highlights, and the config holds
/// the library paths — none of it is other users' business on a shared machine.
#[cfg(unix)]
pub const PRIVATE_MODE: u32 = 0o600;
/// Owner-only for the data directory itself (`rwx`, so it stays traversable).
#[cfg(unix)]
const PRIVATE_DIR_MODE: u32 = 0o700;

/// Create `dir` (and parents) and, on Unix, tighten it to owner-only.
///
/// Best-effort on the permission step: a pre-existing directory the user has
/// deliberately opened up is left alone rather than fought with, and a
/// filesystem with no Unix modes (a FAT stick, a network mount) simply keeps
/// whatever it gives us. Directory *creation* failing is a real error and is
/// propagated, since the caller is about to write into it.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    let existed = dir.exists();
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    if !existed {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(PRIVATE_DIR_MODE));
    }
    let _ = existed;
    Ok(())
}

/// Write `contents` to `path` **atomically and privately**.
///
/// A truncate-in-place write (`fs::write`) is not crash-safe: a kill or power
/// loss part-way through leaves a half-written file, and for the config that
/// meant every setting and the whole library source list silently reverting to
/// defaults on the next launch. So this writes a sibling temp file, flushes it
/// to disk, and renames it over the target — `rename(2)` is atomic within a
/// filesystem, so a reader sees either the old file or the new one, never a
/// torn one. The same shape `epub_write::embed_cover` already uses for books.
///
/// The temp name carries the process id so two instances never share one, and
/// on Unix the file is created `0600` *before* any bytes reach it, so the
/// contents are never briefly world-readable. A failed write removes the temp
/// and leaves the original untouched.
pub fn write_private_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    create_private_dir(dir)?;

    let stem = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{stem}.tmp-{}", std::process::id()));

    let write = || -> io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(PRIVATE_MODE);
        }
        let mut f = opts.open(&tmp)?;
        io::Write::write_all(&mut f, contents)?;
        // Without the flush the rename can land before the data does, so a power
        // loss right here would leave a present-but-empty file — exactly the
        // outcome the temp-then-rename is meant to rule out.
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };

    write().inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("delryn_paths_{tag}_{}", std::process::id()))
    }

    #[test]
    fn atomic_write_replaces_contents_and_is_owner_only() {
        let dir = tmp_dir("atomic");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");

        write_private_atomic(&path, b"first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");

        // Overwriting an existing file replaces it wholesale (no leftover tail).
        write_private_atomic(&path, b"second-and-longer").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second-and-longer");
        write_private_atomic(&path, b"3").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "3");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, PRIVATE_MODE, "written owner-only");
            let dmode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dmode, PRIVATE_DIR_MODE, "data dir owner-only");
        }

        // No temp file is left behind on the happy path.
        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(strays.is_empty(), "temp files cleaned up: {strays:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The scratch dir is only used once it's ours and ours alone — a directory
    /// another local user could write to is rejected, and we fall back rather
    /// than dropping our logs into it.
    #[test]
    #[cfg(unix)]
    fn runtime_dir_is_private_and_rejects_a_world_writable_one() {
        use std::os::unix::fs::PermissionsExt;

        let dir = runtime_dir();
        assert!(dir.is_dir());
        assert!(
            owned_private_dir(&dir),
            "a fresh runtime dir passes the check"
        );
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, PRIVATE_DIR_MODE);

        // Open it up the way a hostile pre-created directory would be, and the
        // check must refuse it.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            !owned_private_dir(&dir),
            "a group/world-writable scratch dir is not trusted"
        );
        assert_eq!(
            runtime_dir(),
            std::env::temp_dir(),
            "and runtime_dir falls back instead of using it"
        );

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(PRIVATE_DIR_MODE)).unwrap();
    }

    #[test]
    fn atomic_write_creates_missing_parents() {
        let dir = tmp_dir("nested");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("a").join("b").join("config.toml");
        write_private_atomic(&path, b"x").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
