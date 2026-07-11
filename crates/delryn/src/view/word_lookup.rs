//! Word-lookup overlay: the dictionary definition + Wikipedia summary for the
//! `K`-selected term, in a scrollable read-only panel. Presentation only — the
//! content model comes from `delryn-online`; this just lays it out and themes it.

use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, LookupState, Overlay, WordLookup};
use crate::online::LookupResult;
use crate::theme::{Role, Theme};

pub fn render(f: &mut Frame, app: &mut App) {
    let theme = app.config.theme;
    let any_source =
        app.config.lookup_sdcv || app.config.lookup_dictionary || app.config.lookup_wikipedia;
    let large = app.overlay_large;
    let area = super::overlay_rect(f.area(), large);
    let inner_w = area.width.saturating_sub(2) as usize;
    let inner_h = area.height.saturating_sub(2);

    let Overlay::WordLookup(wl) = &mut app.overlay else {
        return;
    };
    f.render_widget(Clear, area);

    let lines = build(wl, theme, inner_w, any_source);
    // Clamp scroll so `G`/overscroll can't drop the content off the top.
    let max_scroll = (lines.len() as u16).saturating_sub(inner_h);
    wl.scroll = wl.scroll.min(max_scroll);

    let title = super::truncate(&wl.word, inner_w.saturating_sub(6));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.style(Role::BorderFocus))
        .title(Span::styled(
            format!(" 📖 {title} "),
            theme.style(Role::Title),
        ))
        .title_bottom(Line::from(Span::styled(
            " j/k scroll · Esc close ",
            theme.style(Role::Muted),
        )))
        .title_alignment(Alignment::Center)
        .style(theme.text_style());
    f.render_widget(
        Paragraph::new(lines).block(block).scroll((wl.scroll, 0)),
        area,
    );
}

/// Lay the lookup state out as styled, already-wrapped lines (so scroll is exact).
/// `any_source` is whether at least one lookup source is enabled — its absence is
/// why an empty result came back, so the message points at Settings.
fn build(wl: &WordLookup, theme: Theme, width: usize, any_source: bool) -> Vec<Line<'static>> {
    let muted = theme.style(Role::Muted);
    let body = theme.style(Role::Body);
    let heading = theme.style(Role::Hint).add_modifier(Modifier::BOLD);
    let example = theme.style(Role::Muted).add_modifier(Modifier::ITALIC);

    match &wl.state {
        LookupState::Fetching => vec![Line::raw(""), indented(" Looking up…", muted)],
        LookupState::Ready(result) if result.is_empty() => {
            let msg = if any_source {
                format!(" No dictionary or Wikipedia entry for “{}”.", wl.word)
            } else {
                " No lookup sources are enabled (Settings ▸ Lookup).".to_string()
            };
            vec![Line::raw(""), indented(msg, muted)]
        }
        LookupState::Ready(result) => {
            let mut lines = Vec::new();
            render_definition(
                result, &mut lines, width, theme, heading, body, muted, example,
            );
            render_wiki(result, &mut lines, width, heading, body, muted);
            lines
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_definition(
    result: &LookupResult,
    lines: &mut Vec<Line<'static>>,
    width: usize,
    theme: Theme,
    heading: Style,
    body: Style,
    muted: Style,
    example: Style,
) {
    let Some(def) = &result.definition else {
        return;
    };
    // Phonetic (if any) plus the source, on the first line.
    let mut top = Vec::new();
    if let Some(ph) = &def.phonetic {
        top.push(Span::styled(format!(" {ph}"), theme.style(Role::Heading)));
        top.push(Span::styled(format!("   · {}", def.source), muted));
    } else {
        top.push(Span::styled(format!(" {}", def.source), muted));
    }
    lines.push(Line::from(top));

    for meaning in &def.meanings {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!(" {}", meaning.label),
            heading,
        )));
        for (i, item) in meaning.items.iter().enumerate() {
            let prefix = format!("  {}. ", i + 1);
            let indent = prefix.chars().count();
            for (row, seg) in wrap(&item.text, width.saturating_sub(indent))
                .into_iter()
                .enumerate()
            {
                let lead = if row == 0 {
                    prefix.clone()
                } else {
                    " ".repeat(indent)
                };
                lines.push(Line::from(vec![
                    Span::styled(lead, muted),
                    Span::styled(seg, body),
                ]));
            }
            if let Some(ex) = &item.example {
                for seg in wrap(&format!("“{ex}”"), width.saturating_sub(indent)) {
                    lines.push(Line::from(Span::styled(
                        format!("{}{}", " ".repeat(indent), seg),
                        example,
                    )));
                }
            }
        }
    }
}

fn render_wiki(
    result: &LookupResult,
    lines: &mut Vec<Line<'static>>,
    width: usize,
    heading: Style,
    body: Style,
    muted: Style,
) {
    let Some(w) = &result.wiki else {
        return;
    };
    if result.definition.is_some() {
        lines.push(Line::raw(""));
    }
    lines.push(Line::from(Span::styled(" Wikipedia", heading)));
    if let Some(desc) = &w.description {
        for seg in wrap(desc, width.saturating_sub(1)) {
            lines.push(indented(format!(" {seg}"), muted));
        }
    }
    for seg in wrap(&w.extract, width.saturating_sub(1)) {
        lines.push(indented(format!(" {seg}"), body));
    }
}

/// A single-span line (small readability helper).
fn indented(text: impl Into<String>, style: Style) -> Line<'static> {
    Line::from(Span::styled(text.into(), style))
}

/// Greedy word-wrap `text` to `width` display columns. Over-long single words are
/// left intact (rare in prose); an empty input yields one empty line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for word in text.split_whitespace() {
        let ww = UnicodeWidthStr::width(word);
        if cur.is_empty() {
            cur.push_str(word);
            cur_w = ww;
        } else if cur_w + 1 + ww <= width {
            cur.push(' ');
            cur.push_str(word);
            cur_w += 1 + ww;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
            cur_w = ww;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}
