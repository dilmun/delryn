//! The unified bottom status bar.
//!
//! One row, two zones: the **keys** (shortcut legend) on the left, dimmed, and
//! the **state** (active context / options) on the right, emphasised — so the
//! two read as distinct. Popups and overlays no longer draw their own in-window
//! footers — they contribute their context + shortcuts here via [`legend`],
//! which the main render overlays on the bottom row whenever one is open.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, EditMode, EditTab, MetaEdit};
use crate::theme::Theme;

/// Render `keys` (left, dimmed legend) and `state` (right, emphasised) into a
/// one-row status bar. The shared renderer for the library bar and popup bars.
pub fn bar(f: &mut Frame, area: Rect, theme: Theme, state: &str, keys: &str) {
    let width = area.width as usize;
    let state_w = state.chars().count();
    let keys_w = keys.chars().count();
    let pad = width.saturating_sub(state_w + keys_w + 2);
    let line = Line::from(vec![
        Span::styled(
            format!(" {keys}"),
            Style::default()
                .fg(theme.status_fg)
                .add_modifier(Modifier::DIM),
        ),
        Span::raw(" ".repeat(pad + 1)),
        Span::styled(
            format!("{state} "),
            Style::default()
                .fg(theme.status_fg)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(theme.status_bg)),
        area,
    );
}

/// If an overlay/popup is open, draw its (context, shortcuts) over the bottom
/// row. The note-entry prompt owns the row itself, so it's deliberately skipped.
pub fn overlay(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    if app.prompt.is_some() {
        return;
    }
    if let Some((state, keys)) = legend(app) {
        bar(f, area, theme, &state, &keys);
    }
}

/// The (context label, shortcut legend) for the active overlay, if any. Highest
/// to lowest precedence matches how `on_key` routes input.
fn legend(app: &App) -> Option<(String, String)> {
    // A pending yes/no confirmation is modal — it owns the bar above everything.
    if let Some(c) = &app.pending_confirm {
        return Some((c.question.clone(), "y/⏎ confirm · n/Esc cancel".into()));
    }
    if app.settings.is_some() {
        return Some(("Settings".into(), "↑↓ move · ←→ change · Esc close".into()));
    }
    if let Some(p) = &app.shelf_picker {
        let keys = if p.new_name.is_some() {
            "type a name · ⏎ create · Esc back"
        } else {
            "↑↓ move · ⏎ toggle / new · Esc close"
        };
        return Some(("Collections".into(), keys.into()));
    }
    if app.annot.is_some() {
        return Some((
            "Bookmarks".into(),
            "↑↓ move · ⏎ jump · r name · f folder · d delete · Esc close".into(),
        ));
    }
    // The image viewer is fullscreen and shows its own shortcut footer, so it
    // doesn't use the shared status-row legend.
    if let Some(ed) = &app.meta_edit {
        return Some(editor_legend(ed));
    }
    if let Some(br) = &app.bulk_rename {
        let n = br.targets.len();
        let books = format!("book{}", if n == 1 { "" } else { "s" });
        return Some((
            format!("Rename · {n} {books}"),
            "type template · ^F full screen · ^U clear · ^S rename · Esc cancel".into(),
        ));
    }
    if let Some(e) = &app.lib_coll_edit {
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
