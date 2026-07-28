//! Duplicate-resolution overlay: every duplicate group with a checkbox per copy.
//! A smart auto-select keeps the best copy of each group and marks the rest for
//! deletion — any copy you've read/rated/favourited wins, then originals over
//! converted and the configured format keep-order, then metadata richness and size.
//! The format priority and a "converted: always delete" rule are configurable
//! (Library Settings → Duplicates, reachable via `o`). The reader adjusts the
//! checkboxes manually (`f` resizes the window, like every overlay), then deletes
//! all checked.
//! `n` *ignores* a group (stop flagging it); the ignored-groups manager (`I`)
//! lists them to restore or clear.

use crossterm::event::{KeyCode, KeyEvent};

use super::confirm::ConfirmAction;
use super::{App, Overlay};
use crate::config::Config;

/// One copy within a duplicate group.
pub struct DupMember {
    pub path: String,
    /// File name (basename), to tell same-titled copies apart.
    pub file: String,
    pub format: String,
    pub size: u64,
    pub converted: bool,
    pub pct: u8,
    pub favorite: bool,
    pub rating: u8,
    /// Count of present metadata fields (isbn/publisher/series/year), 0–4.
    pub meta: u8,
    /// Marked for deletion.
    pub checked: bool,
}

impl DupMember {
    /// Keep-priority: higher wins (is kept). Engagement dominates so a copy you've
    /// read/rated/favourited is never auto-deleted; then original > converted, the
    /// configured format preference, metadata richness, and finally size as a
    /// tiebreak. Format ranking comes from `config.dup_format_order`.
    fn keep_score(&self, config: &Config) -> i64 {
        // Earlier in the keep-order → higher score; an unknown format → 0.
        let order = &config.dup_format_order;
        let fmt = order
            .len()
            .saturating_sub(config.dup_format_rank(&self.format)) as i64;
        i64::from(self.favorite) * 100_000
            + i64::from(self.rating) * 10_000
            + i64::from(self.pct) * 100
            + i64::from(!self.converted) * 5_000
            + fmt * 1_000
            + i64::from(self.meta) * 200
            + (self.size / 50_000) as i64
    }
}

/// A duplicate group (≥2 copies linked by a shared ISBN or title+author).
pub struct DupGroup {
    pub label: String,
    pub members: Vec<DupMember>,
    /// Stable identity of the group (sorted member paths), used to remember it as
    /// dismissed when the reader chooses to keep every copy.
    pub signature: String,
}

/// The open resolution overlay.
pub struct DupResolve {
    pub groups: Vec<DupGroup>,
    /// Flat cursor over member rows (group headers aren't selectable).
    pub cursor: usize,
}

impl DupResolve {
    /// `(group, member)` pairs in display order — the selectable rows.
    pub fn rows(&self) -> Vec<(usize, usize)> {
        self.groups
            .iter()
            .enumerate()
            .flat_map(|(gi, g)| (0..g.members.len()).map(move |mi| (gi, mi)))
            .collect()
    }

    /// Path of the copy under the cursor, if any.
    pub fn selected_path(&self) -> Option<&str> {
        let (gi, mi) = self.rows().get(self.cursor).copied()?;
        Some(self.groups[gi].members[mi].path.as_str())
    }

    /// Total copies marked for deletion across all groups.
    pub fn checked_count(&self) -> usize {
        self.groups
            .iter()
            .flat_map(|g| &g.members)
            .filter(|m| m.checked)
            .count()
    }
}

/// Manager for the groups the reader has *ignored* (pressed `n` on) — so they can
/// be reviewed, restored individually, or cleared. Each entry is one ignored
/// group's member paths (decoded from its stored signature).
pub struct IgnoredView {
    /// Member paths per ignored group, parallel to `signatures`.
    pub groups: Vec<Vec<String>>,
    /// The stored signatures, parallel to `groups` (the key to restore each).
    pub signatures: Vec<String>,
    pub cursor: usize,
}

