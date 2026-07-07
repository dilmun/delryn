//! Library sidebar: smart sections + user collections (with inline editing).

use ratatui::widgets::ListState;

use super::*;

/// One sidebar row (a fixed section or a collection). The active entry gets a
/// solid cursor highlight when the sidebar is focused, else just a marker. When
/// `count` is given it's right-aligned to the pane edge so every group's total
/// lines up in a column; rows without one (e.g. "＋ New collection") render plain.
fn section_item(
    label: &str,
    count: Option<usize>,
    inner_w: usize,
    here: bool,
    focused: bool,
    theme: Theme,
) -> ListItem<'static> {
    let marker = if here { "▸ " } else { "  " };

    // Selected in the focused pane → a full-width rounded selection bar. Lay the
    // row out to `inner_w - 2` (the rounded caps take one cell each side), count
    // right-aligned, then let `rounded_line` cap it.
    if here && focused {
        let content_w = inner_w.saturating_sub(2);
        let text = match count {
            Some(c) => {
                let count = c.to_string();
                let cw = count.chars().count();
                let label = crate::view::truncate(label, content_w.saturating_sub(2 + cw + 1));
                let pad = content_w.saturating_sub(2 + label.chars().count() + cw);
                format!("{marker}{label}{}{count}", " ".repeat(pad))
            }
            None => format!(
                "{marker}{}",
                crate::view::truncate(label, content_w.saturating_sub(2))
            ),
        };
        return ListItem::new(crate::view::rounded_line(text, inner_w as u16, theme));
    }

    // Otherwise a flat row: accent text for the active (unfocused) entry, plain
    // body for the rest; the count dims to set the column apart.
    let mut style = if here {
        theme.style(Role::AccentStrong)
    } else {
        theme.style(Role::Body)
    };
    if let Some(bg) = theme.bg {
        style = style.bg(bg);
    }
    let Some(count) = count else {
        return ListItem::new(Line::from(Span::styled(format!("{marker}{label}"), style)));
    };
    let mut count_style = theme.style(Role::Muted);
    if let Some(bg) = theme.bg {
        count_style = count_style.bg(bg);
    }
    let count = count.to_string();
    let cw = count.chars().count();
    let label = crate::view::truncate(label, inner_w.saturating_sub(2 + cw + 1));
    let pad = inner_w.saturating_sub(2 + label.chars().count() + cw);
    ListItem::new(Line::from(vec![
        Span::styled(format!("{marker}{label}{}", " ".repeat(pad)), style),
        Span::styled(count, count_style),
    ]))
}

