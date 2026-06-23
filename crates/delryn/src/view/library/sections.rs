//! Library sidebar: smart sections + user collections (with inline editing).

use super::*;

/// One sidebar row (a fixed section or a collection). The active entry gets a
/// solid cursor highlight when the sidebar is focused, else just a marker.
fn section_item(label: &str, here: bool, focused: bool, theme: Theme) -> ListItem<'static> {
    let style = if here && focused {
        Style::default()
            .fg(theme.on_accent())
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else if here {
        let mut s = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD);
        if let Some(bg) = theme.bg {
            s = s.bg(bg);
        }
        s
    } else {
        let mut s = Style::default().fg(theme.fg);
        if let Some(bg) = theme.bg {
            s = s.bg(bg);
        }
        s
    };
    let marker = if here { "▸ " } else { "  " };
    ListItem::new(Line::from(Span::styled(format!("{marker}{label}"), style)))
}

pub(crate) fn render_sections(f: &mut Frame, area: Rect, app: &App, theme: Theme, focused: bool) {
    // The "＋ New" row parks the cursor without changing the active view, so a
    // section/collection isn't "here" while it's selected.
    let on_new = app.lib_side_new;
    let mut items: Vec<ListItem> = LibrarySection::ALL
        .iter()
        .map(|s| {
            let here = !on_new && matches!(&app.lib_view, LibView::Section(cur) if cur == s);
            section_item(s.label(), here, focused, theme)
        })
        .collect();

    // Collections, below a divider, each with its book count — always shown so
    // "＋ New collection" is reachable even before any collection exists.
    let mut header = Style::default().fg(theme.muted).add_modifier(Modifier::DIM);
    if let Some(bg) = theme.bg {
        header = header.bg(bg);
    }
    items.push(ListItem::new(Line::from(Span::styled(
        "  Collections",
        header,
    ))));
    // Which collection (if any) is being renamed in place.
    let renaming = app
        .lib_coll_edit
        .as_ref()
        .and_then(|e| e.rename_from.as_deref());
    let creating = app
        .lib_coll_edit
        .as_ref()
        .filter(|e| e.rename_from.is_none());
    // Value width inside the pane: drop the L/R border (2) and the "▸ " marker (2).
    let field_w = area.width.saturating_sub(4).max(2) as usize;
    for (name, count) in &app.lib_shelves {
        if Some(name.as_str()) == renaming {
            items.push(coll_edit_item(
                app.lib_coll_edit.as_ref().unwrap(),
                field_w,
                theme,
            ));
        } else {
            let here = !on_new && matches!(&app.lib_view, LibView::Shelf(cur) if cur == name);
            items.push(section_item(
                &format!("{name}  ({count})"),
                here,
                focused,
                theme,
            ));
        }
    }
    // The trailing "＋ New collection" row — an inline input while creating.
    if let Some(input) = creating {
        items.push(coll_edit_item(input, field_w, theme));
    } else {
        items.push(section_item("＋ New collection", on_new, focused, theme));
    }

    f.render_widget(
        List::new(items).block(pane_block("Library", focused, theme)),
        area,
    );
}

/// A sidebar row rendered as an inline text field (create / rename a
/// collection), with a block cursor at the caret. The value scrolls horizontally
/// within `width` cells so the caret stays visible for long names.
fn coll_edit_item(input: &crate::app::CollInput, width: usize, theme: Theme) -> ListItem<'static> {
    let mut spans = vec![Span::styled("▸ ", Style::default().fg(theme.accent))];
    spans.extend(crate::view::field_spans(
        &input.buf,
        input.cursor,
        width,
        theme,
    ));
    ListItem::new(Line::from(spans))
}
