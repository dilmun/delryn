//! Pack a [`StatusBar`] into the row: Left segments left-aligned, Right
//! right-aligned, Center centred. When the segments don't fit, drop the
//! lowest-priority ones first until they do.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::segment::{Segment, StatusBar, Zone};
use crate::theme::{Role, Theme};

/// Width of the gauge segment, in cells.
pub(super) const GAUGE_WIDTH: usize = 14;

fn span_w(s: &Span) -> usize {
    s.content.chars().count()
}

fn seg_w(seg: &Segment) -> usize {
    seg.spans.iter().map(span_w).sum()
}

/// Render `bar` into `area`, eliding low-priority segments to fit.
pub fn render(f: &mut Frame, area: Rect, bar: &StatusBar, theme: Theme) {
    let total = area.width as usize;
    if total == 0 {
        return;
    }
    let sep_style = theme.style(Role::StatusDim);

    // Drop the lowest-priority segments until the kept set fits the row.
    let mut kept: Vec<&Segment> = bar.segments.iter().collect();
    loop {
        let need =
            zone_w(&kept, Zone::Left) + zone_w(&kept, Zone::Center) + zone_w(&kept, Zone::Right);
        // Leading + trailing pad, plus a gap between adjacent non-empty zones.
        let gaps = 2 + present_gaps(&kept);
        if need + gaps <= total || kept.len() <= 1 {
            break;
        }
        let drop = kept
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| s.priority)
            .map(|(i, _)| i);
        match drop {
            Some(i) => {
                kept.remove(i);
            }
            None => break,
        }
    }

    let left_w = 1 + zone_w(&kept, Zone::Left); // +1 leading space
    let right_w = zone_w(&kept, Zone::Right) + 1; // +1 trailing space
    let center_w = zone_w(&kept, Zone::Center);

    let mut spans: Vec<Span> = vec![Span::raw(" ".to_string())];
    push_zone(&mut spans, &kept, Zone::Left, sep_style);

    if center_w > 0 {
        let pad_l = ((total.saturating_sub(center_w)) / 2).saturating_sub(left_w);
        spans.push(Span::raw(" ".repeat(pad_l)));
        push_zone(&mut spans, &kept, Zone::Center, sep_style);
        let used = left_w + pad_l + center_w;
        spans.push(Span::raw(" ".repeat(total.saturating_sub(used + right_w))));
    } else {
        spans.push(Span::raw(
            " ".repeat(total.saturating_sub(left_w + right_w)),
        ));
    }

    push_zone(&mut spans, &kept, Zone::Right, sep_style);
    spans.push(Span::raw(" ".to_string()));

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(theme.style(Role::StatusBar)),
        area,
    );
}

/// Total width of a zone's kept segments, with a ` · ` separator between them.
fn zone_w(kept: &[&Segment], zone: Zone) -> usize {
    let segs: Vec<&&Segment> = kept.iter().filter(|s| s.zone == zone).collect();
    if segs.is_empty() {
        return 0;
    }
    segs.iter().map(|s| seg_w(s)).sum::<usize>() + 3 * (segs.len() - 1)
}

/// One gap-cell for each pair of adjacent non-empty zones (so they don't touch).
fn present_gaps(kept: &[&Segment]) -> usize {
    let has = |z| kept.iter().any(|s| s.zone == z);
    let present = [Zone::Left, Zone::Center, Zone::Right]
        .into_iter()
        .filter(|&z| has(z))
        .count();
    present.saturating_sub(1)
}

fn push_zone<'a>(out: &mut Vec<Span<'a>>, kept: &[&'a Segment], zone: Zone, sep: Style) {
    let mut first = true;
    for seg in kept.iter().filter(|s| s.zone == zone) {
        if !first {
            out.push(Span::styled(" · ", sep));
        }
        first = false;
        out.extend(seg.spans.iter().cloned());
    }
}

/// A unicode progress gauge (`██████░░░░`) of `width` cells.
pub(super) fn gauge(frac: f32, width: usize) -> String {
    let filled = (frac.clamp(0.0, 1.0) * width as f32).round() as usize;
    let mut s = String::with_capacity(width * 3);
    s.extend(std::iter::repeat_n('█', filled));
    s.extend(std::iter::repeat_n('░', width.saturating_sub(filled)));
    s
}
