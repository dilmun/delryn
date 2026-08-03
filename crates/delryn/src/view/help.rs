//! The `?` key reference. Two columns — the keys, right-aligned so they form a
//! readable edge, and what they do — under group headings, scrolled as one list.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{App, HelpRow, Mode, Overlay, help};
use crate::theme::Role;

/// Width of the key column. Wide enough for the longest binding shown
/// (`h j k l  ← ↓ ↑ →`) so nothing in it ever wraps or is cut.
const KEY_COL: usize = 18;

pub fn render(f: &mut Frame, app: &mut App) {
    let Overlay::Help(h) = &app.overlay else {
        return;
    };
    let theme = app.config.theme;
    let area = super::overlay_rect(f.area(), app.overlay_large);

    f.render_widget(Clear, area);

    let title = match h.scope {
        Mode::Reader => " Keys — reading ",
        Mode::Library => " Keys — library ",
    };
    let block = super::overlay_frame(theme, app.config.bold_borders)
        .title(Span::styled(title, theme.style(Role::Title)))
        .style(theme.style(Role::Body).bg(theme.paper()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Min(0),    // the reference
        Constraint::Length(1), // how to move through it
    ])
    .split(inner);

    let list = rows[0];
    let body_w = (list.width as usize).saturating_sub(KEY_COL + 2);
    let lines: Vec<Line> = help::rows(h.scope)
        .iter()
        .map(|row| match row {
            HelpRow::Gap => Line::from(""),
            HelpRow::Heading(text) => Line::from(Span::styled(
                (*text).to_string(),
                theme.style(Role::Heading).add_modifier(Modifier::BOLD),
            )),
            HelpRow::Key(keys, what) => Line::from(vec![
                Span::styled(
                    format!("{:>KEY_COL$}", super::truncate(keys, KEY_COL)),
                    theme.style(Role::Accent),
                ),
                Span::styled("  ", theme.style(Role::Body)),
                Span::styled(super::truncate(what, body_w), theme.style(Role::Body)),
            ]),
        })
        .collect();

    let total = lines.len();
    let h_rows = list.height as usize;
    // Never scroll past the point where the last row sits at the bottom — a list
    // that scrolls into blank space reads as "there is more", and there isn't.
    let offset = h.scroll.min(total.saturating_sub(h_rows));
    let view: Vec<Line> = lines.into_iter().skip(offset).take(h_rows).collect();
    f.render_widget(Paragraph::new(view), list);

    // The popup's height is only known here, so hand it back: it's what bounds
    // scrolling (`Help::max_scroll`) and sizes a page. Writing the clamped offset
    // back keeps the two from drifting — after a resize the stored scroll can be
    // past the new end, and this pulls it in on the first frame that shows it.
    if let Overlay::Help(h) = &mut app.overlay {
        h.visible = h_rows;
        h.scroll = offset;
    }

    let more = total.saturating_sub(offset + h_rows);
    let hint = if more > 0 {
        format!("j/k scroll  ·  {more} more below  ·  Esc close")
    } else if offset > 0 {
        "j/k scroll  ·  end of list  ·  Esc close".to_string()
    } else {
        "Esc close".to_string()
    };
    f.render_widget(
        Paragraph::new(Line::styled(hint, theme.style(Role::Muted))),
        rows[1],
    );
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Draw the library help and return the screen as text.
    fn screen() -> String {
        let _env = crate::test_env_guard();
        let dir = std::env::temp_dir().join(format!("delryn_helpview_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: serialized by `_env`; keeps `App::library()` off the real config dir.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };

        let mut app = App::library();
        app.open_help();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::view::render(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::remove_dir_all(&dir);
        text
    }

    /// Each binding lands on one line beside what it does — the whole point of the
    /// two-column layout, and the thing a width change would quietly break.
    #[test]
    fn a_binding_and_its_description_share_a_line() {
        let text = screen();
        assert!(
            text.lines()
                .any(|l| l.contains("e") && l.contains("edit metadata")),
            "the key column sits beside its description\n{text}"
        );
        assert!(text.contains("Moving around"), "groups are headed\n{text}");
    }

    /// A list taller than the popup must say so — otherwise the keys below the
    /// fold may as well not exist.
    #[test]
    fn a_list_that_overflows_says_how_much_is_left() {
        let text = screen();
        assert!(
            text.contains("more below"),
            "the overflow is announced\n{text}"
        );
        assert!(text.contains("Esc close"), "and the way out\n{text}");
    }
}
