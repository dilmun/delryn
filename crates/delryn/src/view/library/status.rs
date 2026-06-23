//! Library status bar.

use super::*;

pub(crate) fn render_status(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    let marked = app.lib_marked.len();
    let visual = app.lib_visual.is_some();
    let state = if let Some(flash) = &app.lib_flash {
        flash.clone()
    } else if visual {
        format!("VISUAL · {marked} selected")
    } else if marked > 0 {
        format!("{marked} selected")
    } else if app.lib_filtering || !app.lib_filter.is_empty() {
        format!("/{}", app.lib_filter)
    } else {
        let read = app.total_read_seconds();
        let sort = if app.lib_sort == SortKey::Default {
            String::new()
        } else {
            format!(
                " · sort {} {}",
                app.lib_sort.label(),
                if app.lib_sort_desc { "↓" } else { "↑" }
            )
        };
        let pos = if app.lib_books.is_empty() {
            String::new()
        } else {
            format!(
                "{}/{} · ",
                app.lib_sel.min(app.lib_books.len() - 1) + 1,
                app.lib_books.len()
            )
        };
        let size = if app.is_grid() {
            format!(" · {} covers", app.config.library_grid_size.label())
        } else {
            String::new()
        };
        format!(
            "{pos}{} · {}h{}m read{sort}{size}",
            app.lib_view.label(),
            read / 3600,
            (read % 3600) / 60,
        )
    };
    // Selection (visual range or individual picks) gets bulk keys; grid (no
    // side panes) gets size keys, else the panes get </> resize.
    let keys = if visual {
        "j/k extend · space pick · e edit · r rename · f favorite · V/Esc cancel"
    } else if marked > 0 {
        "space/A pick · e edit · r rename · f favorite · c shelf · Esc clear"
    } else if app.is_grid() {
        "hjkl move · ⏎ open · space/V/A select · e edit · r rename · c shelf · s sort · v view · +/- size · q"
    } else {
        "hjkl move · ⏎ open · space/V/A select · e edit · r rename · c shelf · s sort · </> size · q"
    };
    crate::view::status::bar(f, area, theme, &state, keys);
}
