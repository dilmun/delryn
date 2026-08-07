//! Finding library folders for you: a background walk of the home directory
//! (`delryn_library::discover`) and the picker that turns the folders you tick
//! into library sources.
//!
//! The walk reads thousands of directories, so it runs on a worker thread and
//! reports once — the UI stays live throughout, like every other scan here. The
//! result is a *proposal*: nothing is added to the library until the picker is
//! confirmed, because the walk is a heuristic over someone's whole home
//! directory. Sibling of `scan` (which does the indexing afterwards) and
//! `dup_scan`.

use std::sync::mpsc::{Receiver, TryRecvError};

use crossterm::event::{KeyCode, KeyEvent};
use delryn_library::discover::Found;

use super::{App, Overlay};

/// An in-flight folder search: the worker's one-shot result channel.
pub struct DiscoverJob {
    rx: Receiver<Vec<Found>>,
}

/// The proposal list once the search finishes: each folder with whether it's
/// ticked for adding. Everything starts ticked — the answer is usually "yes, all
/// of them", and unticking the odd one is cheaper than ticking the rest.
pub struct FolderFinder {
    /// Proposed folders, fullest first, each with its tick state.
    pub found: Vec<(Found, bool)>,
    /// Focused row.
    pub sel: usize,
}

impl FolderFinder {
    /// How many folders are ticked (drives the confirm label and the legend).
    pub fn picked(&self) -> usize {
        self.found.iter().filter(|(_, on)| *on).count()
    }
}

