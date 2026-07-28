//! The two small modal windows every prompt in the app goes through: a yes/no
//! [`confirm`] and a single-field [`prompt`].
//!
//! Both used to live on the status bar. A status line is glanceable by design —
//! which is exactly wrong for something that needs an answer: a destructive
//! confirmation could be missed entirely, and a text field down there gave no
//! sign that the keyboard had been taken over. These sit in the middle of the
//! screen, over a dimmed page, so a prompt cannot be mistaken for chrome.
//!
//! Anything needing an answer or a keystroke belongs here; the status bar is
//! left to report state. See `DESIGN.md` §7.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::theme::{Role, Theme};

/// Width the modals open at, clamped to the screen by [`super::centered`]. Narrow
/// enough to read as a dialog rather than a pane, wide enough for a book title.
const DIALOG_W: u16 = 64;

/// Wrap `text` to `width`, capped at `max_lines` with an ellipsis on the last —
/// a long question stays readable instead of overflowing the frame.
fn wrap(text: &str, width: u16, max_lines: usize) -> Vec<String> {
    let width = width.max(8) as usize;
    let mut out: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match out.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => {
                if out.len() == max_lines {
                    if let Some(line) = out.last_mut() {
                        let keep: String = line.chars().take(width.saturating_sub(1)).collect();
                        *line = format!("{}…", keep.trim_end());
                    }
                    break;
                }
                out.push(word.to_string());
            }
        }
    }
    out
}

/// Dim the whole screen behind a modal so the prompt reads as the only live
/// thing. Cheap: it restyles the existing cells rather than redrawing them.
fn dim_behind(f: &mut Frame, theme: Theme) {
    let area = f.area();
    let dim = theme.style(Role::Muted);
    let buf = f.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            let cell = &mut buf[(x, y)];
            if let Some(fg) = dim.fg {
                cell.set_fg(fg);
            }
        }
    }
}

/// The shared modal frame: dimmed backdrop, cleared rect, titled border.
fn frame(f: &mut Frame, title: &str, height: u16, theme: Theme, bold: bool) -> Rect {
    dim_behind(f, theme);
    let area = super::centered(f.area(), DIALOG_W, height);
    f.render_widget(Clear, area);
    let block = super::overlay_frame(theme, bold)
        .title(Span::styled(format!(" {title} "), theme.style(Role::Title)))
        .style(theme.style(Role::Body).bg(theme.paper()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// A yes/no confirmation. `danger` styles the affirmative in the warning role,
/// so "this deletes files" looks different from "this saves your edits".
pub fn confirm(f: &mut Frame, question: &str, danger: bool, theme: Theme, bold: bool) {
    let body = wrap(question, DIALOG_W.saturating_sub(6), 3);
    // question rows + a blank + the answer row, inside the border.
    let height = body.len() as u16 + 4;
    let inner = frame(f, "Confirm", height, theme, bold);

    let mut lines: Vec<Line> = body
        .into_iter()
        .map(|l| Line::styled(l, theme.style(Role::Body)).alignment(Alignment::Center))
        .collect();
    lines.push(Line::raw(""));
    let yes_role = if danger {
        Role::Danger
    } else {
        Role::AccentStrong
    };
    lines.push(
        Line::from(vec![
            Span::styled("y / ⏎ ", theme.style(yes_role)),
            Span::styled("confirm", theme.style(Role::Body)),
            Span::styled("   ·   ", theme.style(Role::Hint)),
            Span::styled("n / Esc ", theme.style(Role::Muted)),
            Span::styled("cancel", theme.style(Role::Body)),
        ])
        .alignment(Alignment::Center),
    );
    f.render_widget(Paragraph::new(lines), inner);
}

/// A single-field text prompt: a title, an optional explanatory line, the field
/// with its caret, and the key hints. Used for tag editing, the library filter,
/// and in-book search, so all three look and behave alike.
#[allow(clippy::too_many_arguments)]
pub fn prompt(
    f: &mut Frame,
    title: &str,
    note: Option<&str>,
    text: &str,
    cursor: usize,
    hint: &str,
    theme: Theme,
    bold: bool,
) {
    let note_lines = note
        .map(|n| wrap(n, DIALOG_W.saturating_sub(6), 2))
        .unwrap_or_default();
    // note + blank + field + blank + hint, inside the border.
    let height = note_lines.len() as u16 + 5;
    let inner = frame(f, title, height, theme, bold);

    let mut lines: Vec<Line> = note_lines
        .into_iter()
        .map(|l| Line::styled(format!(" {l}"), theme.style(Role::Muted)))
        .collect();
    if !lines.is_empty() {
        lines.push(Line::raw(""));
    }
    // The field itself, with the caret drawn by the shared input renderer.
    let mut field = vec![Span::styled(" › ", theme.style(Role::AccentStrong))];
    field.extend(super::field_spans(
        text,
        cursor,
        (inner.width as usize).saturating_sub(4),
        theme,
    ));
    lines.push(Line::from(field));
    lines.push(Line::raw(""));
    lines.push(Line::styled(format!(" {hint}"), theme.style(Role::Hint)));
    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A long question must stay inside the frame — the modal is fixed-width, so
    /// an un-wrapped title would spill past the border and mangle it.
    #[test]
    fn a_long_question_wraps_within_the_frame() {
        let q = "Move 42 books to the trash, including a rather long book title that \
                 keeps going well past any sensible width for one line?";
        let lines = wrap(q, DIALOG_W - 6, 3);
        assert!(lines.len() <= 3, "capped at 3 rows, got {}", lines.len());
        for l in &lines {
            assert!(
                l.chars().count() <= (DIALOG_W - 6) as usize,
                "line overflows the frame: {l:?}"
            );
        }
    }

    /// Truncation must be visible: silently dropping the tail of a question could
    /// hide *what* is about to be deleted.
    #[test]
    fn an_over_long_question_is_marked_as_truncated() {
        let q = "word ".repeat(200);
        let lines = wrap(&q, 20, 2);
        assert_eq!(lines.len(), 2);
        assert!(
            lines.last().unwrap().ends_with('…'),
            "the cut has to show, got {:?}",
            lines.last()
        );
    }

    /// Short text is left alone — no stray padding or ellipsis.
    #[test]
    fn a_short_question_is_a_single_untouched_line() {
        assert_eq!(wrap("Delete \"SciFi\"?", 40, 3), vec!["Delete \"SciFi\"?"]);
    }
}
