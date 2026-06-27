//! Library cover-grid view.

use super::*;

/// Cover-grid view: cards of cover thumbnails (built lazily) reflowing to width,
/// with the title under each and the selection framed in the accent colour.
pub(crate) fn render_grid(f: &mut Frame, area: Rect, app: &mut App, theme: Theme, focused: bool) {
    let block = pane_block("Books", focused, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let area = inner;
    // The grid has no columns, so the `s` sort cycle follows all enabled ones.
    app.lib_sort_cycle = super::sort_cycle(&app.config, false, u16::MAX);
    if app.lib_books.is_empty() {
        let msg = if app.config.library_paths.is_empty() {
            "No library configured.\n\nAdd a folder:  delryn --add <dir>"
        } else {
            "No books in this section."
        };
        f.render_widget(Paragraph::new(msg).style(theme.text_style()), area);
        return;
    }

    // Card size from the configured grid size, shrunk to fit a narrow / short
    // pane (preserving the card aspect) so a card never overflows — the cover
    // image scales down with it. Cell pitch adds a 1-cell gutter.
    let (cfg_w, cfg_h) = app.config.library_grid_size.card();
    let (mut cover_w, mut cover_h) = (cfg_w, cfg_h);
    let avail_w = area.width.saturating_sub(1);
    if cover_w > avail_w {
        cover_w = avail_w.max(6);
        cover_h = ((cover_w as u32 * cfg_h as u32) / cfg_w.max(1) as u32).max(4) as u16;
    }
    let avail_h = area.height.saturating_sub(LABEL_H + 1);
    if cover_h > avail_h {
        cover_h = avail_h.max(4);
        cover_w = ((cover_h as u32 * cfg_w as u32) / cfg_h.max(1) as u32).max(6) as u16;
    }
    let cell_w = cover_w + 1;
    let cell_h = cover_h + LABEL_H + 1;

    let cols = (area.width / cell_w).max(1) as usize;
    let rows_screen = (area.height / cell_h).max(1) as usize;
    let len = app.lib_books.len();
    let sel = app.lib_sel.min(len - 1);

    // Center the card block within the pane so the leftover width/height becomes
    // balanced margins instead of one big gap on the right / bottom. The trailing
    // gutter (cell pitch − card) is trimmed off each edge before centering.
    let block_w = (cols as u16 * cell_w).saturating_sub(1).min(area.width);
    let block_h = (rows_screen as u16 * cell_h)
        .saturating_sub(1)
        .min(area.height);
    let x0 = area.x + (area.width - block_w) / 2;
    let y0 = area.y + (area.height - block_h) / 2;

    // Keep the selected row centered in the viewport (clamped at the ends),
    // like the sidebar — rather than pinning it to the bottom edge.
    let sel_row = sel / cols;
    let total_rows = len.div_ceil(cols);
    let max_top = total_rows.saturating_sub(rows_screen);
    let top_row = sel_row.saturating_sub(rows_screen / 2).min(max_top);
    let start = top_row * cols;
    let end = ((top_row + rows_screen) * cols).min(len);

    // Snapshot the visible cells (ends the immutable borrow before building).
    let visible: Vec<(usize, String, String, bool, bool)> = (start..end)
        .map(|i| {
            let b = &app.lib_books[i];
            let marked = app.lib_marked.contains(&b.path);
            (i, b.path.clone(), b.title.clone(), b.favorite, marked)
        })
        .collect();
    let paths: Vec<String> = visible.iter().map(|(_, p, _, _, _)| p.clone()).collect();

    app.lib_grid_cols = cols;
    app.ensure_grid_covers(&paths, GRID_BUILD_PER_FRAME);
    let mut book_hits: Vec<(usize, Rect)> = Vec::with_capacity(visible.len());

    for (i, path, title, fav, marked) in &visible {
        let pos = i - start;
        let (r, c) = ((pos / cols) as u16, (pos % cols) as u16);
        let x = x0 + c * cell_w;
        let y = y0 + r * cell_h;
        let card = Rect {
            x,
            y,
            width: cover_w,
            height: cover_h,
        };
        // Title sits on a single row so the selection highlight covers only the
        // text, not the blank gutter row below it; LABEL_H still spaces the cells.
        let label = Rect {
            x,
            y: y + cover_h,
            width: cover_w,
            height: 1,
        };
        // Whole cell (cover + title) is the click target for this book.
        book_hits.push((
            *i,
            Rect {
                x,
                y,
                width: cover_w,
                height: cover_h + LABEL_H,
            },
        ));
        let selected = *i == sel;

        // Card frame — rounded; accent border for the cursor, marker colour for a
        // multi-select mark, else quiet. The file format sits in the top-left.
        let border = if selected {
            theme.accent
        } else if *marked {
            theme.marker
        } else {
            theme.muted
        };
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_uppercase)
            .unwrap_or_default();
        let mut frame = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border))
            .style(theme.text_style());
        if !ext.is_empty() {
            frame = frame.title(Line::from(vec![
                Span::styled("─", Style::default().fg(border)),
                Span::styled(
                    ext,
                    Style::default()
                        .fg(if selected { theme.accent } else { theme.fg })
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        let inner = frame.inner(card);
        f.render_widget(frame, card);

        match app.lib_grid_covers.get_mut(path) {
            Some(Some(cover)) => {
                // Stretch to fill the card (object-fit: fill) so covers are
                // uniform; the rounded frame + badge + title frame each one.
                let w = StatefulImage::default().resize(Resize::Stretch(None));
                f.render_stateful_widget(w, inner, &mut cover.proto);
            }
            _ => {
                let star = if *fav { "★\n" } else { "" };
                f.render_widget(
                    Paragraph::new(format!("{star}\nno\ncover"))
                        .alignment(Alignment::Center)
                        .style(Style::default().fg(theme.muted)),
                    inner,
                );
            }
        }

        let style = if selected {
            Style::default().fg(theme.on_accent()).bg(theme.accent)
        } else {
            Style::default().fg(theme.fg)
        };
        let check = if *marked { "✓ " } else { "" };
        let star = if *fav { "★ " } else { "" };
        f.render_widget(
            Paragraph::new(crate::view::truncate(
                &format!("{check}{star}{title}"),
                cover_w as usize,
            ))
            .style(style),
            label,
        );
    }
    app.mouse.books = book_hits;
}
