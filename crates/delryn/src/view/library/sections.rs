//! Library sidebar: smart sections + user collections (with inline editing).

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
    let Some(count) = count else {
        return ListItem::new(Line::from(Span::styled(format!("{marker}{label}"), style)));
    };
    // The count keeps the row style while active (so it rides the selection bar),
    // else dims to set the column apart. The label fills the gap and truncates
    // before it would collide with the count.
    let count_style = if here && focused {
        style
    } else {
        let mut s = Style::default().fg(theme.muted);
        if let Some(bg) = theme.bg {
            s = s.bg(bg);
        }
        s
    };
    let count = count.to_string();
    let cw = count.chars().count();
    let label = crate::view::truncate(label, inner_w.saturating_sub(2 + cw + 1));
    let pad = inner_w.saturating_sub(2 + label.chars().count() + cw);
    ListItem::new(Line::from(vec![
        Span::styled(format!("{marker}{label}{}", " ".repeat(pad)), style),
        Span::styled(count, count_style),
    ]))
}

pub(crate) fn render_sections(f: &mut Frame, area: Rect, app: &App, theme: Theme, focused: bool) {
    // The "＋ New" row parks the cursor without changing the active view, so a
    // section/collection isn't "here" while it's selected.
    let on_new = app.library.side_new;
    // Usable text width inside the pane (drop the L/R border): the column the
    // per-group counts right-align to.
    let inner_w = area.width.saturating_sub(2) as usize;
    let mut items: Vec<ListItem> = LibrarySection::ALL
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let here = !on_new && matches!(&app.library.view, LibView::Section(cur) if cur == s);
            let count = app.library.section_counts.get(i).copied();
            section_item(s.label(), count, inner_w, here, focused, theme)
        })
        .collect();

    // Collections, below a clear divider, each with its book count — always shown
    // so "＋ New collection" is reachable even before any collection exists. A
    // blank spacer + a full-width rule separates them from the built-in sections.
    let (mut rule, mut label) = (
        Style::default().fg(theme.muted),
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
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
    for (name, count) in &app.library.shelves {
        if Some(name.as_str()) == renaming {
            items.push(coll_edit_item(
                app.lib_coll_edit.as_ref().unwrap(),
                field_w,
                theme,
            ));
        } else {
            let here = !on_new && matches!(&app.library.view, LibView::Shelf(cur) if cur == name);
            items.push(section_item(
                name,
                Some(*count),
                inner_w,
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
        items.push(section_item(
            "＋ New collection",
            None,
            inner_w,
            on_new,
            focused,
            theme,
        ));
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
