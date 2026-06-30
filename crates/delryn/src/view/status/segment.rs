//! The status-bar segment model. Each context (reader / library / overlay)
//! produces a [`StatusBar`] of zoned, prioritised [`Segment`]s; the renderer
//! ([`super::render`]) packs them and drops the lowest-priority ones first when
//! the row is too narrow.

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

/// One status-bar item: pre-styled spans, its zone, and a drop priority — higher
/// survives longer when the row can't fit everything (so position/search beat the
/// gauge, and key hints drop before the title).
pub struct Segment {
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
    pub fn add(&mut self, zone: Zone, priority: u8, spans: Vec<Span<'static>>) {
        if !spans.is_empty() {
            self.segments.push(Segment {
                spans,
                zone,
                priority,
            });
        }
    }

    /// Add a single-style text segment (skipped if the text is empty).
    pub fn text(
        &mut self,
        zone: Zone,
        priority: u8,
        text: impl Into<String>,
        style: ratatui::style::Style,
    ) {
        let text = text.into();
        if !text.is_empty() {
            self.add(zone, priority, vec![Span::styled(text, style)]);
        }
    }
}