pub(crate) fn render_sections(
    f: &mut Frame,
    area: Rect,
    app: &mut App,
    theme: Theme,
    focused: bool,
) {
    // The "＋ New" row parks the cursor without changing the active view, so a
    // section/collection isn't "here" while it's selected.
    let on_new = app.library.side_new;
    // Usable text width inside the pane (drop the L/R border): the column the
    // per-group counts right-align to.
    let inner_w = area.width.saturating_sub(2) as usize;
    let n = LibrarySection::ALL.len();

    // (ring index, item index) for every clickable row — the map the click handler
    // reads back. `active_item` is the current row's item index; it drives the
    // list's scroll-into-view so the selection is always on screen (the sidebar can
    // outgrow its pane once enough collections exist).
    let mut row_meta: Vec<(usize, usize)> = Vec::new();
    let mut active_item: Option<usize> = None;
    let mut items: Vec<ListItem> = Vec::new();
    for (i, s) in LibrarySection::ALL.iter().enumerate() {
        let here = !on_new && matches!(&app.library.view, LibView::Section(cur) if cur == s);
        let count = app.library.section_counts.get(i).copied();
        if here {
            active_item = Some(items.len());
        }
        row_meta.push((i, items.len()));
        items.push(section_item(
            s.label(),
            count,
            inner_w,
            here,
            focused,
            theme,
        ));
    }

    // Collections, below a clear divider, each with its book count — always shown
    // so "＋ New collection" is reachable even before any collection exists. A
    // blank spacer + a full-width rule separates them from the built-in sections.
    let (mut rule, mut label) = (
        theme.style(Role::Muted),
        theme.style(Role::Body).add_modifier(Modifier::BOLD),
    );
    if let Some(bg) = theme.bg {
        rule = rule.bg(bg);
        label = label.bg(bg);
    }
    let fill = inner_w.saturating_sub("─ Collections ".chars().count());
    items.push(ListItem::new(Line::default()));
    items.push(ListItem::new(Line::from(vec![
        Span::styled("─ ", rule),
        Span::styled("Collections", label),
        Span::styled(format!(" {}", "─".repeat(fill)), rule),
    ])));
    // Which collection (if any) is being renamed in place.
    let renaming = match &app.overlay {
        Overlay::CollEdit(e) => e.rename_from.as_deref(),
        _ => None,
    };
    let creating = match &app.overlay {
        Overlay::CollEdit(e) if e.rename_from.is_none() => Some(e),
        _ => None,
    };
    // Value width inside the pane: drop the L/R border (2) and the "▸ " marker (2).
    let field_w = area.width.saturating_sub(4).max(2) as usize;
    for (j, (name, count)) in app.library.shelves.iter().enumerate() {
        // A row being renamed becomes an inline input, not a click target.
        if Some(name.as_str()) == renaming {
            if let Overlay::CollEdit(ci) = &app.overlay {
                items.push(coll_edit_item(ci, field_w, theme));
            }
            continue;
        }
        let here = !on_new && matches!(&app.library.view, LibView::Shelf(cur) if cur == name);
        if here {
            active_item = Some(items.len());
        }
        row_meta.push((n + j, items.len()));
        items.push(section_item(
            name,
            Some(*count),
            inner_w,
            here,
            focused,
            theme,
        ));
    }
    // The trailing "＋ New collection" row — an inline input while creating, else a
    // click target that begins one (ring index `lib_view_count()`).
    if let Some(input) = creating {
        items.push(coll_edit_item(input, field_w, theme));
    } else {
        if on_new {
            active_item = Some(items.len());
        }
        row_meta.push((n + app.library.shelves.len(), items.len()));
        items.push(section_item(
            "＋ New collection",
            None,
            inner_w,
            on_new,
            focused,
            theme,
        ));
    }

    // Stateful so the active row scrolls into view; the selection carries no visible
    // highlight (the rows style themselves), it only anchors the scroll offset.
    let block = pane_block("Library", focused, theme);
    let inner = block.inner(area);
    let mut state = ListState::default().with_selected(active_item);
    f.render_stateful_widget(List::new(items).block(block), area, &mut state);

    // Map each clickable row to its on-screen rect, using the offset the list
    // settled on (rows scrolled above the fold or past the bottom are dropped).
    let offset = state.offset();
    let mut hits: Vec<(usize, Rect)> = Vec::with_capacity(row_meta.len());
    for (ring, item_idx) in row_meta {
        if item_idx < offset {
            continue;
        }
        let sy = inner.y + (item_idx - offset) as u16;
        if sy >= inner.y + inner.height {
            continue;
        }
        hits.push((
            ring,
            Rect {
                x: inner.x,
                y: sy,
                width: inner.width,
                height: 1,
            },
        ));
    }
    app.mouse.side_rows = hits;
}

/// A sidebar row rendered as an inline text field (create / rename a
/// collection), with a block cursor at the caret. The value scrolls horizontally
/// within `width` cells so the caret stays visible for long names.
fn coll_edit_item(input: &crate::app::CollInput, width: usize, theme: Theme) -> ListItem<'static> {
    let mut spans = vec![Span::styled("▸ ", theme.style(Role::Accent))];
    spans.extend(crate::view::field_spans(
        input.input.text(),
        input.input.cursor(),
        width,
        theme,
    ));
    ListItem::new(Line::from(spans))
}