impl App {
    /// Build and open the duplicate-resolution overlay from the whole library,
    /// pre-applying the smart auto-select. No-op (with a flash) when nothing is
    /// duplicated.
    pub(crate) fn open_dup_resolve(&mut self) {
        // Every exit from here reports something. Returning silently made `D` look
        // like an unbound key rather than "there is nothing to resolve".
        let Some(store) = &self.session.store else {
            self.library.flash = Some("library database unavailable".into());
            return;
        };
        let all = store.all_books();
        // Cover-scan links fold in metadata-less matches; dismissed groups ("keep
        // both") shouldn't be offered again.
        let dismissed = store.dismissed_duplicate_groups();
        let links = store.dup_links();
        let groups: Vec<DupGroup> =
            crate::library::dedup::duplicate_groups_with_links(&all, &links)
                .into_iter()
                .filter(|idxs| {
                    !dismissed.contains(&crate::library::dedup::group_signature(idxs, &all))
                })
                .map(|idxs| {
                    let signature = crate::library::dedup::group_signature(&idxs, &all);
                    let members = idxs
                        .iter()
                        .map(|&i| {
                            let b = &all[i];
                            DupMember {
                                path: b.path.clone(),
                                file: file_name(&b.path),
                                format: crate::document::BookFormat::from_path(&b.path)
                                    .label()
                                    .to_string(),
                                size: b.size,
                                converted: b.converted,
                                pct: b.pct,
                                favorite: b.favorite,
                                rating: b.rating,
                                meta: [
                                    !b.isbn.is_empty(),
                                    !b.publisher.is_empty(),
                                    !b.series.is_empty(),
                                    b.year.is_some(),
                                ]
                                .into_iter()
                                .filter(|x| *x)
                                .count() as u8,
                                checked: false,
                            }
                        })
                        .collect();
                    let label = all[idxs[0]].title.clone();
                    DupGroup {
                        label,
                        members,
                        signature,
                    }
                })
                .collect();
        if groups.is_empty() {
            // Name the deeper scan, so "nothing here" doesn't read as "broken":
            // the metadata pass misses PDFs that only a cover scan (`R`) matches.
            self.library.flash =
                Some("no duplicates found — press R in Duplicates for a cover scan".into());
            return;
        }
        let mut dr = DupResolve { groups, cursor: 0 };
        auto_select(&mut dr, &self.config);
        self.overlay = Overlay::DupResolve(dr);
    }

