//! Fullscreen code-block browser (`O`): a sidebar of the code blocks in the
//! current chapter (toggle to whole-book with `w`), the selected block scrollable
//! and syntax-highlighted, with copy-all (`y`). Like the image viewer, for code.
//! Presentation lives in `view::code_view`.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Overlay};
use crate::document::Block;

/// One code block available in the viewer: where it lives, its language, a
/// sidebar label, and the source lines.
pub struct CodeSnippet {
    pub section: usize,
    /// Index among the section's code blocks (matches `LineKind::Code`).
    pub code_index: usize,
    pub lang: Option<String>,
    /// Sidebar label — language + first non-blank line, else "Code N".
    pub label: String,
    pub lines: Vec<String>,
}

/// Collect a section's code blocks into `out`, labelling each by its language and
/// first meaningful line (books rarely title code, so the first line reads best).
pub fn collect_code_blocks(blocks: &[Block], section: usize, out: &mut Vec<CodeSnippet>) {
    let mut code_index = 0usize;
    for b in blocks {
        let Block::Code { lang, lines } = b else {
            continue;
        };
        let idx = code_index;
        code_index += 1;
        let label = label_for(lang.as_deref(), lines, out.len() + 1);
        out.push(CodeSnippet {
            section,
            code_index: idx,
            lang: lang.clone(),
            label,
            lines: lines.clone(),
        });
    }
}

/// A sidebar label: `"<lang> · <first line>"`, degrading to one part or `Code N`.
fn label_for(lang: Option<&str>, lines: &[String], n: usize) -> String {
    let first: String = lines
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(48)
        .collect();
    match (
        lang.map(str::trim).filter(|l| !l.is_empty()),
        first.is_empty(),
    ) {
        (Some(l), false) => format!("{l} · {first}"),
        (Some(l), true) => format!("{l} · Code {n}"),
        (None, false) => first,
        (None, true) => format!("Code {n}"),
    }
}

/// Which pane the keys drive: the block list, or the code content.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CodeFocus {
    Sidebar,
    Content,
}

/// The open code browser: its snippets, the selection, scope, the focused pane,
/// and the selected block's scroll offset.
pub struct CodeView {
    snippets: Vec<CodeSnippet>,
    pub sel: usize,
    /// Whole-book list (true) vs. just the current chapter (false).
    pub whole_book: bool,
    pub focus: CodeFocus,
    pub scroll: u16,
    pub copied: bool,
}

impl CodeView {
    pub fn new(snippets: Vec<CodeSnippet>, whole_book: bool) -> Option<CodeView> {
        (!snippets.is_empty()).then_some(CodeView {
            snippets,
            sel: 0,
            whole_book,
            focus: CodeFocus::Sidebar,
            scroll: 0,
            copied: false,
        })
    }

    /// Toggle which pane the keys drive (sidebar list ⇄ code content).
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            CodeFocus::Sidebar => CodeFocus::Content,
            CodeFocus::Content => CodeFocus::Sidebar,
        };
    }

    pub fn current(&self) -> Option<&CodeSnippet> {
        self.snippets.get(self.sel)
    }

    pub fn visible(&self) -> impl Iterator<Item = (usize, &CodeSnippet)> {
        self.snippets.iter().enumerate()
    }

    /// Selection position (1-based) and count, for the title.
    pub fn position(&self) -> (usize, usize) {
        (self.sel + 1, self.snippets.len())
    }

    /// Number of code blocks (for list navigation).
    pub fn count(&self) -> usize {
        self.snippets.len()
    }

    /// Select block `i` (from list navigation); resets scroll + the copied flag.
    pub fn set_sel(&mut self, i: usize) {
        if i < self.snippets.len() {
            self.sel = i;
            self.scroll = 0;
            self.copied = false;
        }
    }

    /// Move the selection (wrapping); resets scroll + the copied flag for the new
    /// block.
    pub fn move_sel(&mut self, delta: isize) {
        let n = self.snippets.len() as isize;
        if n == 0 {
            return;
        }
        self.sel = (self.sel as isize + delta).rem_euclid(n) as usize;
        self.scroll = 0;
        self.copied = false;
    }

    /// Select the block matching `(section, code_index)`, if present.
    pub fn select_code(&mut self, section: usize, code_index: usize) {
        if let Some(pos) = self
            .snippets
            .iter()
            .position(|s| s.section == section && s.code_index == code_index)
        {
            self.sel = pos;
            self.scroll = 0;
        }
    }
}

