//! Library view — sections sidebar + list/grid of books. Stub for now; the
//! working slice is the Reader. See `DESIGN.md` §5.

use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let has_book = app.reader.is_some();

    let mut lines = vec![
        Line::raw(""),
        Line::from(Span::styled(
            "  delryn",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::raw("  Library view is coming soon — list & grid of your shelf."),
        Line::raw("  For now, open a book directly:  delryn <file.epub>"),
        Line::raw(""),
    ];
    if has_book {
        lines.push(Line::raw("  Enter / l   resume reading"));
    }
    lines.push(Line::raw("  q           quit"));

    let widget = Paragraph::new(Text::from(lines)).block(Block::bordered().title("Library"));
    f.render_widget(widget, area);
}
