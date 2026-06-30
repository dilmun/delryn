//! Image viewer overlay (the `i` key): a figure sidebar, the selected figure
//! rendered large and centered, and its details. See `DESIGN.md` §18.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui_image::{FontSize, Resize, StatefulImage};

use crate::app::{App, Overlay};
use crate::theme::Role;

pub fn render(f: &mut Frame, app: &mut App) {
    let theme = app.config.theme;
    let policy = crate::media::RenderPolicy {
        tint: super::theme_ink(theme),
        mode: app.config.image_mode,
    };
    let mode_label = app.config.image_mode.label();
    // Disjoint field borrows: picker (read), viewer (mutate to build the proto),
    // reader (chapter titles).
    let App {
        picker,
        overlay,
        reader,
        ..
    } = app;
    let (Some(picker), Overlay::ImageView(viewer)) = (picker.as_ref(), overlay) else {
        return;
    };
    let reader = reader.as_ref();
    let font = picker.font_size();
    let bg = theme.paper();

    let area = f.area();
    f.render_widget(Clear, area);

    // The selected figure's details (read once; used by the title badge below
    // and the under-image strip).
    let (dims, caption, section) = match viewer.current() {
        Some(fig) => (fig.dims, fig.caption.clone(), fig.section),
        None => (None, String::new(), 0),
    };

    // Outer frame: position + scope, or a transient flash (e.g. "saved …").
    let (pos, count) = viewer.position();
    let scope = if viewer.whole_book { "book" } else { "chapter" };
    let title = match &viewer.flash {
        Some(flash) => format!(" {flash} "),
        None => format!(" Figures · {pos}/{count} · {scope} · {mode_label} "),
    };
    // The footer shows the active text prompt, else the shortcut legend.
    let footer = if viewer.filtering {
        format!(" filter: {} ", viewer.filter)
    } else if viewer.saving {
        format!(" save to: {} ", viewer.save_path)
    } else {
        " ↑↓ select · ⏎ go · / filter · w chapter/book · m mode · c copy · s save · Esc "
            .to_string()
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.style(Role::BorderFocus))
        .title(Span::styled(title, theme.style(Role::Title)))
        .title_bottom(Line::from(Span::styled(footer, theme.style(Role::Muted))))
        .style(theme.style(Role::Body).bg(bg));
    // Pixel dimensions as a right-aligned metadata badge in the top border.
    if let Some((w, h)) = dims {
        block = block.title(
            Line::from(Span::styled(
                format!(" {w}×{h}px "),
                theme.style(Role::Muted),
            ))
            .alignment(Alignment::Right),
        );
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Responsive: the figure list takes ~30% of the width and collapses on a
    // narrow screen so the figure keeps the room (shared app-standard split).
    let (sidebar, right) = super::sidebar_split(inner, 30, 24, 40, 50);

    // The book's own chapter label, made presentable: a bare number gets a
    // "Chapter " prefix; a label that already names the chapter is shown as-is
    // (so we never double "Chapter Chapter 7").
    let chapter_label = |sec: usize| -> String {
        let label = reader
            .and_then(|r| {
                r.outline
                    .iter()
                    .find(|e| e.section == sec)
                    .map(|e| e.label.clone())
            })
            .unwrap_or_default();
        let t = label.trim();
        if t.is_empty() {
            format!("§{}", sec + 1)
        } else if t.chars().all(|c| c.is_ascii_digit()) {
            format!("Chapter {t}")
        } else {
            t.to_string()
        }
    };

    // Sidebar: the filtered figure list (skipped when collapsed on a narrow pane).
    if let Some(sidebar) = sidebar {
        if viewer.is_empty() {
            f.render_widget(
                Paragraph::new(Line::styled("  No figures.", theme.style(Role::Muted))),
                sidebar,
            );
        } else {
            let items: Vec<ListItem> = viewer
                .visible()
                .map(|(_, fig)| {
                    Line::from(vec![
                        Span::styled(format!("§{} ", fig.section + 1), theme.style(Role::Muted)),
                        Span::styled(fig.name.clone(), theme.style(Role::Body)),
                    ])
                    .into()
                })
                .collect();
            let list = List::new(items).highlight_style(theme.style(Role::Selection));
            let mut st = ListState::default();
            st.select(Some(viewer.sel));
            f.render_stateful_widget(list, sidebar, &mut st);
        }
    }

    // Compose the image + its details as one block, centered in the right pane
    // with equal padding; the image scales (up or down) to fill the space above
    // the details, and the whole group is centered vertically.
    let avail = inset(right, 2);
    const DETAIL_H: u16 = 3;
    const GAP: u16 = 1;
    let img_space_h = avail.height.saturating_sub(DETAIL_H + GAP);
    let (iw, ih) = match dims {
        Some(d) => fit_size(avail.width, img_space_h, d, font),
        None => (avail.width.min(24), 1),
    };
    let group_h = (ih + GAP + DETAIL_H).min(avail.height);
    let top = avail.y + avail.height.saturating_sub(group_h) / 2;
    let img_rect = Rect {
        x: avail.x + avail.width.saturating_sub(iw) / 2,
        y: top,
        width: iw,
        height: ih,
    };
    let detail_rect = Rect {
        x: avail.x,
        y: img_rect.y.saturating_add(ih + GAP),
        width: avail.width,
        height: DETAIL_H,
    };

    // Details, centered under the image: the chapter, then the caption (only
    // when the figure has one — no synthetic "Figure N"). Dimensions live in the
    // title badge.
    let mut dlines = vec![Line::from(Span::styled(
        chapter_label(section),
        Style::default().fg(theme.color(Role::Heading)),
    ))];
    if !caption.is_empty() {
        dlines.push(Line::from(Span::styled(caption, theme.style(Role::Body))));
    }
    f.render_widget(
        Paragraph::new(dlines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .style(theme.style(Role::Body).bg(bg)),
        detail_rect,
    );

    // The image: Scale fills the slot (it upscales small figures; Fit would not).
    if dims.is_some()
        && let Some(proto) = viewer.ensure_proto(picker, policy)
    {
        f.render_stateful_widget(
            StatefulImage::default().resize(Resize::Scale(None)),
            img_rect,
            proto,
        );
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

/// The cell size an image scales to within `aw`×`ah` cells, preserving aspect
/// (up or down), given the terminal's pixels-per-cell `font`.
fn fit_size(aw: u16, ah: u16, dims: (u32, u32), font: FontSize) -> (u16, u16) {
    if aw == 0 || ah == 0 {
        return (aw.max(1), ah.max(1));
    }
    let (iw, ih) = (dims.0.max(1) as f32, dims.1.max(1) as f32);
    let (fw, fh) = (font.width.max(1) as f32, font.height.max(1) as f32);
    let scale = ((aw as f32 * fw) / iw).min((ah as f32 * fh) / ih);
    let cols = (((iw * scale) / fw).round() as u16).clamp(1, aw);
    let rows = (((ih * scale) / fh).round() as u16).clamp(1, ah);
    (cols, rows)
}
