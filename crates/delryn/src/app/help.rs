//! The `?` key reference: every binding that matters, grouped, for whichever
//! surface you're on.
//!
//! The status bar carries a one-line legend, which is enough once you know the
//! app and useless before that — it can only ever show a handful of keys, and it
//! shows the same handful whatever you're trying to do. This is the full list,
//! one keystroke away from anywhere.
//!
//! The rows are static data rather than something derived from the key router.
//! `input::map_key` maps keys to actions; it can't say that `[` and `]` are a
//! pair, that `1`–`5` pick a highlight colour, or which of the forty bindings a
//! reader actually needs first. That ordering is editorial, so it's written down
//! — and a test holds it to the same shape as the README's tables.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, Mode, Overlay};

/// One line of the reference: a group heading, or a binding and what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpRow {
    /// A group heading ("Navigation", "Annotations").
    Heading(&'static str),
    /// `keys` and what pressing them does.
    Key(&'static str, &'static str),
    /// A blank separator line.
    Gap,
}

use HelpRow::{Gap, Heading, Key};

/// Open help: which surface's bindings, and how far it's scrolled.
pub struct Help {
    /// The surface help was opened over — reader or library.
    pub scope: Mode,
    /// First visible row.
    pub scroll: usize,
    /// Rows the popup drew last frame, written by `view::help` (0 before the
    /// first draw). The view owns the popup's geometry, so it's the only thing
    /// that knows how far the list can usefully scroll — see [`Self::max_scroll`].
    pub visible: usize,
}

impl Help {
    /// The furthest the list may scroll: the offset that puts the *last* row at
    /// the bottom of the popup.
    ///
    /// Clamping to `rows.len() - 1` instead — the obvious thing — lets the state
    /// keep climbing after the view has stopped moving, because the view refuses
    /// to scroll into blank space. Holding `j` at the end then banks a silent
    /// debt of keypresses that `k` has to pay off before anything moves again.
    pub fn max_scroll(&self) -> usize {
        rows(self.scope).len().saturating_sub(self.visible.max(1))
    }
}

/// Bindings shown while reading a book.
const READER: &[HelpRow] = &[
    Heading("Navigation"),
    Key("j k  ↓ ↑", "line down / up"),
    Key("Space", "page down"),
    Key("Ctrl-f  Ctrl-b", "page down / up"),
    Key("Ctrl-d  Ctrl-u", "half-page down / up"),
    Key("gg  G", "top · bottom  (NG jumps to page/section N)"),
    Key("J K", "next / previous chapter"),
    Key("Ctrl-o  Ctrl-p", "jump-list back / forward"),
    Key(
        "w b",
        "next / previous rich element (code · table · math · figure)",
    ),
    Key("Tab  s", "focus / toggle the contents sidebar"),
    Gap,
    Heading("Search & lookup"),
    Key("/", "search the book — plain, regex, or fuzzy"),
    Key("n N", "next / previous match"),
    Key(
        "K",
        "look the word up (dictionary · Wikipedia · translation)",
    ),
    Gap,
    Heading("Layout & modes"),
    Key("v", "one column ⇄ two-page spread"),
    Key("p", "page mode — turn whole pages ⇄ scroll by rows"),
    Key("c", "chapter lock — stop at the chapter edge"),
    Key("t  M", "cycle theme · reading preset"),
    Key("[ ]  { }", "text column narrower / wider · line spacing"),
    Key(
        "f  z",
        "focus (distraction-free) mode · toggle the status bar",
    ),
    Gap,
    Heading("PDF"),
    Key("+ -  0", "zoom in / out · reset"),
    Key("W", "fit page / width / height"),
    Key("x", "trim the page margins"),
    Gap,
    Heading("Annotations"),
    Key("m", "bookmark this position"),
    Key("H", "highlight (press again to recolour)"),
    Key("a", "attach a note"),
    Key("'", "open the annotations browser"),
    Gap,
    Heading("Selecting text  (V starts it, Esc leaves)"),
    Key("v  Space", "anchor the selection"),
    Key("y", "copy"),
    Key("c  Tab", "step the highlight pen"),
    Key("⏎  H", "highlight the selection"),
    Key("1 – 5", "pick a highlight colour"),
    Key("a  m", "note · bookmark the line"),
    Key("K", "look up the selected word or phrase"),
    Gap,
    Heading("Browsers & code"),
    Key("I  O", "figure browser · code browser (fullscreen)"),
    Key("Z", "fold every long code block"),
    Key("F", "pick a visible code block to fold (1–9)"),
    Gap,
    Heading("Elsewhere"),
    Key(";", "settings"),
    Key("?", "this help"),
    Key("q  Q", "back to the library · quit delryn"),
];

