//! The unified bottom status bar.
//!
//! One row, two zones: the **state** (active context / options) on the left and
//! the **keys** (shortcut legend) on the right, dimmed so the two read as
//! distinct. Popups and overlays no longer draw their own in-window footers —
//! they contribute their context + shortcuts here via [`legend`], which the main
//! render overlays on the bottom row whenever one is open.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, EditMode, EditTab, MetaEdit};
use crate::theme::Theme;

/// Render `state` (left, emphasised) and `keys` (right, dimmed legend) into a
/// one-row status bar. The shared renderer for the library bar and popup bars.
pub fn bar(f: &mut Frame, area: Rect, theme: Theme, state: &str, keys: &str) {
    let width = area.width as usize;
    let state_w = state.chars().count();
    let keys_w = keys.chars().count();
    let pad = width.saturating_sub(state_w + keys_w + 2);
    let line = Line::from(vec![
        Span::styled(
            format!(" {state}"),
            Style::default().fg(theme.status_fg).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(pad + 1)),
        Span::styled(
            format!("{keys} "),
            Style::default().fg(theme.status_fg).add_modifier(Modifier::DIM),
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
    if app.note_input.is_some() {
        return;
    }
    if let Some((state, keys)) = legend(app) {
        bar(f, area, theme, &state, &keys);
    }
}

/// The (context label, shortcut legend) for the active overlay, if any. Highest
/// to lowest precedence matches how `on_key` routes input.
fn legend(app: &App) -> Option<(String, String)> {
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
            "Annotations".into(),
            "↑↓ move · ⏎ jump · d delete · Esc close".into(),
        ));
    }
    if app.image_view.is_some() {
        return Some(("Images".into(), "n/N · h/l · ←→ page · i / q / Esc close".into()));
    }
    if let Some(ed) = &app.meta_edit {
        return Some(editor_legend(ed));
    }
    if let Some(br) = &app.bulk_rename {
        return Some((
            format!("Bulk rename · {} books", br.targets.len()),
            "type template · ←→ move · ^U clear · ^S rename all · Esc cancel".into(),
        ));
    }
    None
}

/// Context + shortcuts for the metadata editor, varying by tab and edit mode.
fn editor_legend(ed: &MetaEdit) -> (String, String) {
    let state = format!("Edit · {}", ed.tab.label());
    let keys = if ed.search().editing {
        "type to search · ←→ move · ^U clear · ⏎ run · Esc done"
    } else if ed.mode == EditMode::Edit {
        "type to edit · ←→ move · ^U clear · ⏎/Esc done"
    } else {
        match ed.tab {
            EditTab::Details => "Tab tab · j/k move · ⏎ edit · r/R reset · ^S save · Esc",
            EditTab::Cover => "Tab tab · / search · j/k pick · ⏎ use cover · ^S save · Esc",
            EditTab::Online => "Tab tab · / search · j/k pick · ⏎ apply · ^S save · Esc",
            EditTab::File => "Tab tab · j/k move · ⏎ edit · ^S rename + save · Esc",
        }
    };
    (state, keys.to_string())
}
