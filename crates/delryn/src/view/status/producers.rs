//! Build a [`StatusBar`] for each context — the reader, the library, and the
//! active overlay. State/context goes Left; reading fields and key hints go
//! Right. This is where the former `reader::render_status`, `library::status`,
//! and the overlay `legend` cascade now live, as segment producers.

use ratatui::text::Span;

use super::render::{GAUGE_WIDTH, gauge};
use super::segment::{SegmentId, StatusBar, Zone};
use crate::app::{App, EditMode, EditTab, LibView, MetaEdit, Overlay, Reader, SortKey};
use crate::config::Config;
use crate::store::LibrarySection;
use crate::theme::{Role, Theme};

/// The reader's bar: title/flash on the left; search, position, page, percent,
/// and the progress gauge on the right (each toggled by `config.status`).
pub fn reader_bar(reader: &Reader, config: &Config, theme: Theme) -> StatusBar {
    let bold = theme.style(Role::StatusStrong);
    let dim = theme.style(Role::StatusDim);
    let plain = theme.style(Role::StatusText);
    let mut bar = StatusBar::new();

    let meta = reader.doc.metadata();
    let (left_id, left) = if let Some(flash) = &reader.flash {
        (SegmentId::Flash, flash.clone())
    } else if meta.authors.is_empty() {
        (SegmentId::Context, meta.title.clone())
    } else {
        (
            SegmentId::Context,
            format!("{} — {}", meta.title, meta.author_line()),
        )
    };
    bar.text(left_id, Zone::Left, 9, left, bold);

    let sf = &config.status;
    if reader.search.matcher.is_some() {
        let n = reader.search_count();
        let cur = if n == 0 { 0 } else { reader.search.idx + 1 };
        bar.text(
            SegmentId::Search,
            Zone::Right,
            8,
            format!("⌕ {cur}/{n}"),
            plain,
        );
    }
    if sf.theme {
        bar.text(SegmentId::Theme, Zone::Right, 3, theme.name, dim);
    }
    if sf.view {
        bar.text(
            SegmentId::View,
            Zone::Right,
            3,
            config.view_mode.label(),
            dim,
        );
    }
    // Continuous cross-section scroll indicator (reflow text or PDF page stacking).
    if reader.continuous_active() || reader.continuous_paged_active() {
        bar.text(SegmentId::Continuous, Zone::Right, 3, "continuous", dim);
    }
    // Manga (right-to-left) indicator — only meaningful for paged spreads.
    if reader.is_paged_image() && config.reading_direction.is_rtl() {
        bar.text(SegmentId::Manga, Zone::Right, 3, "manga ←", dim);
    }
    if reader.paged || reader.is_paged_image() {
        // A paged-image (PDF) page is the section itself; reflowable page mode
        // counts virtual pages within the section.
        let (cur, total) = if reader.is_paged_image() {
            (reader.section + 1, reader.section_count())
        } else {
            (reader.current_page(), reader.page_count())
        };
        bar.text(
            SegmentId::Page,
            Zone::Right,
            7,
            format!("p {cur}/{total}"),
            plain,
        );
    }
    // Zoom/fit indicator: the continuous stack's zoom, else a single page zoomed
    // off fit-page.
    if reader.continuous_paged_active() {
        if let Some(z) = reader.cont_zoom_label() {
            bar.text(SegmentId::Zoom, Zone::Right, 4, z, plain);
        }
    } else if reader.is_paged_image() && reader.page_view.is_zoomed() {
        bar.text(
            SegmentId::Zoom,
            Zone::Right,
            4,
            reader.page_view.label(),
            plain,
        );
    }
    if sf.position && !reader.is_paged_image() {
        bar.text(
            SegmentId::Position,
            Zone::Right,
            6,
            format!("{}/{}", reader.section + 1, reader.doc.section_count()),
            plain,
        );
    }
    if sf.percent {
        let pct = (reader.progress() * 100.0).round() as u32;
        bar.text(SegmentId::Percent, Zone::Right, 5, format!("{pct}%"), plain);
    }
    if sf.gauge {
        // Themed progress bar: the filled part takes the theme accent, the empty
        // track a muted colour — so it reads against any theme, not flat white.
        let (fill, track) = gauge(reader.progress(), GAUGE_WIDTH);
        bar.add(
            SegmentId::Gauge,
            Zone::Right,
            2,
            vec![
                Span::styled(fill, theme.style(Role::Accent)),
                Span::styled(track, dim),
            ],
        );
    }
    if sf.clock {
        bar.text(
            SegmentId::Clock,
            Zone::Right,
            4,
            super::clock::local_hhmm(),
            dim,
        );
    }
    bar
}

