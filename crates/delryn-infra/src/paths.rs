//! Where delryn keeps its data — the single config/data directory shared by the
//! config file, the SQLite store, and the cover cache.

use std::path::PathBuf;

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
