//! Library detail pane: cover + full metadata for the selected book.

use super::*;

/// Right-hand pane: the selected book's cover (via the image protocol) plus its
/// full metadata.
pub(crate) fn render_detail(f: &mut Frame, area: Rect, app: &mut App, theme: Theme, focused: bool) {
    let block = pane_block("Details", focused, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Gather the selected book's fields (immutable) before the mutable cover render.
    let Some(b) = app.library.books.get(app.library.sel) else {
        return;
    };
    let title = b.title.clone();
    let subtitle = b.subtitle.clone();
    let author = b.author.clone();
    let series = series_suffix(b).trim_start().to_string();
    let year = b.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into());
    let publisher = b.publisher.clone();
    let isbn = b.isbn.clone();
    let language = b.language.clone();
    let size = fmt_size(b.size);
    let pct = b.pct;
    let fav = b.favorite;
    let converted = b.converted;
    let rating = b.rating;
    let status = b.status.clone();
    let tags = b.tags.clone();

    let parts = Layout::vertical([Constraint::Min(2), Constraint::Length(13)]).split(inner);

    // Cover (or a fallback box when there's none / no graphics protocol).
    let font = crate::view::image_font(app);
    if let Some(cover) = app.library.cover.as_mut() {
        let rect = crate::view::cover_image_rect(parts[0], font, cover.dims);
        let img = StatefulImage::default().resize(Resize::Scale(None));
        f.render_stateful_widget(img, rect, &mut cover.proto);
    } else {
        let ph = Paragraph::new("\n  (no cover)")
            .style(theme.style(Role::Muted))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(theme.style(Role::Muted)),
            );
        f.render_widget(ph, parts[0]);
    }

    // Metadata.
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                if fav { "★ " } else { "" },
                Style::default().fg(theme.color(Role::Marker)),
            ),
            Span::styled(title, theme.style(Role::Body).add_modifier(Modifier::BOLD)),
        ]),
        Line::styled(author, theme.style(Role::Muted)),
    ];
    if !subtitle.is_empty() {
        lines.push(Line::styled(
            subtitle,
            theme.style(Role::Muted).add_modifier(Modifier::ITALIC),
        ));
    }
    lines.push(Line::raw(""));
    if !series.is_empty() {
        lines.push(meta_kv("Series", &series, theme));
    }
    lines.push(meta_kv("Year", &year, theme));
    if rating > 0 {
        let stars = format!(
            "{}{}",
            "★".repeat(rating as usize),
            "☆".repeat(5 - rating as usize)
        );
        lines.push(meta_kv("Rating", &stars, theme));
    }
    if !publisher.is_empty() {
        lines.push(meta_kv("Publisher", &publisher, theme));
    }
    if !language.is_empty() {
        lines.push(meta_kv("Language", &language, theme));
    }
    if !isbn.is_empty() {
        lines.push(meta_kv("ISBN", &isbn, theme));
    }
    lines.push(meta_kv("Size", &size, theme));
    lines.push(Line::from(vec![
        Span::styled("Source: ", theme.style(Role::Muted)),
        Span::styled(
            if converted {
                "Converted EPUB"
            } else {
                "Original EPUB"
            },
            Style::default().fg(if converted {
                theme.color(Role::Marker)
            } else {
                theme.color(Role::Body)
            }),
        ),
    ]));
    lines.push(meta_kv("Progress", &format!("{pct}%"), theme));
    let st = delryn_model::ReadingStatus::effective(pct, &status);
    lines.push(meta_kv(
        "Status",
        format!("{} {}", st.badge(), st.label()).trim(),
        theme,
    ));
    if !tags.is_empty() {
        lines.push(meta_kv("Tags", &tags, theme));
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(theme.text_style()),
        parts[1],
    );
}

/// A `key: value` metadata line.
fn meta_kv(key: &str, val: &str, theme: Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key}: "), theme.style(Role::Muted)),
        Span::styled(val.to_string(), theme.style(Role::Body)),
    ])
}
