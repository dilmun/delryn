//! Fullscreen code-block browser: a sidebar of the chapter's (or book's) code
//! blocks with a sticky chapter header, and the selected block syntax-highlighted
//! and scrollable. Presentation only — highlighting/wrapping reuse the reader's
//! layout so colours match.

use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{App, CodeFocus, Overlay};
use crate::document::Block;
use crate::layout::{WrapOpts, wrap_blocks};
use crate::theme::Role;

pub fn render(f: &mut Frame, app: &mut App) {
    let theme = app.config.theme;
    let bold = app.config.bold_borders;
    // Chapter names come from the reader's outline (disjoint field from `overlay`).
    let reader = app.reader.as_ref();

    let Overlay::CodeView(cv) = &mut app.overlay else {
        return;
    };
    // Fullscreen — fill the whole screen (like the image viewer), so it's just the
    // view and its own footer.
    let area = f.area();
    f.render_widget(Clear, area);

    let (n, total) = cv.position();
    let scope = if cv.whole_book {
        "whole book"
    } else {
        "chapter"
    };
    let on_list = cv.focus == CodeFocus::Sidebar;
    let footer = if cv.copied {
        " copied ✓ · Tab focus · ⏎ go · w scope · Esc close "
    } else {
        " Tab focus · j/k nav · ⏎ go · w scope · y copy · Esc close "
    };
    let panel = super::overlay_frame(theme, bold)
        .title(Span::styled(
            format!(" Code · {n}/{total} · {scope} "),
            theme.style(Role::Title),
        ))
        .title_bottom(Line::from(Span::styled(footer, theme.style(Role::Muted))))
        .title_alignment(Alignment::Center)
        .style(theme.text_style());
    let inner = panel.inner(area);
    f.render_widget(panel, area);

    // The code list takes ~34% (shared app-standard split), collapsing when narrow.
    let (sidebar, right) = super::sidebar_split(inner, 34, 24, 52, 40);

    if let Some(sidebar) = sidebar {
        let items: Vec<(usize, &str)> = cv
            .visible()
            .map(|(_, s)| (s.section, s.label.as_str()))
            .collect();
        super::grouped_sidebar(f, sidebar, &items, cv.sel, on_list, reader, theme);
    }

    // Right pane: the selected block, re-highlighted + wrapped to its width.
    let Some((lang, source)) = cv.current().map(|s| (s.lang.clone(), s.lines.clone())) else {
        return;
    };
    let opts = WrapOpts {
        width: (right.width as usize).max(1),
        code_theme: theme.code_syntect(),
        code_wrap: true,
        code_line_numbers: true,
        code_label: false,
        ..Default::default()
    };
    let block = Block::Code {
        lang,
        lines: source,
    };
    let dlines = wrap_blocks(&[block], &opts, &[]);
    let lines = super::reader::plain_lines(&dlines, theme);
    let max_scroll = (lines.len() as u16).saturating_sub(right.height);
    cv.scroll = cv.scroll.min(max_scroll);
    f.render_widget(Paragraph::new(lines).scroll((cv.scroll, 0)), right);
}
