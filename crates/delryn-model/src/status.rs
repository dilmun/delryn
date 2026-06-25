//! Reading status: the progress-derived states (unread / reading / finished) plus
//! the manual overrides a reader can set (paused / dropped / reference). A manual
//! override, when present, wins over the progress-derived status.

/// A book's reading status. The first three are derived from reading progress;
/// the last three are manual overrides the reader sets explicitly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadingStatus {
    Unread,
    Reading,
    Finished,
    Paused,
    Dropped,
    Reference,
}

impl ReadingStatus {
    /// Progress percent at/above which a book counts as finished.
    pub const FINISHED_PCT: u8 = 98;

    /// Parse a stored manual override (empty / unknown → `None`).
    pub fn parse_manual(s: &str) -> Option<ReadingStatus> {
        match s.trim().to_ascii_lowercase().as_str() {
            "paused" => Some(Self::Paused),
            "dropped" | "abandoned" => Some(Self::Dropped),
            "reference" | "ref" => Some(Self::Reference),
            _ => None,
        }
    }

    /// The effective status: a manual override wins, else derived from `pct`.
    pub fn effective(pct: u8, manual: &str) -> ReadingStatus {
        if let Some(s) = Self::parse_manual(manual) {
            return s;
        }
        if pct == 0 {
            Self::Unread
        } else if pct >= Self::FINISHED_PCT {
            Self::Finished
        } else {
            Self::Reading
        }
    }

    /// Whether this is a manual override (vs a progress-derived status).
    pub fn is_manual(self) -> bool {
        matches!(self, Self::Paused | Self::Dropped | Self::Reference)
    }

    /// The storage string for a manual override (empty for derived statuses).
    pub fn manual_str(self) -> &'static str {
        match self {
            Self::Paused => "paused",
            Self::Dropped => "dropped",
            Self::Reference => "reference",
            _ => "",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::Reading => "reading",
            Self::Finished => "finished",
            Self::Paused => "paused",
            Self::Dropped => "dropped",
            Self::Reference => "reference",
        }
    }

    /// A one-glyph badge for the list (none for the default Unread state). All
    /// monochrome BMP symbols, safe on any terminal.
    pub fn badge(self) -> &'static str {
        match self {
            Self::Unread => "",
            Self::Reading => "◐",
            Self::Finished => "✓",
            Self::Paused => "‖",
            Self::Dropped => "✗",
            Self::Reference => "◆",
        }
    }

    /// Sort ordinal (groups statuses sensibly: in-progress first … dropped last).
    pub fn order(self) -> u8 {
        match self {
            Self::Reading => 0,
            Self::Paused => 1,
            Self::Unread => 2,
            Self::Reference => 3,
            Self::Finished => 4,
            Self::Dropped => 5,
        }
    }

    /// Cycle the *manual* override for the set key: none → paused → dropped →
    /// reference → none. Takes and returns the storage string.
    pub fn cycle_manual(current: &str) -> &'static str {
        match Self::parse_manual(current) {
            None => "paused",
            Some(Self::Paused) => "dropped",
            Some(Self::Dropped) => "reference",
            _ => "",
        }
    }
}
