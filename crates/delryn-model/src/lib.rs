//! `delryn-model` — the pure domain vocabulary shared across every layer.
//!
//! Content blocks, metadata, table-of-contents, and (later) text heuristics.
//! No I/O, no UI, no external dependencies — everything else depends on this.

pub mod content;
pub mod math;
pub mod metadata;
pub mod naming;
pub mod status;
pub mod toc;

pub use content::{Anchor, Block, CalloutKind, Inline, Section, Span, TableCell, find_footnote};
pub use metadata::Metadata;
pub use status::ReadingStatus;
pub use toc::{OutlineItem, TocEntry};
