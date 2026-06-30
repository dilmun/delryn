//! Library cover-grid view: an immersive grid of cover images.
//!
//! Covers stretch to fill identical cells (rounded by [`media::build_cover`]),
//! the cursor and multi-select marks are frames floating in the gutter (so the
//! cover stays full-bleed), and the selected book's details show in a single
//! caption bar along the bottom.

use super::*;

/// Rows reserved at the bottom for the selected book's caption.
const CAPTION_H: u16 = 2;

pub(crate) fn render_grid(f: &mut Frame, area: Rect, app: &mut App, theme: Theme, focused: bool) {
    let block = pane_block("Books", focused, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    // No columns in the grid, so the `s` sort cycle follows all enabled keys.
    app.library.sort_cycle = super::sort_cycle(&app.config, false, u16::MAX);
    if app.library.books.is_empty() {
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
    let len = app.library.books.len();
    let sel = app.library.sel.min(len - 1);

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
            let b = &app.library.books[i];
            (
                i,
                b.path.clone(),
                b.title.clone(),
                app.library.marked.contains(&b.path),
            )
        })
        .collect();
    // Cover build list: the visible cells first, then a screenful ahead in the
    // travel direction — so idle frames pre-build the covers a held j/k is about
    // to scroll into, instead of building them only once they're on screen.
    let mut paths: Vec<String> = visible.iter().map(|(_, p, _, _)| p.clone()).collect();
    let ahead = rows_screen * cols;
    if app.library.nav_down {
        paths.extend((end..(end + ahead).min(len)).map(|i| app.library.books[i].path.clone()));
    } else {
        paths.extend(
            (start.saturating_sub(ahead)..start)
                .rev()
                .map(|i| app.library.books[i].path.clone()),
        );
    }

    app.library.grid_cols = cols;
    app.library.visible_rows = rows_screen;
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

        match app.library.grid_covers.get_mut(path) {
            Some(Some(cover)) => {
                // Stretch the cover to exactly fill the cell (object-fit: fill),
                // so every thumbnail is the same size with no gaps. The corners
                // are pre-rounded in the cover image itself.
                let w = StatefulImage::default().resize(Resize::Stretch(None));
                f.render_stateful_widget(w, card, &mut cover.proto);
            }
            // Coverless book: a default placeholder card — a rounded frame (to
            // match the rounded covers) with a book glyph and the title centred,
            // so it reads as a cover rather than empty space.
            _ => {
                let frame = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(theme.style(Role::Muted));
                let inner = frame.inner(card);
                f.render_widget(frame, card);
                let pad = (inner.height / 2).saturating_sub(2) as usize;
                let body = format!(
                    "{}▢\n\n{}",
                    "\n".repeat(pad),
                    crate::view::truncate(title, (inner.width as usize) * 3)
                );
                f.render_widget(
                    Paragraph::new(body)
                        .alignment(Alignment::Center)
                        .wrap(Wrap { trim: true })
                        .style(theme.style(Role::Muted)),
                    inner,
                );
            }
        }

        // Selection / mark: a frame floating one cell outside the cover (in the
        // gutter), so the cover itself stays full-bleed. Accent for the cursor,
        // marker colour for a multi-select mark; unselected covers get no frame.
        let frame_color = if selected {
            Some(theme.color(Role::BorderFocus))
        } else if *marked {
            Some(theme.color(Role::Marker))
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

    render_caption(f, caption_area, &app.library.books[sel], theme);
}

/// The bottom bar: the selected book's title and a metadata line (so the grid
/// itself stays all covers — the format/author/etc. live here).
fn render_caption(f: &mut Frame, area: Rect, b: &BookRow, theme: Theme) {
    if area.height == 0 {
        return;
    }
    let width = area.width as usize;
    let fav = if b.favorite { "★ " } else { "" };
    let title = Line::from(Span::styled(
        crate::view::truncate(&format!("{fav}{}", b.title), width),
        theme.style(Role::AccentStrong),
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
        theme.style(Role::Muted),
    ));

    f.render_widget(Paragraph::new(vec![title, meta]), area);
}