/// The library's bar: context/selection on the left (a pill for visual/marked
/// state); the context-sensitive key hints on the right.
pub fn library_bar(app: &App, theme: Theme) -> StatusBar {
    let bold = theme.style(Role::StatusStrong);
    let dim = theme.style(Role::StatusDim);
    let mut bar = StatusBar::new();

    let marked = app.library.marked.len();
    let visual = app.library.visual.is_some();
    if let Some(flash) = &app.library.flash {
        bar.text(SegmentId::Flash, Zone::Left, 9, flash.clone(), bold);
    } else if visual {
        bar.add(
            SegmentId::Context,
            Zone::Left,
            9,
            // Capped against the page, not a band: the bar floats now, so the
            // pill's rounded ends have the page behind them like every other pill.
            crate::view::pill_spans(format!("VISUAL · {marked} selected"), theme),
        );
    } else if marked > 0 {
        bar.add(
            SegmentId::Context,
            Zone::Left,
            9,
            crate::view::pill_spans(format!("{marked} selected"), theme),
        );
    } else if app.library.filtering || !app.library.filter.is_empty() {
        bar.text(
            SegmentId::Context,
            Zone::Left,
            9,
            format!("/{}", app.library.filter),
            bold,
        );
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
        bar.text(SegmentId::Context, Zone::Left, 9, state, bold);
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
    bar.text(SegmentId::Keys, Zone::Right, 2, keys, dim);
    bar
}

/// The active overlay's bar (context + key hints), if one is open.
pub fn overlay_bar(app: &App, theme: Theme) -> Option<StatusBar> {
    let (context, keys) = legend(app)?;
    let mut bar = StatusBar::new();
    bar.text(
        SegmentId::Context,
        Zone::Left,
        9,
        context,
        theme.style(Role::StatusStrong),
    );
    bar.text(
        SegmentId::Keys,
        Zone::Right,
        2,
        keys,
        theme.style(Role::StatusDim),
    );
    Some(bar)
}

/// The (context label, shortcut legend) for the active overlay, if any. Highest
/// to lowest precedence matches how `on_key` routes input.
fn legend(app: &App) -> Option<(String, String)> {
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
    if matches!(app.overlay, Overlay::WordLookup(_)) {
        return Some(("Look up".into(), "j/k scroll · d/u page · Esc close".into()));
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

#[cfg(test)]
mod tests {
    /// The status bar reports state; it never hosts a prompt. Confirmations, tag
    /// editing, the library filter, and in-book search are modals now (see
    /// `view::dialog`) — a prompt down here was missed, and its text entry
    /// silently swallowed every shortcut. This pins the rule against a relapse.
    #[test]
    fn no_prompt_text_is_produced_for_the_status_bar() {
        // Scan only the module's real code: this test's own needles live below.
        let full = include_str!("producers.rs");
        let src = full.split("#[cfg(test)]").next().unwrap_or(full);
        for needle in [
            "pending_confirm",
            "Overlay::TagEdit",
            "confirm · n/Esc",
            "⏎ save · Esc cancel",
        ] {
            assert!(
                !src.contains(needle),
                "{needle:?} is prompt UI and belongs in view::dialog, not the status bar"
            );
        }
    }
}
