//! Image viewer overlay (the `i` key): a figure sidebar, the selected figure
//! rendered large and centered, and its details. See `DESIGN.md` §18.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui_image::{FontSize, Resize, StatefulImage};

use crate::app::App;

pub fn render(f: &mut Frame, app: &mut App) {
    let theme = app.config.theme;
    let policy = crate::media::RenderPolicy {
        tint: super::theme_ink(theme),
        mode: app.config.image_mode,
    };
    // Disjoint field borrows: picker (read), viewer (mutate to build the proto),
    // reader (chapter titles).
    let App {
        picker,
        image_view,
        reader,
        ..
    } = app;
    let (Some(picker), Some(viewer)) = (picker.as_ref(), image_view.as_mut()) else {
        return;
    };
    let reader = reader.as_ref();
    let font = picker.font_size();
    let bg = theme.paper();

    let area = f.area();
    f.render_widget(Clear, area);

    // Outer frame: position + scope, or a transient flash (e.g. "saved …").
    let (pos, count) = viewer.position();
    let scope = if viewer.whole_book { "book" } else { "chapter" };
    let title = match &viewer.flash {
        Some(flash) => format!(" {flash} "),
        None => format!(" Figures · {pos}/{count} · {scope} "),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(Span::styled(
            " ↑↓ select · ⏎ go to figure · / filter · w chapter/book · s save · Esc close ",
            Style::default().fg(theme.muted),
        )))
        .style(Style::default().fg(theme.fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::horizontal([
        Constraint::Length(34),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(inner);
    let (sidebar, right) = (cols[0], cols[2]);

    let chapter_title = |sec: usize| -> String {
        reader
            .and_then(|r| {
                r.outline
                    .iter()
                    .find(|e| e.section == sec)
                    .map(|e| e.label.clone())
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("§{}", sec + 1))
    };

    // Sidebar: the filtered figure list (immutable borrow of the viewer).
    if viewer.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(
                "  No figures.",
                Style::default().fg(theme.muted),
            )),
            sidebar,
        );
    } else {
        let items: Vec<ListItem> = viewer
            .visible()
            .map(|(_, fig)| {
                Line::from(vec![
                    Span::styled(
                        format!("§{} ", fig.section + 1),
                        Style::default().fg(theme.muted),
                    ),
                    Span::styled(fig.name.clone(), Style::default().fg(theme.fg)),
                ])
                .into()
            })
            .collect();
        let list = List::new(items).highlight_style(
            Style::default()
                .fg(theme.on_accent())
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
        let mut st = ListState::default();
        st.select(Some(viewer.sel));
        f.render_stateful_widget(list, sidebar, &mut st);
    }

    let rrows = Layout::vertical([Constraint::Min(0), Constraint::Length(5)]).split(right);
    let (img_box, detail_area) = (rrows[0], rrows[1]);

    // Read the selected figure's details before the mutable proto build.
    let (dims, caption, name, section) = match viewer.current() {
        Some(fig) => (fig.dims, fig.caption.clone(), fig.name.clone(), fig.section),
        None => (None, String::new(), String::new(), 0),
    };

    let mut meta: Vec<Span> = vec![Span::styled(
        format!("Chapter: {}", chapter_title(section)),
        Style::default().fg(theme.heading),
    )];
    if let Some((w, h)) = dims {
        meta.push(Span::styled(
            format!("    {w}×{h}px"),
            Style::default().fg(theme.muted),
        ));
    }
    let mut dlines = vec![Line::from(meta)];
    let body = if caption.is_empty() { name } else { caption };
    if !body.is_empty() {
        dlines.push(Line::from(Span::styled(
            body,
            Style::default().fg(theme.fg),
        )));
    }
    f.render_widget(
        Paragraph::new(dlines)
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(theme.fg).bg(bg)),
        detail_area,
    );

    // The image: centered to its fitted size with equal padding all round.
    let widget = StatefulImage::default().resize(Resize::Fit(None));
    let target = match dims {
        Some(d) => centered_fit(inset(img_box, 1), d, font),
        None => inset(img_box, 1),
    };
    if let Some(proto) = viewer.ensure_proto(picker, policy) {
        f.render_stateful_widget(widget, target, proto);
    }
}

/// Shrink a rect by `m` cells on every side (clamped).
fn inset(r: Rect, m: u16) -> Rect {
    Rect {
        x: r.x + m.min(r.width / 2),
        y: r.y + m.min(r.height / 2),
        width: r.width.saturating_sub(m * 2),
        height: r.height.saturating_sub(m * 2),
    }
}

/// A rect sized to the image's fitted dimensions, centered in `area` (so the
/// padding is equal on all sides), given the terminal's pixels-per-cell `font`.
fn centered_fit(area: Rect, dims: (u32, u32), font: FontSize) -> Rect {
    if area.width == 0 || area.height == 0 {
        return area;
    }
    let (iw, ih) = (dims.0.max(1) as f32, dims.1.max(1) as f32);
    let (fw, fh) = (font.width.max(1) as f32, font.height.max(1) as f32);
    let scale = ((area.width as f32 * fw) / iw).min((area.height as f32 * fh) / ih);
    let cols = (((iw * scale) / fw).round() as u16).clamp(1, area.width);
    let rows = (((ih * scale) / fh).round() as u16).clamp(1, area.height);
    Rect {
        x: area.x + area.width.saturating_sub(cols) / 2,
        y: area.y + area.height.saturating_sub(rows) / 2,
        width: cols,
        height: rows,
    }
}
