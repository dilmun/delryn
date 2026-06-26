//! Duplicate-resolution overlay: every duplicate group with a checkbox per copy.
//! A smart auto-select keeps the best copy of each group and marks the rest for
//! deletion (original beats converted, EPUB beats other formats, richer metadata
//! and any copy you've read/rated/favourited win, larger breaks ties). The reader
//! adjusts the checkboxes manually, then deletes all checked at once.

use crossterm::event::{KeyCode, KeyEvent};

use super::App;
use super::confirm::ConfirmAction;

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
    /// read/rated/favourited is never auto-deleted; then original > converted,
    /// format, metadata richness, and finally size as a tiebreak.
    fn keep_score(&self) -> i64 {
        let fmt = match self.format.as_str() {
            "EPUB" => 3,
            "AZW3" | "MOBI" => 2,
            "PDF" => 1,
            _ => 0,
        };
        i64::from(self.favorite) * 100_000
            + i64::from(self.rating) * 10_000
            + i64::from(self.pct) * 100
            + i64::from(!self.converted) * 5_000
            + fmt * 1_000
            + i64::from(self.meta) * 200
            + (self.size / 50_000) as i64
    }
}

/// A duplicate group (≥2 copies sharing an ISBN, else title+author).
pub struct DupGroup {
    pub label: String,
    pub members: Vec<DupMember>,
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

    /// Total copies marked for deletion across all groups.
    pub fn checked_count(&self) -> usize {
        self.groups
            .iter()
            .flat_map(|g| &g.members)
            .filter(|m| m.checked)
            .count()
    }
}

impl App {
    /// Build and open the duplicate-resolution overlay from the whole library,
    /// pre-applying the smart auto-select. No-op (with a flash) when nothing is
    /// duplicated.
    pub(crate) fn open_dup_resolve(&mut self) {
        let Some(store) = &self.store else {
            return;
        };
        let all = store.all_books();
        let groups: Vec<DupGroup> = crate::library::dedup::duplicate_groups(&all)
            .into_iter()
            .map(|idxs| {
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
                DupGroup { label, members }
            })
            .collect();
        if groups.is_empty() {
            self.lib_flash = Some("no duplicates found".into());
            return;
        }
        let mut dr = DupResolve { groups, cursor: 0 };
        auto_select(&mut dr);
        self.dup_resolve = Some(dr);
    }

    /// Keys while the overlay is open.
    pub(crate) fn dup_resolve_key(&mut self, key: KeyEvent) {
        let Some(dr) = self.dup_resolve.as_mut() else {
            return;
        };
        let rows = dr.rows();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.dup_resolve = None,
            KeyCode::Char('j') | KeyCode::Down => {
                dr.cursor = (dr.cursor + 1).min(rows.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => dr.cursor = dr.cursor.saturating_sub(1),
            KeyCode::Char(' ') => {
                if let Some(&(gi, mi)) = rows.get(dr.cursor) {
                    let m = &mut dr.groups[gi].members[mi];
                    m.checked = !m.checked;
                }
            }
            KeyCode::Char('a') => auto_select(dr),
            KeyCode::Char('u') => {
                for g in &mut dr.groups {
                    for m in &mut g.members {
                        m.checked = false;
                    }
                }
            }
            KeyCode::Char('d') | KeyCode::Enter => {
                let paths: Vec<String> = dr
                    .groups
                    .iter()
                    .flat_map(|g| &g.members)
                    .filter(|m| m.checked)
                    .map(|m| m.path.clone())
                    .collect();
                if paths.is_empty() {
                    self.lib_flash = Some("nothing checked to delete".into());
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

    /// After the confirmed deletion, rebuild the overlay so it reflects what's
    /// left (closing it when no duplicates remain).
    pub(crate) fn refresh_dup_resolve(&mut self) {
        if self.dup_resolve.is_none() {
            return;
        }
        self.dup_resolve = None;
        self.open_dup_resolve();
    }
}

/// Keep the highest-scoring copy of each group; check the rest for deletion.
fn auto_select(dr: &mut DupResolve) {
    for g in &mut dr.groups {
        let best = g
            .members
            .iter()
            .enumerate()
            .max_by_key(|(_, m)| m.keep_score())
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