    /// Keys while the overlay is open.
    pub(crate) fn dup_resolve_key(&mut self, key: KeyEvent) {
        // Standard vim list navigation over the group rows (j/k · Ctrl-d/u · g/G).
        if let Overlay::DupResolve(dr) = &mut self.overlay
            && let Some(ns) = crate::input::list_nav(key, dr.cursor, dr.rows().len(), 10)
        {
            dr.cursor = ns;
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
            // Re-run the (config-driven) auto-select.
            KeyCode::Char('a') => self.auto_select_dups(),
            // (`f` resizes the window — handled centrally in `on_key`.)
            // Open this overlay's preferences (Library Settings → Duplicates).
            KeyCode::Char('o') => self.open_dup_settings(),
            // Preview: open the selected copy in the reader; q/Esc returns here.
            KeyCode::Char('p') => self.preview_dup_book(),
            // Reveal the selected copy in the OS file manager.
            KeyCode::Char('r') => {
                if let Overlay::DupResolve(dr) = &self.overlay
                    && let Some(path) = dr.selected_path()
                {
                    reveal_in_file_manager(path);
                }
            }
            KeyCode::Char(' ') => self.dup_toggle_checked(),
            KeyCode::Char('u') => {
                if let Overlay::DupResolve(dr) = &mut self.overlay {
                    for m in dr.groups.iter_mut().flat_map(|g| &mut g.members) {
                        m.checked = false;
                    }
                }
            }
            // Open the ignored-groups manager (restore / clear).
            KeyCode::Char('I') => self.open_ignored_view(),
            KeyCode::Char('n') => {
                // "Ignore": stop flagging the group under the cursor as a duplicate,
                // then rebuild to drop it from the overlay. Reversible via the
                // ignored-groups manager (`I`).
                let sig = if let Overlay::DupResolve(dr) = &self.overlay {
                    dr.rows()
                        .get(dr.cursor)
                        .copied()
                        .map(|(gi, _)| dr.groups[gi].signature.clone())
                } else {
                    None
                };
                if let Some(sig) = sig {
                    if let Some(store) = &self.session.store {
                        store.dismiss_duplicate_group(&sig);
                    }
                    self.library.flash =
                        Some("ignored — won't be flagged again (I to undo)".into());
                    self.refresh_dup_resolve();
                }
            }
            KeyCode::Char('d') => {
                let paths: Vec<String> = if let Overlay::DupResolve(dr) = &self.overlay {
                    dr.groups
                        .iter()
                        .flat_map(|g| &g.members)
                        .filter(|m| m.checked)
                        .map(|m| m.path.clone())
                        .collect()
                } else {
                    Vec::new()
                };
                if paths.is_empty() {
                    self.library.flash = Some("nothing checked to delete".into());
                    return;
                }
                let q = format!(
                    "Delete {} duplicate file(s)? This cannot be undone.",
                    paths.len()
                );
                self.ask_confirm(&q, ConfirmAction::ResolveDuplicates(paths));
            }
            _ => {}
        }
    }

    /// Toggle the keep/delete check on the copy under the cursor (Space / double-
    /// click).
    pub(crate) fn dup_toggle_checked(&mut self) {
        if let Overlay::DupResolve(dr) = &mut self.overlay
            && let Some(&(gi, mi)) = dr.rows().get(dr.cursor)
        {
            let m = &mut dr.groups[gi].members[mi];
            m.checked = !m.checked;
        }
    }

    /// Re-apply the smart auto-select to the open overlay using the current prefs.
    fn auto_select_dups(&mut self) {
        let config = &self.config;
        if let Overlay::DupResolve(dr) = &mut self.overlay {
            auto_select(dr, config);
        }
    }

    /// Open the ignored-groups manager. Closes the resolver if it was open. Flashes
    /// (and doesn't open) when nothing has been ignored.
    pub(crate) fn open_ignored_view(&mut self) {
        let Some(view) = self.build_ignored_view() else {
            self.library.flash = Some("no ignored duplicate groups".into());
            return;
        };
        self.overlay = Overlay::None;
        self.overlay = Overlay::IgnoredView(view);
    }

    /// Build the ignored-groups list from the store, or `None` if empty.
    fn build_ignored_view(&self) -> Option<IgnoredView> {
        let store = self.session.store.as_ref()?;
        let mut signatures: Vec<String> = store.dismissed_duplicate_groups().into_iter().collect();
        signatures.sort();
        if signatures.is_empty() {
            return None;
        }
        let groups = signatures
            .iter()
            .map(|s| s.split('\n').map(String::from).collect())
            .collect();
        Some(IgnoredView {
            groups,
            signatures,
            cursor: 0,
        })
    }

    /// Keys while the ignored-groups manager is open.
    pub(crate) fn ignored_view_key(&mut self, key: KeyEvent) {
        // Standard vim list navigation (Ctrl-u for half-page; plain `u` restores).
        if let Overlay::IgnoredView(v) = &mut self.overlay
            && let Some(ns) = crate::input::list_nav(key, v.cursor, v.signatures.len(), 10)
        {
            v.cursor = ns;
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
            // Restore (un-ignore) the selected group.
            KeyCode::Char('u') | KeyCode::Enter => self.ignored_restore_selected(),
            // Restore all.
            KeyCode::Char('C') => self.ignored_restore_all(),
            _ => {}
        }
    }

