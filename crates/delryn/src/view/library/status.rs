//! Library status bar.

use super::*;

pub(crate) fn render_status(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    // The tag prompt owns the row while active, showing the typed buffer.
    if let Overlay::TagEdit(t) = &app.overlay {
        let label = if t.multi {
            format!("Tag {} books", t.targets.len())
        } else {
            "Edit tags".to_string()
        };
        crate::view::status::bar(
            f,
            area,
            theme,
            &format!("{label}: {}", t.buf),
            "type · ←→ move · ^U clear · ⏎ save · Esc cancel",
        );
        return;
    }
    let marked = app.library.marked.len();
    let visual = app.library.visual.is_some();
    let state = if let Some(flash) = &app.library.flash {
        flash.clone()
    } else if visual {
        format!("VISUAL · {marked} selected")
    } else if marked > 0 {
        format!("{marked} selected")
    } else if app.library.filtering || !app.library.filter.is_empty() {
        format!("/{}", app.library.filter)
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
        format!(
            "{pos}{} · {}h{}m read{sort}{size}",
            app.library.view.label(),
            read / 3600,
            (read % 3600) / 60,
        )
    };
    // Selection (visual range or individual picks) gets bulk keys; grid (no
    // side panes) gets size keys, else the panes get </> resize. In the
    // Duplicates section, lead with the resolve key.
    let dups = matches!(
        app.library.view,
        LibView::Section(crate::store::LibrarySection::Duplicates)
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
    crate::view::status::bar(f, area, theme, &state, keys);
}
