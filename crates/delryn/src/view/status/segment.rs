//! The status-bar segment model. Each context (reader / library / overlay)
//! produces a [`StatusBar`] of zoned, prioritised, *identified* [`Segment`]s; the
//! renderer ([`super::render`]) orders each zone per the user's `[status]` config,
//! then drops the lowest-priority ones first when the row is too narrow.

use ratatui::text::Span;

/// Where a segment sits on the row.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// Primary state / context (book title, library state, overlay name).
    Left,
    /// Optional centred context.
    Center,
    /// Key hints, reading fields, position/progress.
    Right,
}

/// A stable identity for each kind of segment, so the `[status]` config can
/// reorder and hide them by name (see [`SegmentId::from_label`]). Context labels
/// and key legends are grouped under [`SegmentId::Context`]/[`SegmentId::Keys`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SegmentId {
    /// Book title / author (reader) or library/overlay context label.
    Context,
    /// A transient flash message ("copied", "cover embedded", …).
    Flash,
    /// Search match counter (`⌕ 3/17`).
    Search,
    /// Active theme name.
    Theme,
    /// View-mode label (single / two-page / …).
    View,
    /// Continuous-scroll indicator.
    Continuous,
    /// Manga (right-to-left) indicator.
    Manga,
    /// Page counter (`p 12/340`).
    Page,
    /// Zoom / fit label.
    Zoom,
    /// Section position (`12/31`).
    Position,
    /// Reading percent (`23%`).
    Percent,
    /// Slim unicode progress gauge.
    Gauge,
    /// Wall-clock time (`14:05`).
    Clock,
    /// Contextual key hints (the former `legend` cascade).
    Keys,
}

impl SegmentId {
    /// The lowercase config label for this segment.
    pub fn label(self) -> &'static str {
        match self {
            SegmentId::Context => "context",
            SegmentId::Flash => "flash",
            SegmentId::Search => "search",
            SegmentId::Theme => "theme",
            SegmentId::View => "view",
            SegmentId::Continuous => "continuous",
            SegmentId::Manga => "manga",
            SegmentId::Page => "page",
            SegmentId::Zoom => "zoom",
            SegmentId::Position => "position",
            SegmentId::Percent => "percent",
            SegmentId::Gauge => "gauge",
            SegmentId::Clock => "clock",
            SegmentId::Keys => "keys",
        }
    }
}

/// One status-bar item: an identity, pre-styled spans, its zone, and a drop
/// priority — higher survives longer when the row can't fit everything (so
/// position/search beat the gauge, and key hints drop before the title).
pub struct Segment {
    pub id: SegmentId,
    pub spans: Vec<Span<'static>>,
    pub zone: Zone,
    pub priority: u8,
}

/// An assembled status bar: a flat, ordered list of segments.
#[derive(Default)]
pub struct StatusBar {
    pub segments: Vec<Segment>,
}

impl StatusBar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a segment of pre-styled spans (skipped if empty).
    pub fn add(&mut self, id: SegmentId, zone: Zone, priority: u8, spans: Vec<Span<'static>>) {
        if !spans.is_empty() {
            self.segments.push(Segment {
                id,
                spans,
                zone,
                priority,
            });
        }
    }

    /// Add a single-style text segment (skipped if the text is empty).
    pub fn text(
        &mut self,
        id: SegmentId,
        zone: Zone,
        priority: u8,
        text: impl Into<String>,
        style: ratatui::style::Style,
    ) {
        let text = text.into();
        if !text.is_empty() {
            self.add(id, zone, priority, vec![Span::styled(text, style)]);
        }
    }
}
