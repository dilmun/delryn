//! Build a [`StatusBar`] for each context — the reader, the library, and the
//! active overlay. State/context goes Left; reading fields and key hints go
//! Right. This is where the former `reader::render_status`, `library::status`,
//! and the overlay `legend` cascade now live, as segment producers.

use ratatui::style::{Modifier, Style};

use super::render::{GAUGE_WIDTH, gauge};
use super::segment::{StatusBar, Zone};
use crate::app::{App, EditMode, EditTab, LibView, MetaEdit, Overlay, Reader, SortKey};
use crate::config::Config;
use crate::store::LibrarySection;
use crate::theme::Theme;

/// The reader's bar: title/flash on the left; search, position, page, percent,
/// and the progress gauge on the right (each toggled by `config.status`).
pub fn reader_bar(reader: &Reader, config: &Config, theme: Theme) -> StatusBar {
    let fg = theme.status_fg;
    let bold = Style::default().fg(fg).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(fg).add_modifier(Modifier::DIM);
    let plain = Style::default().fg(fg);
    let mut bar = StatusBar::new();

    let meta = reader.doc.metadata();
    let left = if let Some(flash) = &reader.flash {
        flash.clone()
    } else if meta.authors.is_empty() {
        meta.title.clone()
    } else {
        format!("{} — {}", meta.title, meta.author_line())
    };
    bar.text(Zone::Left, 9, left, bold);

    let sf = config.status;
    if reader.search.matcher.is_some() {
        let n = reader.search_count();
        let cur = if n == 0 { 0 } else { reader.search.idx + 1 };
        bar.text(Zone::Right, 8, format!("⌕ {cur}/{n}"), plain);
    }
    if sf.theme {
        bar.text(Zone::Right, 3, theme.name, dim);
    }
    if sf.view {
        bar.text(Zone::Right, 3, config.view_mode.label(), dim);
    }
    if reader.paged || reader.is_paged_image() {
        // A paged-image (PDF) page is the section itself; reflowable page mode
        // counts virtual pages within the section.
        let (cur, total) = if reader.is_paged_image() {
            (reader.section + 1, reader.section_count())
        } else {
            (reader.current_page(), reader.page_count())
        };
        bar.text(Zone::Right, 7, format!("p {cur}/{total}"), plain);
    }
    if sf.position && !reader.is_paged_image() {
        bar.text(
            Zone::Right,
            6,
            format!("{}/{}", reader.section + 1, reader.doc.section_count()),
            plain,
        );
    }
    if sf.percent {
        let pct = (reader.progress() * 100.0).round() as u32;
        bar.text(Zone::Right, 5, format!("{pct}%"), plain);
    }
    if sf.gauge {
        bar.text(Zone::Right, 2, gauge(reader.progress(), GAUGE_WIDTH), dim);
    }
    bar
}

/// The library's bar: context/selection on the left (a pill for visual/marked
/// state); the context-sensitive key hints on the right.
pub fn library_bar(app: &App, theme: Theme) -> StatusBar {
    let fg = theme.status_fg;
    let bold = Style::default().fg(fg).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(fg).add_modifier(Modifier::DIM);
    let pill = Style::default()
        .fg(theme.on_accent())
        .bg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let mut bar = StatusBar::new();

    // The tag prompt owns the row while active.
    if let Overlay::TagEdit(t) = &app.overlay {
        let label = if t.multi {
            format!("Tag {} books", t.targets.len())
        } else {
            "Edit tags".to_string()
        };
        bar.text(Zone::Left, 9, format!("{label}: {}", t.input.text()), bold);
        bar.text(
            Zone::Right,
            2,
            "type · ←→ move · ^U clear · ⏎ save · Esc cancel",
            dim,
        );
        return bar;
    }

    let marked = app.library.marked.len();
    let visual = app.library.visual.is_some();
    if let Some(flash) = &app.library.flash {
        bar.text(Zone::Left, 9, flash.clone(), bold);
    } else if visual {
        bar.text(Zone::Left, 9, format!(" VISUAL · {marked} selected "), pill);
    } else if marked > 0 {
        bar.text(Zone::Left, 9, format!(" {marked} selected "), pill);
    } else if app.library.filtering || !app.library.filter.is_empty() {
        bar.text(Zone::Left, 9, format!("/{}", app.library.filter), bold);
    } else {
        let read = app.total_read_seconds();
        let sort = if app.library.sort == SortKey::Default {
            String::new()
        } else {
            format!(
                " · sort {} {}",
                app.library.sort.label(),
                if app.library.sort_desc { "↓" } else { "↑" }
            )
        };
        let pos = if app.library.books.is_empty() {
            String::new()
        } else {
            format!(
                "{}/{} · ",
                app.library.sel.min(app.library.books.len() - 1) + 1,
                app.library.books.len()
            )
        };
        let size = if app.is_grid() {
            format!(" · {} covers", app.config.library_grid_size.label())
        } else {
            String::new()
        };
        let state = format!(
            "{pos}{} · {}h{}m read{sort}{size}",
            app.library.view.label(),
            read / 3600,
            (read % 3600) / 60,
        );
        bar.text(Zone::Left, 9, state, bold);
    }

    let dups = matches!(
        app.library.view,
        LibView::Section(LibrarySection::Duplicates)
    );
    let keys = if visual {
        "j/k extend · space pick · e edit · r rename · T tag · f favorite · V/Esc cancel"
    } else if marked > 0 {
        "space/A pick · e edit · r rename · T tag · f favorite · c shelf · Esc clear"
    } else if dups {
        "hjkl move · ⏎ open · D resolve · R deep scan · I ignored · e edit · s sort · q"
    } else if app.is_grid() {
        "hjkl move · ⏎ open · e edit · T tag · D dedup · c shelf · s sort · +/- size · q"
    } else {
        "hjkl move · ⏎ open · e edit · r rename · T tag · D dedup · c shelf · s sort · q"
    };
    bar.text(Zone::Right, 2, keys, dim);
    bar
}

