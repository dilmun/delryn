//! Library-statistics overlay: a centered popup summarising the collection.

use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::App;
use crate::library::stats::fmt_duration;
use crate::theme::Theme;

pub fn render(f: &mut Frame, app: &App) {
    let Some(s) = app.stats.as_ref() else {
        return;
    };
    let theme = app.config.theme;
    let area = super::centered(f.area(), 46, (10 + s.top_authors.len() as u16).min(22));
    f.render_widget(Clear, area);

    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!(" {k:<12}"), Style::default().fg(theme.muted)),
            Span::styled(v, Style::default().fg(theme.fg)),
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
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD | Modifier::DIM),
        )));
        for (name, count) in &s.top_authors {
            lines.push(kv("", format!("{name}  ({count})")));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Library statistics ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(Span::styled(
            " any key to close ",
            Style::default().fg(theme.muted),
        )))
        .title_alignment(Alignment::Center)
        .style(base(theme));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn base(theme: Theme) -> Style {
    let s = Style::default().fg(theme.fg);
    match theme.bg {
        Some(bg) => s.bg(bg),
        None => s,
    }
}
