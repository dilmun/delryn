//! Word-lookup overlay (`K`): dictionary definition + Wikipedia summary for the
//! selected word or phrase. The network call runs on a worker thread and its
//! result is drained by the event loop (mirrors the metadata `poll_online`), so
//! the UI never blocks. Presentation lives in `view::word_lookup`.

use std::thread;

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Overlay};
use crate::online::{self, LookupResult, LookupSources};

/// State of the open word-lookup panel: the term, the fetch state, and the
/// vertical scroll offset (clamped to the content on render).
pub struct WordLookup {
    pub word: String,
    pub state: LookupState,
    pub scroll: u16,
}

/// Whether the lookup is still in flight or its result has arrived.
pub enum LookupState {
    Fetching,
    Ready(Box<LookupResult>),
}

impl App {
    /// Open the lookup panel for `raw` (a selection or the word under the caret)
    /// and kick off the background fetch. A term with no letters/digits flashes
    /// a hint instead of opening an empty panel.
    pub fn open_word_lookup(&mut self, raw: String) {
        let word = clean_term(&raw);
        if word.is_empty() {
            if let Some(r) = self.reader.as_mut() {
                r.flash = Some("Nothing to look up".into());
            }
            return;
        }
        let sources = LookupSources {
            sdcv: self.config.lookup_sdcv,
            dictionary: self.config.lookup_dictionary,
            wikipedia: self.config.lookup_wikipedia,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let query = word.clone();
        thread::spawn(move || {
            let _ = tx.send(online::look_up(&query, sources));
        });
        self.define_rx = Some(rx);
        self.overlay = Overlay::WordLookup(WordLookup {
            word,
            state: LookupState::Fetching,
            scroll: 0,
        });
    }

    /// Drain a finished lookup into the open panel; returns whether the view
    /// changed. Called from the event loop.
    pub fn poll_define(&mut self) -> bool {
        let Some(rx) = &self.define_rx else {
            return false;
        };
        let Ok(result) = rx.try_recv() else {
            return false;
        };
        self.define_rx = None;
        if let Overlay::WordLookup(wl) = &mut self.overlay {
            wl.state = LookupState::Ready(Box::new(result));
        }
        true
    }

    /// Is a lookup in flight (keeps the loop polling so the result pops in)?
    pub fn define_active(&self) -> bool {
        matches!(
            &self.overlay,
            Overlay::WordLookup(wl) if matches!(wl.state, LookupState::Fetching)
        )
    }

    /// Keys while the read-only lookup panel is open: scroll, or close.
    pub fn word_lookup_key(&mut self, key: KeyEvent) {
        // Close on Esc / q / a second `K` (toggle back to reading).
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('K')
        ) {
            self.overlay = Overlay::None;
            return;
        }
        let Overlay::WordLookup(wl) = &mut self.overlay else {
            return;
        };
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => wl.scroll = wl.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => wl.scroll = wl.scroll.saturating_sub(1),
            KeyCode::Char('d') | KeyCode::PageDown => wl.scroll = wl.scroll.saturating_add(10),
            KeyCode::Char('u') | KeyCode::PageUp => wl.scroll = wl.scroll.saturating_sub(10),
            KeyCode::Char('g') | KeyCode::Home => wl.scroll = 0,
            // Overscroll — render clamps to the last page.
            KeyCode::Char('G') | KeyCode::End => wl.scroll = u16::MAX,
            _ => {}
        }
    }
}

/// Trim a raw selection/word down to a lookup term: strip surrounding
/// punctuation and whitespace, keep internal characters (so multi-word phrases
/// and `well-being` survive).
fn clean_term(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::clean_term;

    #[test]
    fn strips_surrounding_punctuation_keeps_internal() {
        assert_eq!(clean_term("  (hello),  "), "hello");
        assert_eq!(clean_term("well-being"), "well-being");
        assert_eq!(clean_term("“don't”"), "don't");
        assert_eq!(clean_term("machine learning."), "machine learning");
        assert_eq!(clean_term("—"), "");
        assert_eq!(clean_term(""), "");
    }
}
