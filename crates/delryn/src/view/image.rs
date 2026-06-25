//! Fullscreen image viewer overlay (the `i` key). Renders the current
//! section's images via the terminal graphics protocol. See `DESIGN.md` §18.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Clear};
use ratatui_image::{Resize, StatefulImage};

use crate::app::App;

pub fn render(f: &mut Frame, app: &mut App) {
    let theme = app.config.theme;
    let Some(view) = app.image_view.as_mut() else {
        return;
    };
    let area = f.area();
    f.render_widget(Clear, area);

    let bg = theme.paper();
    let title = format!(" Image {}/{} ", view.sel + 1, view.protocols.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Line::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Leave a small margin so the image doesn't touch the border.
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(inner);
    let cols = Layout::horizontal([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .split(rows[1]);
    let img_area: Rect = cols[1];

    if let Some(proto) = view.protocols.get_mut(view.sel) {
        let widget = StatefulImage::default().resize(Resize::Fit(None));
        f.render_stateful_widget(widget, img_area, proto);
    }
}
