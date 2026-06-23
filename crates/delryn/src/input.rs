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
    ToggleStatus,
    ToggleSidebar,
    CycleView,
    CycleTheme,
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
    OpenAnnotations,
    CopyCode,
    ToggleCodeWrap,
    PanLeft,
    PanRight,
    ToggleChapterLock,
    NextChapter,
    PrevChapter,
    /// Jump to the next code block in the chapter.
    NextCode,
    /// Jump to the previous code block in the chapter.
    PrevCode,
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
    let count = pending.count.take().unwrap_or(1);

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
        KeyCode::Char('G') => Action::Bottom,
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
        KeyCode::Char('M') => Action::AddNote,
        KeyCode::Char('\'') => Action::OpenAnnotations,
        KeyCode::Char('y') => Action::CopyCode,
        KeyCode::Char('w') => Action::NextCode,
        KeyCode::Char('b') if !ctrl => Action::PrevCode,
        KeyCode::Char('\\') => Action::ToggleCodeWrap,
        KeyCode::Char('<') => Action::PanLeft,
        KeyCode::Char('>') => Action::PanRight,
        KeyCode::Char('c') => Action::ToggleChapterLock,
        KeyCode::Char('J') => Action::NextChapter,
        KeyCode::Char('K') => Action::PrevChapter,
        _ => Action::None,
    }
}
