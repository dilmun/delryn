//! Renaming books on disk: the shared single-file rename mechanism and the
//! bulk-rename popup (one template applied to the marked books, with a preview).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use delryn_model::naming::{filename_title, fill_template, sanitize_filename};

use super::confirm::ConfirmAction;
use super::{App, fmt_series_index, str_delete_before, str_insert};
use crate::online;

/// Default rename template: `Title.ext` (subtitle stripped — see `filename_title`).
pub const DEFAULT_RENAME_TEMPLATE: &str = "%T.%E";

/// One book queued for a bulk rename: its path, the metadata values used to fill
/// the template (in `META_FIELDS` order), its extension, and its current name
/// (for the preview).
pub struct BulkTarget {
    pub path: String,
    pub values: Vec<String>,
    pub ext: String,
    pub old_name: String,
}

/// Bulk-rename popup: one editable template applied to every marked book.
pub struct BulkRename {
    pub template: String,
    pub cursor: usize,
    pub targets: Vec<BulkTarget>,
    /// Expand the popup to (near) full screen for a wider before/after view.
    pub full: bool,
}

/// Outcome of renaming a single book file.
enum RenameOutcome {
    Renamed,
    Unchanged,
    /// Skipped — name empty, target clashes, or the move failed.
    Skipped,
}

impl App {
    /// Move one book file to `new_name` (in its own directory), repointing the
    /// database row and cached cover. Pure mechanism — no UI/state side effects
    /// beyond persistence, so both the editor and bulk rename share it.
    fn rename_book_file(&self, old: &str, new_name: &str) -> RenameOutcome {
        let name = sanitize_filename(new_name.trim());
        if name.is_empty() {
            return RenameOutcome::Skipped;
        }
        let old_path = std::path::Path::new(old);
        let new_path = match old_path.parent() {
            Some(dir) => dir.join(&name),
            None => std::path::PathBuf::from(&name),
        };
        let new = new_path.to_string_lossy().into_owned();
        if new == old {
            return RenameOutcome::Unchanged;
        }
        if new_path.exists() {
            return RenameOutcome::Skipped;
        }
        if std::fs::rename(old, &new_path).is_err() {
            return RenameOutcome::Skipped;
        }
        // Repoint persistence + move the cached cover to the new key.
        if let Some(store) = &self.store {
            store.rename_book_path(old, &new);
        }
        let _ = std::fs::rename(
            online::cover_cache_path(old),
            online::cover_cache_path(&new),
        );
        RenameOutcome::Renamed
    }

    /// Open the rename popup over the marked books, or — when nothing is marked —
    /// the current book. Snapshots the data the template needs from each.
    pub(crate) fn open_bulk_rename(&mut self) {
        let current = self.lib_books.get(self.lib_sel).map(|b| b.path.clone());
        let targets: Vec<BulkTarget> = self
            .lib_books
            .iter()
            .filter(|b| {
                if self.lib_marked.is_empty() {
                    Some(&b.path) == current.as_ref()
                } else {
                    self.lib_marked.contains(&b.path)
                }
            })
            .map(|b| {
                let ext = std::path::Path::new(&b.path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("epub")
                    .to_string();
                let old_name = std::path::Path::new(&b.path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                BulkTarget {
                    path: b.path.clone(),
                    values: vec![
                        // %T is the main title with any subtitle stripped, so the
                        // default "%T.%E" never bakes the subtitle into the file.
                        filename_title(&b.title, &b.subtitle),
                        b.author.clone(),
                        b.year.map(|y| y.to_string()).unwrap_or_default(),
                        b.series.clone(),
                        b.series_index.map(fmt_series_index).unwrap_or_default(),
                        b.publisher.clone(),
                    ],
                    ext,
                    old_name,
                }
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        let template = DEFAULT_RENAME_TEMPLATE.to_string();
        self.bulk_rename = Some(BulkRename {
            cursor: template.chars().count(),
            template,
            targets,
            full: false,
        });
    }

    /// Apply the bulk-rename template to every target, then close + report.
    pub(crate) fn apply_bulk_rename(&mut self) {
        let Some(br) = self.bulk_rename.take() else {
            return;
        };
        let mut renamed = 0usize;
        let mut skipped = 0usize;
        for t in &br.targets {
            let new_name = fill_template(&br.template, &t.values, &t.ext);
            match self.rename_book_file(&t.path, &new_name) {
                RenameOutcome::Renamed => renamed += 1,
                RenameOutcome::Unchanged => {}
                RenameOutcome::Skipped => skipped += 1,
            }
        }
        self.lib_exit_visual();
        self.refresh_library();
        self.lib_flash = Some(if skipped == 0 {
            format!(
                "renamed {renamed} book{}",
                if renamed == 1 { "" } else { "s" }
            )
        } else {
            format!("renamed {renamed}, skipped {skipped}")
        });
    }

    /// Bulk-rename popup keys: type to edit the template, ^S apply, Esc cancel.
    pub(crate) fn bulk_rename_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.bulk_rename = None,
            // ^S asks for confirmation before moving files on disk.
            KeyCode::Char('s') if ctrl => {
                let n = self.bulk_rename.as_ref().map_or(0, |b| b.targets.len());
                if n > 0 {
                    let q = format!("Rename {n} book{}?", if n == 1 { "" } else { "s" });
                    self.ask_confirm(&q, ConfirmAction::Rename);
                }
            }
            // Toggle the full-screen before/after view.
            KeyCode::Char('f') if ctrl => {
                if let Some(b) = self.bulk_rename.as_mut() {
                    b.full = !b.full;
                }
            }
            KeyCode::Left => {
                if let Some(b) = self.bulk_rename.as_mut() {
                    b.cursor = b.cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Some(b) = self.bulk_rename.as_mut() {
                    b.cursor = (b.cursor + 1).min(b.template.chars().count());
                }
            }
            KeyCode::Char('u') if ctrl => {
                if let Some(b) = self.bulk_rename.as_mut() {
                    b.template.clear();
                    b.cursor = 0;
                }
            }
            KeyCode::Backspace => {
                if let Some(b) = self.bulk_rename.as_mut() {
                    let cur = b.cursor;
                    if str_delete_before(&mut b.template, cur) {
                        b.cursor -= 1;
                    }
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(b) = self.bulk_rename.as_mut() {
                    let cur = b.cursor;
                    str_insert(&mut b.template, cur, c);
                    b.cursor += 1;
                }
            }
            _ => {}
        }
    }
}