/// Bindings shown in the library.
const LIBRARY: &[HelpRow] = &[
    Heading("Moving around"),
    Key("h j k l  ← ↓ ↑ →", "move the selection"),
    Key(
        "Ctrl-n  Ctrl-p",
        "next / previous — works in any list, even while typing",
    ),
    Key("⏎  o", "open the book"),
    Key("/", "filter the list"),
    Key("s  S", "sort · reverse the order"),
    Gap,
    Heading("Organising"),
    Key("f", "favorite"),
    Key("0 – 5", "rate"),
    Key("m", "reading status"),
    Key("e", "edit metadata (with Open Library lookup)"),
    Key("T", "edit tags"),
    Key("c", "add to a collection"),
    Key("r", "rename the file"),
    Gap,
    Heading("Selecting several"),
    Key("Space", "mark / unmark"),
    Key("V", "mark a range"),
    Key("A", "mark everything shown"),
    Key("Delete", "move to the trash (asks first)"),
    Gap,
    Heading("View"),
    Key("v", "cycle layout — list / compact / cover grid"),
    Key("+ -", "cover size, in grid layout"),
    Gap,
    Heading("Duplicates"),
    Key("D", "resolve duplicates"),
    Key("R", "deep scan — read each book's contents to match copies"),
    Key("I", "manage ignored groups"),
    Gap,
    Heading("Elsewhere"),
    Key(":", "command palette — every action, by name"),
    Key("i", "library statistics"),
    Key(";", "settings (folders live under Sources)"),
    Key("?", "this help"),
    Key("q", "quit delryn"),
];

/// The rows for `scope`.
pub fn rows(scope: Mode) -> &'static [HelpRow] {
    match scope {
        Mode::Reader => READER,
        Mode::Library => LIBRARY,
    }
}

impl App {
    /// Open the key reference for the current surface.
    pub(crate) fn open_help(&mut self) {
        self.overlay = Overlay::Help(Help {
            scope: self.mode,
            scroll: 0,
            visible: 0,
        });
    }

