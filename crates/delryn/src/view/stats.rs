//! Library-statistics overlay: a centered popup summarising the collection.

use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{App, Overlay};
use crate::library::stats::fmt_duration;
use crate::theme::Role;

pub fn render(f: &mut Frame, app: &App) {
    let Overlay::Stats(s) = &app.overlay else {
        return;
    };
    let theme = app.config.theme;
    let bold = app.config.bold_borders;
    let area = super::overlay_rect(f.area(), app.overlay_large);
    f.render_widget(Clear, area);

    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!(" {k:<12}"), theme.style(Role::Muted)),
            Span::styled(v, theme.style(Role::Body)),
        ])
    };

    let mut lines = vec![
        kv("Books", s.total.to_string()),
        kv("Finished", s.finished.to_string()),
        kv("Reading", s.reading.to_string()),
        kv("Unread", s.unread.to_string()),
        kv("Favorites", s.favorites.to_string()),
        kv(
            "Rated",
            if s.rated > 0 {
                format!("{} (avg {:.1}★)", s.rated, s.avg_rating)
            } else {
                "0".to_string()
            },
        ),
        kv("Read time", fmt_duration(s.read_seconds)),
    ];
    if !s.top_authors.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            " Top authors",
            theme.style(Role::Hint).add_modifier(Modifier::BOLD),
        )));
        for (name, count) in &s.top_authors {
            lines.push(kv("", format!("{name}  ({count})")));
        }
    }

    let block = super::overlay_frame(theme, bold)
        .title(Span::styled(
            " Library statistics ",
            theme.style(Role::Title),
        ))
        .title_bottom(Line::from(Span::styled(
            " any key to close ",
            theme.style(Role::Muted),
        )))
        .title_alignment(Alignment::Center)
        .style(theme.text_style());
    f.render_widget(Paragraph::new(lines).block(block), area);
}
