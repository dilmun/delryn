//! Context-aware keymap. Pure mapping from crossterm events to [`Action`]s;
//! the app interprets actions against the current focus/mode. Vim defaults for
//! now; rebinding lands with General settings. See `DESIGN.md` §9.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Transient vim input state (count prefix, pending `g`).
#[derive(Default)]
pub struct Pending {
    pub count: Option<usize>,
    pub g: bool,
}

/// The list motions that stay available **while a text field has focus**: the
/// arrows, and `Ctrl-n` / `Ctrl-p`.
///
/// A filter or query box has to receive `j` and `k` as letters, which leaves a
/// typed-into list with nothing but the arrow keys — the one place a
/// keyboard-driven app most wants a home-row alternative. `Ctrl-n`/`Ctrl-p` are
/// the long-standing answer (readline, vim's completion menu, every fuzzy
/// finder), so they move the selection on every list in the app, typed-into or
/// not. Returns the new index, or `None` if `key` isn't one of them.
pub fn list_nav_typing(key: KeyEvent, sel: usize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len - 1;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let next = match key.code {
        KeyCode::Down => (sel + 1).min(last),
        KeyCode::Up => sel.saturating_sub(1),
        KeyCode::Char('n') if ctrl => (sel + 1).min(last),
        KeyCode::Char('p') if ctrl => sel.saturating_sub(1),
        _ => return None,
    };
    Some(next)
}

/// Standard vim navigation for any selectable list: the new selection index for
/// `key`, or `None` if it isn't a nav key. `j/k` (and arrows) step and clamp;
/// `Ctrl-d/Ctrl-u` and PageDown/PageUp move a half-page (`page` rows); `g`/Home
/// and `G`/End jump to the ends; `Ctrl-n`/`Ctrl-p` step, as in a text field
/// ([`list_nav_typing`]). Shared by every list/overlay so navigation feels the
/// same everywhere.
pub fn list_nav(key: KeyEvent, sel: usize, len: usize, page: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len - 1;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let next = match key.code {
        // The bare-letter motions must not fire with Ctrl held, or `Ctrl-n`'s
        // neighbours on the home row would swallow their own chords.
        KeyCode::Char('j') if !ctrl => (sel + 1).min(last),
        KeyCode::Char('k') if !ctrl => sel.saturating_sub(1),
        KeyCode::Char('d') if ctrl => (sel + page).min(last),
        KeyCode::Char('u') if ctrl => sel.saturating_sub(page),
        KeyCode::PageDown => (sel + page).min(last),
        KeyCode::PageUp => sel.saturating_sub(page),
        KeyCode::Char('g') if !ctrl => 0,
        KeyCode::Char('G') if !ctrl => last,
        KeyCode::Home => 0,
        KeyCode::End => last,
        // Arrows and the Ctrl-n/Ctrl-p pair, shared with typed-into lists.
        _ => return list_nav_typing(key, sel, len),
    };
    Some(next)
}

/// A semantic intent. `Down`/`Up` are routed by focus (scroll vs. select).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Back,
    Down(usize),
    Up(usize),
    HalfDown,
    HalfUp,
    PageDown,
    PageUp,
    Top,
    Bottom,
    /// Jump to page/section N (1-based), from a count-prefixed `G` (`50G`).
    Goto(usize),
    ToggleStatus,
    ToggleSidebar,
    CycleView,
    CycleTheme,
    CycleReadingMode,
    ToggleFocus,
    WidthDown,
    WidthUp,
    LineSpacingDown,
    LineSpacingUp,
    FocusToggle,
    Activate,
    Expand,
    Collapse,
    HistBack,
    HistForward,
    Search,
    SearchNext,
    SearchPrev,
    AddBookmark,
    AddNote,
    /// Highlight the current line, cycling its colour (and off) on repeat.
    AddHighlight,
    /// Enter visual (vim-style) text selection.
    StartSelection,
    OpenAnnotations,
    CopyCode,
    ToggleCodeWrap,
    /// Fold/unfold every long code block on the page (global).
    ToggleFold,
    /// Fold/unfold the code block under the cursor (per-block override).
    ToggleFoldBlock,
    PanLeft,
    PanRight,
    ToggleChapterLock,
    NextChapter,
    PrevChapter,
    /// Jump to the next rich element (code/table/math/figure).
    NextElement,
    /// Jump to the previous rich element.
    PrevElement,
    /// Toggle paginated (page-flip) reading vs continuous scrolling.
    TogglePaged,
    /// Move the link cursor to the next inline reference (footnote/cross-ref/link).
    NextAnchor,
    /// Move the link cursor to the previous inline reference.
    PrevAnchor,
    /// Dismiss the link cursor.
    ClearAnchor,
    /// Zoom the paged (PDF) page in / out / reset to fit-page.
    ZoomIn,
    ZoomOut,
    ZoomReset,
    /// Cycle the paged fit mode (page → width → height).
    FitCycle,
    /// Toggle trimming the whitespace margins of PDF pages.
    ToggleTrim,
}

