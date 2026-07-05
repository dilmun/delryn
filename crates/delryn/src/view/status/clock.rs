//! Wall-clock helper for the optional status-bar clock segment.

use chrono::{Local, Timelike};

/// The current local time as `HH:MM` (24-hour).
pub fn local_hhmm() -> String {
    let now = Local::now();
    format!("{:02}:{:02}", now.hour(), now.minute())
}
