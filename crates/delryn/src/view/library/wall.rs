//! Library cover-wall view: an immersive, chrome-minimal grid of cover images.
//!
//! Unlike the card [`grid`](super::grid), covers sit borderless and edge-to-edge
//! (no per-card title, so more fit), the selection is an accent frame floating in
//! the gutter, and the selected book's details show in a single caption bar along
//! the bottom — the rest is covers.

use super::*;

/// Rows reserved at the bottom for the selected book's caption.
const CAPTION_H: u16 = 2;

pub(crate) fn render_wall(f: &mut Frame, area: Rect, app: &mut App, theme: Theme, focused: bool) {
    let block = pane_block("Books", focused, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    // No columns in a wall, so the `s` sort cycle follows all enabled keys.
    app.lib_sort_cycle = super::sort_cycle(&app.config, false, u16::MAX);
    if app.lib_books.is_empty() {
        let msg = if app.config.library_paths.is_empty() {
            "No library configured.\n\nAdd a folder:  delryn --add <dir>"
        } else {
            "No books in this section."
        };
        f.render_widget(Paragraph::new(msg).style(theme.text_style()), inner);
        return;
    }

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(CAPTION_H)]).split(inner);
    // Inset the cover area by one cell so the selection frame, which floats one
    // cell outside the selected cover, always has room (even at the edges).
    let wall = rows[0].inner(ratatui::layout::Margin::new(1, 1));
    let caption_area = rows[1];

    // Cover size from the configured grid size, shrunk to fit a narrow / short
    // pane (preserving aspect). Cell pitch adds a one-cell gutter the selection
    // frame floats in.
    let (cfg_w, cfg_h) = app.config.library_grid_size.card();
    let (mut cover_w, mut cover_h) = (cfg_w, cfg_h);
    let avail_w = wall.width.saturating_sub(1);
    if cover_w > avail_w {
        cover_w = avail_w.max(6);
        cover_h = ((cover_w as u32 * cfg_h as u32) / cfg_w.max(1) as u32).max(4) as u16;
    }
    let avail_h = wall.height.saturating_sub(1);
    if cover_h > avail_h {
        cover_h = avail_h.max(4);
        cover_w = ((cover_h as u32 * cfg_w as u32) / cfg_h.max(1) as u32).max(6) as u16;
    }
    let cell_w = cover_w + 1;
    let cell_h = cover_h + 1;

    let cols = (wall.width / cell_w).max(1) as usize;
    let rows_screen = (wall.height / cell_h).max(1) as usize;
    let len = app.lib_books.len();
    let sel = app.lib_sel.min(len - 1);

    // Center the cover block so leftover space becomes balanced margins.
    let block_w = (cols as u16 * cell_w).saturating_sub(1).min(wall.width);
    let block_h = (rows_screen as u16 * cell_h)
        .saturating_sub(1)
        .min(wall.height);
    let x0 = wall.x + (wall.width - block_w) / 2;
    let y0 = wall.y + (wall.height - block_h) / 2;

    // Keep the selected row centered in the viewport (clamped at the ends).
    let sel_row = sel / cols;
    let total_rows = len.div_ceil(cols);
    let max_top = total_rows.saturating_sub(rows_screen);
    let top_row = sel_row.saturating_sub(rows_screen / 2).min(max_top);
    let start = top_row * cols;
    let end = ((top_row + rows_screen) * cols).min(len);

    // Snapshot the visible cells (ends the immutable borrow before building).
    let visible: Vec<(usize, String, String, bool)> = (start..end)
        .map(|i| {
            let b = &app.lib_books[i];
            (
                i,
                b.path.clone(),
                b.title.clone(),
                app.lib_marked.contains(&b.path),
            )
        })
        .collect();
    let paths: Vec<String> = visible.iter().map(|(_, p, _, _)| p.clone()).collect();

    app.lib_grid_cols = cols;
    app.ensure_grid_covers(&paths, GRID_BUILD_PER_FRAME);
    let mut book_hits: Vec<(usize, Rect)> = Vec::with_capacity(visible.len());

    for (i, path, title, marked) in &visible {
        let pos = i - start;
        let (r, c) = ((pos / cols) as u16, (pos % cols) as u16);
        let card = Rect {
            x: x0 + c * cell_w,
            y: y0 + r * cell_h,
            width: cover_w,
            height: cover_h,
        };
        book_hits.push((*i, card));
        let selected = *i == sel;

        match app.lib_grid_covers.get_mut(path) {
            Some(Some(cover)) => {
                // Fill the whole cell, cropping the overflow (like CSS object-fit:
                // cover), so every cover is the same size — a clean, uniform wall
                // rather than ragged letterboxed thumbnails. The default crop
                // keeps the top-left, so a cover's title stays visible.
                let w = StatefulImage::default().resize(Resize::Crop(None));
                f.render_stateful_widget(w, card, &mut cover.proto);
            }
            // Coverless book: show its title (the only identifier here), centered.
            _ => {
                f.render_widget(
                    Paragraph::new(crate::view::truncate(title, (cover_w as usize) * 2))
                        .alignment(Alignment::Center)
                        .wrap(Wrap { trim: true })
                        .style(Style::default().fg(theme.muted)),
                    card,
                );
            }
        }

        // Selection / mark: a frame floating one cell outside the cover (in the
        // gutter), so the cover itself stays full-bleed. Accent for the cursor,
        // marker colour for a multi-select mark; unselected covers get no frame.
        let frame_color = if selected {
            Some(theme.accent)
        } else if *marked {
            Some(theme.marker)
        } else {
            None
        };
        if let Some(color) = frame_color {
            let ring = Rect {
                x: card.x.saturating_sub(1),
                y: card.y.saturating_sub(1),
                width: cover_w + 2,
                height: cover_h + 2,
            };
            f.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(color)),
                ring,
            );
        }
    }
    app.mouse.books = book_hits;

    render_caption(f, caption_area, &app.lib_books[sel], theme);
}

/// The bottom bar: the selected book's title and a metadata line.
fn render_caption(f: &mut Frame, area: Rect, b: &BookRow, theme: Theme) {
    if area.height == 0 {
        return;
    }
    let width = area.width as usize;
    let fav = if b.favorite { "★ " } else { "" };
    let title = Line::from(Span::styled(
        crate::view::truncate(&format!("{fav}{}", b.title), width),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    ));

    // Author · Series #N · FORMAT · rating · progress — non-empty parts only.
    let mut meta: Vec<String> = Vec::new();
    if !b.author.is_empty() {
        meta.push(b.author.clone());
    }
    if !b.series.is_empty() {
        meta.push(super::series_suffix(b).trim().to_string());
    }
    if let Some(ext) = std::path::Path::new(&b.path)
        .extension()
        .and_then(|e| e.to_str())
    {
        meta.push(ext.to_uppercase());
    }
    if b.rating > 0 {
        let r = b.rating.min(5) as usize;
        meta.push(format!("{}{}", "★".repeat(r), "☆".repeat(5 - r)));
    }
    if b.pct > 0 {
        meta.push(format!("{}%", b.pct));
    }
    let meta = Line::from(Span::styled(
        crate::view::truncate(&meta.join("  ·  "), width),
        Style::default().fg(theme.muted),
    ));

    f.render_widget(Paragraph::new(vec![title, meta]), area);
}
