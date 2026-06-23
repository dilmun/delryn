//! `delryn-render` — turn the content model into laid-out terminal content.
//!
//! Reflow/layout and syntax highlighting today; pagination, tables, and
//! graphical math will join here. Produces a format- and ratatui-agnostic
//! intermediate (`layout::Run`/lines) that the view layer maps to ratatui.

pub mod highlight;
pub mod layout;