    /// Keys while help is open. It's a read-only list, so everything either
    /// scrolls or closes.
    pub(crate) fn help_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Overlay::Help(h) = &mut self.overlay else {
            return;
        };
        let last = h.max_scroll();
        // A page is one screenful less a row of overlap, so nothing is skipped.
        let page = h.visible.saturating_sub(1).max(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => self.overlay = Overlay::None,
            KeyCode::Down => h.scroll = (h.scroll + 1).min(last),
            KeyCode::Up => h.scroll = h.scroll.saturating_sub(1),
            KeyCode::Char('j') if !ctrl => h.scroll = (h.scroll + 1).min(last),
            KeyCode::Char('k') if !ctrl => h.scroll = h.scroll.saturating_sub(1),
            KeyCode::Char('n') if ctrl => h.scroll = (h.scroll + 1).min(last),
            KeyCode::Char('p') if ctrl => h.scroll = h.scroll.saturating_sub(1),
            KeyCode::Char('d') if ctrl => h.scroll = (h.scroll + page / 2).min(last),
            KeyCode::Char('u') if ctrl => h.scroll = h.scroll.saturating_sub(page / 2),
            KeyCode::PageDown | KeyCode::Char(' ') => h.scroll = (h.scroll + page).min(last),
            KeyCode::PageUp => h.scroll = h.scroll.saturating_sub(page),
            KeyCode::Home | KeyCode::Char('g') => h.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => h.scroll = last,
            _ => {}
        }
    }

    /// Scroll help by `d` rows (the mouse wheel).
    pub(crate) fn help_scroll(&mut self, d: isize) {
        let Overlay::Help(h) = &mut self.overlay else {
            return;
        };
        let last = h.max_scroll() as isize;
        h.scroll = (h.scroll as isize + d).clamp(0, last) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Help is only useful if it's true, and the thing that rots is a binding
    /// that changed without this being updated. The README's key tables are the
    /// other public record of the same bindings, so hold the two together: every
    /// key listed here must appear in the README.
    #[test]
    fn every_documented_key_also_appears_in_the_readme() {
        let readme =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))
                .expect("README.md sits at the repo root");
        for scope in [Mode::Reader, Mode::Library] {
            for row in rows(scope) {
                let HelpRow::Key(keys, what) = row else {
                    continue;
                };
                // The first token is the canonical binding; the rest are aliases
                // and arrow forms the README writes differently. Matched in the
                // README's own backticked form, so a bare letter can't pass by
                // turning up somewhere in the prose.
                let key = keys.split_whitespace().next().unwrap_or(keys);
                assert!(
                    readme.contains(&format!("`{key}`")),
                    "{scope:?} help lists `{key}` ({what}) but the README's key \
                     tables do not — one of the two is out of date"
                );
            }
        }
    }

    /// Both lists must be usable: headed, non-trivial, and every entry filled in.
    #[test]
    fn both_surfaces_have_a_complete_reference() {
        for scope in [Mode::Reader, Mode::Library] {
            let rows = rows(scope);
            assert!(
                matches!(rows.first(), Some(HelpRow::Heading(_))),
                "{scope:?} help opens on a heading"
            );
            let keys = rows
                .iter()
                .filter(|r| matches!(r, HelpRow::Key(..)))
                .count();
            assert!(keys >= 15, "{scope:?} help lists {keys} keys — too thin");
            for row in rows {
                if let HelpRow::Key(k, what) = row {
                    assert!(!k.is_empty() && !what.is_empty(), "{scope:?}: blank row");
                }
            }
        }
    }

    /// Scrolling stops where the *view* stops — at the offset that puts the last
    /// row at the bottom of the popup, not at the last row itself.
    ///
    /// The reported bug: holding `j` past the end kept incrementing a scroll the
    /// view was already refusing to act on, so the list "took time to come back"
    /// on the way up — one dead keypress per step overshot.
    #[test]
    fn scrolling_stops_where_the_view_does_not_past_it() {
        let _env = crate::test_env_guard();
        let mut app = App::library();
        app.open_help();
        let total = rows(Mode::Library).len();
        const VISIBLE: usize = 20;
        let Overlay::Help(h) = &mut app.overlay else {
            panic!("help open")
        };
        h.visible = VISIBLE; // as the view records after a draw

        app.help_scroll(-5);
        let Overlay::Help(h) = &app.overlay else {
            panic!("help open")
        };
        assert_eq!(h.scroll, 0, "can't scroll above the first row");

        // Hold `j` well past the end.
        for _ in 0..500 {
            app.help_key(KeyEvent::from(KeyCode::Char('j')));
        }
        let Overlay::Help(h) = &app.overlay else {
            panic!("help open")
        };
        assert_eq!(
            h.scroll,
            total - VISIBLE,
            "stops with the last row at the bottom, banking no overshoot"
        );

        // …so a single `k` moves the list immediately.
        app.help_key(KeyEvent::from(KeyCode::Char('k')));
        let Overlay::Help(h) = &app.overlay else {
            panic!("help open")
        };
        assert_eq!(h.scroll, total - VISIBLE - 1, "one press, one row");
    }

    /// `?` opens it and `?` again closes it.
    #[test]
    fn question_mark_toggles_help() {
        let _env = crate::test_env_guard();
        let mut app = App::library();
        app.on_key(KeyEvent::from(KeyCode::Char('?')));
        assert!(matches!(app.overlay, Overlay::Help(_)), "? opened help");
        app.on_key(KeyEvent::from(KeyCode::Char('?')));
        assert!(matches!(app.overlay, Overlay::None), "? closed it again");
    }
}