impl App {
    /// Search the home directory for folders holding books, on a worker thread.
    ///
    /// Closes whatever overlay launched it (the Sources tab, the palette) so the
    /// progress message is visible and the picker has somewhere to open into.
    /// No-op while a search is already running.
    pub(crate) fn start_discover(&mut self) {
        if self.discover.is_some() {
            return;
        }
        let Some(home) = delryn_library::discover::home() else {
            self.library.flash = Some("No home directory to search".into());
            return;
        };
        // Configured folders are skipped by the walk, so a second run proposes
        // only what's new rather than re-listing the whole library.
        let existing = self.config.library_paths.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // A send error just means the app closed mid-search; nothing to do.
            let _ = tx.send(delryn_library::discover::find_book_folders(
                &home, &existing,
            ));
        });
        self.overlay = Overlay::None;
        self.library.flash = Some("Looking for book folders…".into());
        self.discover = Some(DiscoverJob { rx });
    }

    /// True while the folder search is running — keeps the main loop polling.
    pub fn discover_pending(&self) -> bool {
        self.discover.is_some()
    }

    /// Pick up a finished search: open the picker, or report that there was
    /// nothing new to propose. Returns `true` when it completes (request a redraw).
    pub fn poll_discover(&mut self) -> bool {
        let Some(job) = self.discover.as_ref() else {
            return false;
        };
        match job.rx.try_recv() {
            Ok(found) => {
                self.discover = None;
                if found.is_empty() {
                    self.library.flash =
                        Some("No book folders found — add one by hand in ; ▸ Sources".into());
                } else {
                    self.overlay = Overlay::FolderFinder(FolderFinder {
                        found: found.into_iter().map(|f| (f, true)).collect(),
                        sel: 0,
                    });
                }
                true
            }
            Err(TryRecvError::Empty) => false,
            // The worker vanished without sending: drop the job so the UI stops
            // waiting on a result that will never arrive.
            Err(TryRecvError::Disconnected) => {
                self.discover = None;
                true
            }
        }
    }

    /// Keys while the folder picker is open.
    pub(crate) fn finder_key(&mut self, key: KeyEvent) {
        let Overlay::FolderFinder(p) = &mut self.overlay else {
            return;
        };
        // The shared vim motions (j/k · arrows · Ctrl-n/p · Ctrl-d/u · g/G).
        if let Some(ns) = crate::input::list_nav(key, p.sel, p.found.len(), 10) {
            p.sel = ns;
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
            KeyCode::Char(' ') => self.finder_toggle(),
            // `a` ticks everything, or clears when everything is already ticked.
            KeyCode::Char('a') => {
                let on = p.picked() < p.found.len();
                for (_, ticked) in &mut p.found {
                    *ticked = on;
                }
            }
            KeyCode::Enter => self.finder_commit(),
            _ => {}
        }
    }

    /// Tick / untick the focused folder (Space, or a click on its row).
    pub(crate) fn finder_toggle(&mut self) {
        let Overlay::FolderFinder(p) = &mut self.overlay else {
            return;
        };
        if let Some((_, on)) = p.found.get_mut(p.sel) {
            *on = !*on;
        }
    }

    /// Register every ticked folder as a library source, then index them in the
    /// background. Paths go through [`normalize_root`](delryn_library::normalize_root)
    /// so they dedupe against folders added by hand or from the CLI.
    pub(crate) fn finder_commit(&mut self) {
        let Overlay::FolderFinder(p) = &self.overlay else {
            return;
        };
        let picked: Vec<String> = p
            .found
            .iter()
            .filter(|(_, on)| *on)
            .map(|(f, _)| f.path.clone())
            .collect();
        self.overlay = Overlay::None;
        if picked.is_empty() {
            self.library.flash = Some("No folders ticked — nothing added".into());
            return;
        }
        let mut added = 0;
        for path in picked {
            let root = crate::library::normalize_root(&path);
            if !self.config.library_paths.contains(&root) {
                self.config.library_paths.push(root);
                added += 1;
            }
        }
        if added == 0 {
            self.library.flash = Some("Those folders are already library sources".into());
            return;
        }
        self.config.save();
        // Indexing runs in the background so a big collection doesn't block; the
        // completion flash reports the book count (see `App::poll_scan`).
        self.start_scan(false, false, format!("Added {added} folder(s)"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private config dir + workspace for one test, so `config.save()` can
    /// never reach the real `~/.config`. Caller holds the guard that serializes
    /// the `XDG_CONFIG_HOME` mutation.
    fn sandbox(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("delryn_finder_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: serialized by the caller's `test_env_guard`; scopes the config
        // dir to this test.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
        dir
    }

    fn finder(paths: &[(&str, usize)]) -> FolderFinder {
        FolderFinder {
            found: paths
                .iter()
                .map(|(p, books)| {
                    (
                        Found {
                            path: (*p).to_string(),
                            books: *books,
                        },
                        true,
                    )
                })
                .collect(),
            sel: 0,
        }
    }

    /// Only ticked folders are registered, and an untick really excludes one.
    #[test]
    fn committing_adds_only_the_ticked_folders() {
        let _env = crate::test_env_guard();
        let dir = sandbox("ticked");
        let mut app = App::library();
        let keep = dir.join("keep").to_string_lossy().into_owned();
        let drop = dir.join("drop").to_string_lossy().into_owned();
        std::fs::create_dir_all(&keep).unwrap();
        std::fs::create_dir_all(&drop).unwrap();

        let mut f = finder(&[(&keep, 5), (&drop, 3)]);
        f.found[1].1 = false;
        app.overlay = Overlay::FolderFinder(f);
        app.finder_commit();

        assert_eq!(
            app.config.library_paths,
            vec![crate::library::normalize_root(&keep)],
            "the unticked folder stayed out"
        );
        app.await_scan();

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Committing with nothing ticked must not register anything — the escape
    /// hatch for "none of these, thanks".
    #[test]
    fn committing_with_nothing_ticked_adds_nothing() {
        let _env = crate::test_env_guard();
        let dir = sandbox("none");
        let mut app = App::library();
        let mut f = finder(&[("/nowhere", 4)]);
        f.found[0].1 = false;
        app.overlay = Overlay::FolderFinder(f);
        app.finder_commit();

        assert!(app.config.library_paths.is_empty());
        assert!(matches!(app.overlay, Overlay::None), "the picker closed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `a` clears the lot when everything is ticked, and re-ticks it when not.
    #[test]
    fn a_toggles_every_row_at_once() {
        let _env = crate::test_env_guard();
        let dir = sandbox("toggle");
        let mut app = App::library();
        app.overlay = Overlay::FolderFinder(finder(&[("/a", 4), ("/b", 9)]));
        let press = |app: &mut App, c: char| {
            app.finder_key(KeyEvent::from(KeyCode::Char(c)));
        };

        press(&mut app, 'a');
        let Overlay::FolderFinder(p) = &app.overlay else {
            panic!("picker still open")
        };
        assert_eq!(p.picked(), 0, "all ticked ⇒ `a` clears");

        press(&mut app, 'a');
        let Overlay::FolderFinder(p) = &app.overlay else {
            panic!("picker still open")
        };
        assert_eq!(p.picked(), 2, "none ticked ⇒ `a` ticks everything");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