pub fn map_key(key: KeyEvent, pending: &mut Pending) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // Accumulate a vim count prefix (a leading 0 is "go to col 0", not a count).
    if let KeyCode::Char(c) = key.code
        && c.is_ascii_digit()
        && !(c == '0' && pending.count.is_none())
    {
        let d = c as usize - '0' as usize;
        pending.count = Some(pending.count.unwrap_or(0) * 10 + d);
        return Action::None;
    }
    let explicit_count = pending.count.take();
    let count = explicit_count.unwrap_or(1);

    // `gg` → top.
    if pending.g {
        pending.g = false;
        if let KeyCode::Char('g') = key.code {
            return Action::Top;
        }
    }

    match key.code {
        KeyCode::Char('q') => Action::Back,
        KeyCode::Char('Q') => Action::Quit,
        KeyCode::Char('j') | KeyCode::Down => Action::Down(count),
        KeyCode::Char('k') | KeyCode::Up => Action::Up(count),
        KeyCode::Char('d') if ctrl => Action::HalfDown,
        KeyCode::Char('u') if ctrl => Action::HalfUp,
        KeyCode::Char('f') if ctrl => Action::PageDown,
        KeyCode::Char('b') if ctrl => Action::PageUp,
        KeyCode::Char('o') if ctrl => Action::HistBack,
        KeyCode::Char('p') if ctrl => Action::HistForward,
        KeyCode::Char(' ') | KeyCode::PageDown => Action::PageDown,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::Char('g') => {
            pending.g = true;
            Action::None
        }
        // `G` jumps to the last page; `NG` (count-prefixed) jumps to page N.
        KeyCode::Char('G') => match explicit_count {
            Some(n) => Action::Goto(n),
            None => Action::Bottom,
        },
        KeyCode::Home => Action::Top,
        KeyCode::End => Action::Bottom,
        KeyCode::Tab => Action::FocusToggle,
        KeyCode::Char('s') => Action::ToggleSidebar,
        KeyCode::Char('v') => Action::CycleView,
        KeyCode::Char('t') => Action::CycleTheme,
        KeyCode::Char('z') => Action::ToggleStatus,
        KeyCode::Char('f') => Action::ToggleFocus,
        KeyCode::Char('[') => Action::WidthDown,
        KeyCode::Char(']') => Action::WidthUp,
        KeyCode::Char('{') => Action::LineSpacingDown,
        KeyCode::Char('}') => Action::LineSpacingUp,
        KeyCode::Enter => Action::Activate,
        KeyCode::Char('l') | KeyCode::Right => Action::Expand,
        KeyCode::Char('h') | KeyCode::Left => Action::Collapse,
        KeyCode::Char('/') => Action::Search,
        // Guarded: without it `Ctrl-n` — "next item" everywhere else in the app —
        // silently jumped to the next search match here. `Ctrl-p` is the jump
        // list (above), so the pair stays out of the reader's motions entirely.
        KeyCode::Char('n') if !ctrl => Action::SearchNext,
        KeyCode::Char('N') => Action::SearchPrev,
        KeyCode::Char('m') => Action::AddBookmark,
        KeyCode::Char('a') => Action::AddNote,
        KeyCode::Char('H') => Action::AddHighlight,
        KeyCode::Char('V') => Action::StartSelection,
        KeyCode::Char('M') => Action::CycleReadingMode,
        KeyCode::Char('\'') => Action::OpenAnnotations,
        KeyCode::Char('y') => Action::CopyCode,
        KeyCode::Char('w') => Action::NextElement,
        KeyCode::Char('b') if !ctrl => Action::PrevElement,
        KeyCode::Char('p') => Action::TogglePaged,
        KeyCode::Char('e') => Action::NextAnchor,
        KeyCode::Char('E') => Action::PrevAnchor,
        KeyCode::Esc => Action::ClearAnchor,
        KeyCode::Char('\\') => Action::ToggleCodeWrap,
        KeyCode::Char('Z') => Action::ToggleFold,
        KeyCode::Char('F') => Action::ToggleFoldBlock,
        KeyCode::Char('<') => Action::PanLeft,
        KeyCode::Char('>') => Action::PanRight,
        KeyCode::Char('c') => Action::ToggleChapterLock,
        KeyCode::Char('J') => Action::NextChapter,
        KeyCode::Char('K') => Action::PrevChapter,
        // Paged (PDF) zoom & fit. `0` only reaches here as a lone key (a leading
        // `0` isn't a count digit); with a pending count it's consumed above.
        KeyCode::Char('+') | KeyCode::Char('=') => Action::ZoomIn,
        KeyCode::Char('-') => Action::ZoomOut,
        KeyCode::Char('0') => Action::ZoomReset,
        KeyCode::Char('W') => Action::FitCycle,
        KeyCode::Char('x') => Action::ToggleTrim,
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn list_nav_covers_the_vim_motions() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(list_nav(key('j'), 0, 5, 3), Some(1));
        assert_eq!(list_nav(key('j'), 4, 5, 3), Some(4)); // clamps at bottom
        assert_eq!(list_nav(key('k'), 0, 5, 3), Some(0)); // clamps at top
        assert_eq!(list_nav(ctrl('d'), 0, 10, 3), Some(3)); // half-page down
        assert_eq!(list_nav(ctrl('u'), 1, 10, 3), Some(0)); // clamps at top
        assert_eq!(list_nav(key('g'), 4, 5, 3), Some(0));
        assert_eq!(list_nav(key('G'), 0, 5, 3), Some(4));
        assert_eq!(list_nav(key('x'), 0, 5, 3), None); // not a nav key
        assert_eq!(list_nav(key('j'), 0, 0, 3), None); // empty list
    }

    /// `Ctrl-n`/`Ctrl-p` move through every list, including the ones being typed
    /// into where `j`/`k` have to stay letters. They were bound nowhere in the
    /// app before, so a filter or query box could only be navigated with arrows.
    #[test]
    fn ctrl_n_and_ctrl_p_step_lists_typed_into_or_not() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);

        for nav in [list_nav_typing, |k, s, l| list_nav(k, s, l, 3)] {
            assert_eq!(nav(ctrl('n'), 0, 5), Some(1), "Ctrl-n steps down");
            assert_eq!(nav(ctrl('p'), 3, 5), Some(2), "Ctrl-p steps up");
            assert_eq!(nav(ctrl('n'), 4, 5), Some(4), "clamps at the bottom");
            assert_eq!(nav(ctrl('p'), 0, 5), Some(0), "clamps at the top");
            // Arrows work in both; bare letters must not reach a text field.
            assert_eq!(nav(KeyEvent::from(KeyCode::Down), 0, 5), Some(1));
            assert_eq!(nav(ctrl('x'), 0, 5), None, "not a nav chord");
        }
        assert_eq!(
            list_nav_typing(key('j'), 0, 5),
            None,
            "a typed-into list leaves `j` to the text field"
        );
    }

    /// The bare-letter motions must not also fire when Ctrl is held, or `Ctrl-j`
    /// / `Ctrl-g` would move as well as whatever the chord is meant to do.
    #[test]
    fn bare_letter_motions_ignore_the_control_modifier() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(list_nav(ctrl('j'), 0, 5, 3), None);
        assert_eq!(list_nav(ctrl('k'), 2, 5, 3), None);
        assert_eq!(list_nav(ctrl('g'), 2, 5, 3), None);
        assert_eq!(list_nav(ctrl('G'), 2, 5, 3), None);
    }

    /// `G` jumps to the bottom; a count prefix turns it into an absolute
    /// `Goto(N)`, and the count is consumed (doesn't leak into the next key).
    #[test]
    fn count_prefixed_g_is_goto() {
        let mut p = Pending::default();
        assert_eq!(map_key(key('G'), &mut p), Action::Bottom);

        // `50G` → Goto(50).
        assert_eq!(map_key(key('5'), &mut p), Action::None);
        assert_eq!(map_key(key('0'), &mut p), Action::None);
        assert_eq!(map_key(key('G'), &mut p), Action::Goto(50));

        // Count cleared: a bare `G` is Bottom again, and `10j` still counts.
        assert_eq!(map_key(key('G'), &mut p), Action::Bottom);
        assert_eq!(map_key(key('1'), &mut p), Action::None);
        assert_eq!(map_key(key('0'), &mut p), Action::None);
        assert_eq!(map_key(key('j'), &mut p), Action::Down(10));
    }
}