    /// Restore (un-ignore) the group under the cursor, rebuild the list keeping the
    /// cursor near where it was, and close + flash when nothing's left (u / Enter /
    /// double-click).
    pub(crate) fn ignored_restore_selected(&mut self) {
        let sig = if let Overlay::IgnoredView(v) = &self.overlay {
            v.signatures.get(v.cursor).cloned()
        } else {
            None
        };
        let Some(sig) = sig else {
            return;
        };
        if let Some(store) = &self.session.store {
            store.restore_duplicate_group(&sig);
        }
        self.refresh_library();
        let cursor = if let Overlay::IgnoredView(v) = &self.overlay {
            v.cursor
        } else {
            0
        };
        match self.build_ignored_view() {
            Some(mut v) => {
                v.cursor = cursor.min(v.signatures.len() - 1);
                self.overlay = Overlay::IgnoredView(v);
            }
            None => {
                self.overlay = Overlay::None;
                self.library.flash = Some("restored — no ignored groups left".into());
            }
        }
    }

    /// Restore every ignored group and close the manager (`C`).
    fn ignored_restore_all(&mut self) {
        if let Some(store) = &self.session.store {
            store.clear_dismissed_duplicates();
        }
        self.overlay = Overlay::None;
        self.library.flash = Some("restored all ignored groups".into());
        self.refresh_library();
    }

    /// Open the copy under the cursor in the reader for a quick look, stashing the
    /// overlay so the dispatcher/renderer ignore it; `q`/Esc restores it (see the
    /// reader-return handling in `dispatch`). Mirrors `open_selected`. A flash
    /// surfaces the reason when a book can't be opened (e.g. an unreadable format).
    fn preview_dup_book(&mut self) {
        let Overlay::DupResolve(dr) = &self.overlay else {
            return;
        };
        let Some(path) = dr.selected_path().map(str::to_string) else {
            return;
        };
        self.flush_reading_time();
        match super::build_reader(&path, &self.session.store, self.picker.is_some()) {
            Ok((reader, config, book_path)) => {
                self.reader = Some(reader);
                self.config = config;
                self.session.book_path = book_path;
                self.mode = super::Mode::Reader;
                self.session.started = Some(std::time::Instant::now());
                if let Some(s) = &self.session.store {
                    s.mark_opened(&self.session.book_path);
                }
                self.dup_preview = if let Overlay::DupResolve(dr) =
                    std::mem::replace(&mut self.overlay, Overlay::None)
                {
                    Some(dr)
                } else {
                    None
                };
            }
            Err(e) => self.library.flash = Some(e.to_string()),
        }
    }

    /// After the confirmed deletion, rebuild the overlay so it reflects what's
    /// left (closing it when no duplicates remain).
    pub(crate) fn refresh_dup_resolve(&mut self) {
        if !matches!(self.overlay, Overlay::DupResolve(_)) {
            return;
        }
        self.overlay = Overlay::None;
        self.open_dup_resolve();
    }
}

/// Keep the highest-scoring copy of each group; check the rest for deletion. When
/// "converted always delete" is on and a group has an un-converted copy, only
/// un-converted copies are eligible to be kept (every converted one is checked).
fn auto_select(dr: &mut DupResolve, config: &Config) {
    for g in &mut dr.groups {
        let only_originals = config.dup_converted_delete && g.members.iter().any(|m| !m.converted);
        let best = g
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| !only_originals || !m.converted)
            .max_by_key(|(_, m)| m.keep_score(config))
            .map(|(i, _)| i)
            .unwrap_or(0);
        for (i, m) in g.members.iter_mut().enumerate() {
            m.checked = i != best;
        }
    }
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Reveal a file in the OS file manager (Finder on macOS), best-effort — a local
/// reveal, so spawn-and-forget; it doesn't take over the terminal.
fn reveal_in_file_manager(path: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{path}"))
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(dir) = std::path::Path::new(path).parent() {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}