impl App {
    /// Open the code browser for the current chapter, pre-selecting the block in
    /// view. Flashes a hint when the chapter has no code.
    pub fn open_code_view(&mut self) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let target = reader.current_code_index().map(|idx| (reader.section, idx));
        let snippets = reader.code_blocks(false);
        let Some(mut cv) = CodeView::new(snippets, false) else {
            reader.flash = Some("No code blocks in this chapter".into());
            return;
        };
        if let Some((sec, idx)) = target {
            cv.select_code(sec, idx);
        }
        self.overlay = Overlay::CodeView(cv);
    }

    /// Jump the reader to the selected code block's location and close the viewer
    /// (the code counterpart of `image_go_selected`).
    fn code_go_selected(&mut self) {
        let target = match &self.overlay {
            Overlay::CodeView(cv) => cv.current().map(|s| (s.section, s.code_index)),
            _ => None,
        };
        if let Some((section, code_index)) = target {
            if let Some(r) = self.reader.as_mut() {
                r.jump_to_code(section, code_index);
            }
            self.overlay = Overlay::None;
        }
    }

    /// Rebuild the browser toggling chapter ⇄ whole-book scope, keeping the
    /// selected block selected.
    fn toggle_code_scope(&mut self) {
        let Overlay::CodeView(cv) = &self.overlay else {
            return;
        };
        let whole = !cv.whole_book;
        let keep = cv.current().map(|s| (s.section, s.code_index));
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let snippets = reader.code_blocks(whole);
        let Some(mut new_cv) = CodeView::new(snippets, whole) else {
            return;
        };
        if let Some((sec, idx)) = keep {
            new_cv.select_code(sec, idx);
        }
        self.overlay = Overlay::CodeView(new_cv);
    }

    /// Keys while the code browser is open.
    pub fn code_view_key(&mut self, key: KeyEvent) {
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('O')
        ) {
            self.overlay = Overlay::None;
            return;
        }
        // Enter jumps the reader to the block's location.
        if key.code == KeyCode::Enter {
            self.code_go_selected();
            return;
        }
        if key.code == KeyCode::Char('w') {
            self.toggle_code_scope();
            return;
        }
        if key.code == KeyCode::Char('y') {
            let text = if let Overlay::CodeView(cv) = &self.overlay {
                cv.current().map(|s| s.lines.join("\n"))
            } else {
                None
            };
            if let Some(text) = text {
                if let Some(r) = self.reader.as_mut() {
                    r.stage_clipboard(text);
                }
                if let Overlay::CodeView(cv) = &mut self.overlay {
                    cv.copied = true;
                }
            }
            return;
        }
        // Tab switches which pane j/k drives (list ⇄ code).
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            if let Overlay::CodeView(cv) = &mut self.overlay {
                cv.toggle_focus();
            }
            return;
        }
        let Overlay::CodeView(cv) = &mut self.overlay else {
            return;
        };
        // `[`/`]` always switch blocks, regardless of focus.
        if let KeyCode::Char(c @ ('[' | ']')) = key.code {
            cv.move_sel(if c == ']' { 1 } else { -1 });
            return;
        }
        const HALF: u16 = 10;
        match cv.focus {
            // Sidebar: full vim list navigation on the block selection.
            CodeFocus::Sidebar => {
                if let Some(ns) = crate::input::list_nav(key, cv.sel, cv.count(), HALF as usize) {
                    cv.set_sel(ns);
                }
            }
            // Code pane: vim scrolling (Ctrl-d/u + PageDown/Up half-page).
            CodeFocus::Content => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => cv.scroll = cv.scroll.saturating_add(1),
                    KeyCode::Char('k') | KeyCode::Up => cv.scroll = cv.scroll.saturating_sub(1),
                    KeyCode::Char('n') if ctrl => cv.scroll = cv.scroll.saturating_add(1),
                    KeyCode::Char('p') if ctrl => cv.scroll = cv.scroll.saturating_sub(1),
                    KeyCode::Char('d') if ctrl => cv.scroll = cv.scroll.saturating_add(HALF),
                    KeyCode::Char('u') if ctrl => cv.scroll = cv.scroll.saturating_sub(HALF),
                    KeyCode::PageDown => cv.scroll = cv.scroll.saturating_add(HALF),
                    KeyCode::PageUp => cv.scroll = cv.scroll.saturating_sub(HALF),
                    KeyCode::Char('g') | KeyCode::Home => cv.scroll = 0,
                    KeyCode::Char('G') | KeyCode::End => cv.scroll = u16::MAX,
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(lang: Option<&str>, first: &str) -> Block {
        Block::Code {
            lang: lang.map(str::to_string),
            lines: vec![first.to_string(), "more".to_string()],
        }
    }

    #[test]
    fn labels_use_language_and_first_line() {
        let mut out = Vec::new();
        collect_code_blocks(
            &[
                code(Some("python"), "import os"),
                code(None, "SELECT 1"),
                Block::Code {
                    lang: Some("rust".into()),
                    lines: vec!["  ".into()],
                },
            ],
            3,
            &mut out,
        );
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].label, "python · import os");
        assert_eq!(out[0].code_index, 0);
        assert_eq!(out[0].section, 3);
        assert_eq!(out[1].label, "SELECT 1");
        assert_eq!(out[2].label, "rust · Code 3"); // no non-blank line
    }

    #[test]
    fn select_and_move_wrap() {
        let mut out = Vec::new();
        collect_code_blocks(&[code(Some("a"), "1"), code(Some("b"), "2")], 0, &mut out);
        let mut cv = CodeView::new(out, false).unwrap();
        assert_eq!(cv.position(), (1, 2));
        cv.select_code(0, 1);
        assert_eq!(cv.sel, 1);
        cv.move_sel(1); // wraps back to 0
        assert_eq!(cv.sel, 0);
    }
}