/// The active overlay's bar (context + key hints), if one is open. The bottom-row
/// prompt owns the row itself, so it's deliberately skipped.
pub fn overlay_bar(app: &App, theme: Theme) -> Option<StatusBar> {
    if matches!(app.overlay, Overlay::Prompt(_)) {
        return None;
    }
    let (context, keys) = legend(app)?;
    let fg = theme.status_fg;
    let mut bar = StatusBar::new();
    bar.text(
        Zone::Left,
        9,
        context,
        Style::default().fg(fg).add_modifier(Modifier::BOLD),
    );
    bar.text(
        Zone::Right,
        2,
        keys,
        Style::default().fg(fg).add_modifier(Modifier::DIM),
    );
    Some(bar)
}

/// The (context label, shortcut legend) for the active overlay, if any. Highest
/// to lowest precedence matches how `on_key` routes input.
fn legend(app: &App) -> Option<(String, String)> {
    // A pending yes/no confirmation is modal — it owns the bar above everything.
    if let Some(c) = &app.pending_confirm {
        return Some((c.question.clone(), "y/⏎ confirm · n/Esc cancel".into()));
    }
    if matches!(app.overlay, Overlay::Settings(_)) {
        return Some((
            "Settings".into(),
            "Tab section · ↑↓ move · ←→ change · Esc close".into(),
        ));
    }
    if let Overlay::ShelfPicker(p) = &app.overlay {
        let keys = if p.new_name.is_some() {
            "type a name · ⏎ create · Esc back"
        } else {
            "↑↓ move · ⏎ toggle / new · Esc close"
        };
        return Some(("Collections".into(), keys.into()));
    }
    if matches!(app.overlay, Overlay::Annot(_)) {
        return Some((
            "Bookmarks".into(),
            "↑↓ move · ⏎ jump · r name · f folder · d delete · Esc close".into(),
        ));
    }
    // The image viewer is fullscreen and shows its own shortcut footer, so it
    // doesn't use the shared status-row legend.
    if let Overlay::MetaEdit(ed) = &app.overlay {
        return Some(editor_legend(ed));
    }
    if let Overlay::BulkRename(br) = &app.overlay {
        let n = br.targets.len();
        let books = format!("book{}", if n == 1 { "" } else { "s" });
        return Some((
            format!("Rename · {n} {books}"),
            "type template · ^F full screen · ^U clear · ^S rename · Esc cancel".into(),
        ));
    }
    if let Overlay::CollEdit(e) = &app.overlay {
        return Some(if e.rename_from.is_some() {
            (
                "Rename collection".into(),
                "type · ←→ move · ^U clear · ⏎ save · empty ⏎ deletes · Esc cancel".into(),
            )
        } else {
            (
                "New collection".into(),
                "type a name · ←→ move · ^U clear · ⏎ create · Esc cancel".into(),
            )
        });
    }
    None
}

/// Context + shortcuts for the metadata editor, varying by tab and edit mode.
fn editor_legend(ed: &MetaEdit) -> (String, String) {
    let state = format!("Edit · {}", ed.tab.label());
    // The metadata-diff overlay owns the keys while it's open.
    if ed.diff.is_some() {
        return (
            "Apply metadata".to_string(),
            "j/k move · space toggle · a all · ⏎ apply · Esc cancel".to_string(),
        );
    }
    // The Lookup tab drives a structured seed form, with its own edit state.
    if ed.tab == EditTab::Online {
        let keys = if ed.lookup.editing {
            "type · ←→ move · ^U clear · ⏎ search · Esc done"
        } else {
            "1-3 tab · j/k move · ⏎ edit/apply · / search · ^S save · Esc"
        };
        return (state, keys.to_string());
    }
    let keys = if ed.cover_search.editing {
        "type to search · ←→ move · ^U clear · ⏎ run · Esc done"
    } else if ed.mode == EditMode::Edit {
        "type to edit · ←→ move · ^U clear · ⏎/Esc done"
    } else {
        match ed.tab {
            EditTab::Details => {
                "1-3 tab · j/k · ⏎ edit · x extract-from-book · r/R reset · ^S · Esc"
            }
            EditTab::Cover => "1-3 tab · / search · j/k pick · ⏎ use cover · ^S save · Esc",
            EditTab::Online => "", // handled above
        }
    };
    (state, keys.to_string())
}
