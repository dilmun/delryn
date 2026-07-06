//! Pack a [`StatusBar`] into the row: Left segments left-aligned, Right
//! right-aligned, Center centred. When the segments don't fit, drop the
//! lowest-priority ones first until they do.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::segment::{Segment, StatusBar, Zone};
use crate::config::StatusFields;
use crate::theme::{Role, Theme};

/// Width of the gauge segment, in cells.
pub(super) const GAUGE_WIDTH: usize = 14;

fn span_w(s: &Span) -> usize {
    s.content.chars().count()
}

fn seg_w(seg: &Segment) -> usize {
    seg.spans.iter().map(span_w).sum()
}

/// The configured segment order for a zone (by `SegmentId` label).
fn zone_order(status: &StatusFields, zone: Zone) -> &[String] {
    match zone {
        Zone::Left => &status.left,
        Zone::Center => &status.center,
        Zone::Right => &status.right,
    }
}

/// A segment's rank within its zone's configured order — listed segments sort to
/// the front in list order; unlisted ones (or unknown labels) keep their built-in
/// order after them.
fn order_rank(seg: &Segment, status: &StatusFields) -> usize {
    zone_order(status, seg.zone)
        .iter()
        .position(|l| l == seg.id.label())
        .unwrap_or(usize::MAX)
}

/// Render `bar` into `area`, applying the `[status]` config (per-zone order +
/// separator) and eliding low-priority segments to fit.
pub fn render(f: &mut Frame, area: Rect, bar: &StatusBar, theme: Theme, status: &StatusFields) {
    let total = area.width as usize;
    if total == 0 {
        return;
    }
    let sep_style = theme.style(Role::StatusDim);
    let sep = format!(" {} ", status.separator);
    let sep_w = sep.chars().count();

    // Order each zone per the config (stable — unlisted keep built-in order).
    let mut kept: Vec<&Segment> = bar.segments.iter().collect();
    kept.sort_by_key(|s| order_rank(s, status));

    // Drop the lowest-priority segments until the kept set fits the row.
    loop {
        let need = zone_w(&kept, Zone::Left, sep_w)
            + zone_w(&kept, Zone::Center, sep_w)
            + zone_w(&kept, Zone::Right, sep_w);
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

    let left_w = 1 + zone_w(&kept, Zone::Left, sep_w); // +1 leading space
    let right_w = zone_w(&kept, Zone::Right, sep_w) + 1; // +1 trailing space
    let center_w = zone_w(&kept, Zone::Center, sep_w);

    let mut spans: Vec<Span> = vec![Span::raw(" ".to_string())];
    push_zone(&mut spans, &kept, Zone::Left, &sep, sep_style);

    if center_w > 0 {
        let pad_l = ((total.saturating_sub(center_w)) / 2).saturating_sub(left_w);
        spans.push(Span::raw(" ".repeat(pad_l)));
        push_zone(&mut spans, &kept, Zone::Center, &sep, sep_style);
        let used = left_w + pad_l + center_w;
        spans.push(Span::raw(" ".repeat(total.saturating_sub(used + right_w))));
    } else {
        spans.push(Span::raw(
            " ".repeat(total.saturating_sub(left_w + right_w)),
        ));
    }

    push_zone(&mut spans, &kept, Zone::Right, &sep, sep_style);
    spans.push(Span::raw(" ".to_string()));

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(theme.style(Role::StatusBar)),
        area,
    );
}

/// Total width of a zone's kept segments, with the separator between them.
fn zone_w(kept: &[&Segment], zone: Zone, sep_w: usize) -> usize {
    let segs: Vec<&&Segment> = kept.iter().filter(|s| s.zone == zone).collect();
    if segs.is_empty() {
        return 0;
    }
    segs.iter().map(|s| seg_w(s)).sum::<usize>() + sep_w * (segs.len() - 1)
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

fn push_zone<'a>(
    out: &mut Vec<Span<'a>>,
    kept: &[&'a Segment],
    zone: Zone,
    sep: &str,
    style: Style,
) {
    let mut first = true;
    for seg in kept.iter().filter(|s| s.zone == zone) {
        if !first {
            out.push(Span::styled(sep.to_string(), style));
        }
        first = false;
        out.extend(seg.spans.iter().cloned());
    }
}

/// A unicode progress gauge of `width` cells, split into the filled and empty
/// parts so each can be themed separately (`(██████, ░░░░)`) — the fill takes the
/// theme accent, the track a muted colour.
pub(super) fn gauge(frac: f32, width: usize) -> (String, String) {
    let filled = ((frac.clamp(0.0, 1.0) * width as f32).round() as usize).min(width);
    ("█".repeat(filled), "░".repeat(width.saturating_sub(filled)))
}

#[cfg(test)]
mod tests {
    use super::super::segment::SegmentId;
    use super::*;
    use ratatui::style::Style;

    fn seg(id: SegmentId, zone: Zone) -> Segment {
        Segment {
            id,
            spans: vec![Span::styled(id.label().to_string(), Style::default())],
            zone,
            priority: 5,
        }
    }

    #[test]
    fn config_reorders_a_zone_and_keeps_unlisted_after() {
        // Right zone produced as [percent, position, gauge]; config asks for
        // position first, then gauge — percent (unlisted) keeps its slot after.
        let status = StatusFields {
            right: vec!["position".into(), "gauge".into()],
            ..StatusFields::default()
        };
        let mut segs = [
            seg(SegmentId::Percent, Zone::Right),
            seg(SegmentId::Position, Zone::Right),
            seg(SegmentId::Gauge, Zone::Right),
        ];
        let mut kept: Vec<&Segment> = segs.iter().collect();
        kept.sort_by_key(|s| order_rank(s, &status));
        let order: Vec<&str> = kept.iter().map(|s| s.id.label()).collect();
        assert_eq!(order, ["position", "gauge", "percent"]);
        // No config for a zone → built-in order preserved (stable sort).
        let empty = StatusFields::default();
        segs.reverse();
        let mut kept2: Vec<&Segment> = segs.iter().collect();
        kept2.sort_by_key(|s| order_rank(s, &empty));
        let order2: Vec<&str> = kept2.iter().map(|s| s.id.label()).collect();
        assert_eq!(order2, ["gauge", "position", "percent"]);
    }
}
