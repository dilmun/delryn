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
    OpenAnnotations,
    CopyCode,
    ToggleCodeWrap,
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
        KeyCode::Char('n') => Action::SearchNext,
        KeyCode::Char('N') => Action::SearchPrev,
        KeyCode::Char('m') => Action::AddBookmark,
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
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
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
