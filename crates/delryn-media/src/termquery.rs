//! Asking the terminal what it is, and how big its cells are.
//!
//! Two questions the environment cannot answer honestly:
//!
//! **Which terminal is this?** `TERM_PROGRAM` is overwritten by tmux with its own
//! name, and the tmux *server* hands every pane the environment of whichever
//! terminal happened to start it — so a session started from one terminal and
//! re-attached from another reports the wrong one indefinitely. XTVERSION
//! (`CSI > q`) asks over the wire and describes the terminal actually attached
//! now. Inside tmux the query is wrapped in tmux's passthrough DCS so it reaches
//! the outer terminal (needs `allow-passthrough`, on by default since tmux 3.3a).
//!
//! **How big is a cell, in the units the image protocol places by?** `CSI 16t`
//! (what `ratatui-image` queries) and the protocol's own units disagree on HiDPI:
//! iTerm2 answers `16t` in *physical* pixels — 22×46 on a 2× Retina panel — while
//! sizing inline images in *points*, 11×23. Deriving the cell from `CSI 14t` (text
//! area in pixels) over `CSI 18t` (text area in cells) keeps one unit system,
//! whichever it turns out to be.
//!
//! Reading replies needs raw mode (bytes aren't line-buffered) and a timeout (an
//! unsupported terminal never answers and must not hang startup); the prior
//! raw-mode state is restored either way. Unix only; elsewhere every query is a
//! no-op and callers keep whatever detection they had.
//!
//! Ported from the sibling project `lyrfin`, where this was worked out against
//! real terminals; the captured replies in the tests come from there.

/// How long any single terminal query may wait for its reply. Long enough for a
/// terminal that answers, short enough that one which never will costs a blink of
/// startup.
pub const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(120);

/// Ask the terminal what it *is* (XTVERSION, `CSI > q`) and return its
/// self-reported name — `"iTerm2 3.6.11"`, `"WezTerm 20260623-…"`, `"ghostty …"`.
///
/// `None` when the terminal doesn't implement XTVERSION (many don't), so every
/// caller needs an environment-based fallback. Must be called while stdin is
/// still ours — before `ratatui-image`'s own stdio query and before the alternate
/// screen.
#[cfg(unix)]
pub fn terminal_name(timeout: std::time::Duration) -> Option<String> {
    use std::io::Write;

    use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled};

    let was_raw = is_raw_mode_enabled().unwrap_or(false);
    if !was_raw && enable_raw_mode().is_err() {
        return None;
    }
    let query = xtversion_query(std::env::var_os("TMUX").is_some());
    {
        let mut out = std::io::stdout();
        let _ = out.write_all(query.as_bytes());
        let _ = out.flush();
    }
    // The reply is a DCS string terminated by ST; stop as soon as it's whole.
    let buf = drain_replies(timeout, |b| parse_xtversion(b).is_some());
    if !was_raw {
        let _ = disable_raw_mode();
    }
    parse_xtversion(&buf)
}

#[cfg(not(unix))]
pub fn terminal_name(_timeout: std::time::Duration) -> Option<String> {
    None
}

/// The terminal's cell size derived from `CSI 14t` / `CSI 18t`, in the units its
/// image protocol places by. `None` unless both replies arrive and divide into a
/// sane cell, so a terminal that answers neither keeps whatever detection the
/// caller already had.
#[cfg(unix)]
pub fn measured_cell_size(timeout: std::time::Duration) -> Option<(u16, u16)> {
    use std::io::Write;

    use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled};

    let was_raw = is_raw_mode_enabled().unwrap_or(false);
    if !was_raw && enable_raw_mode().is_err() {
        return None;
    }
    {
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[14t\x1b[18t");
        let _ = out.flush();
    }
    let buf = drain_replies(timeout, |b| parse_cell_size(b).is_some());
    if !was_raw {
        let _ = disable_raw_mode();
    }
    parse_cell_size(&buf)
}

#[cfg(not(unix))]
pub fn measured_cell_size(_timeout: std::time::Duration) -> Option<(u16, u16)> {
    None
}

/// Read stdin until `is_done` accepts what has arrived, or `timeout` elapses.
/// Raw mode must already be on. Shared by every query here.
#[cfg(unix)]
fn drain_replies(timeout: std::time::Duration, is_done: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let deadline = std::time::Instant::now() + timeout;
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut pfd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is a valid, fully initialized `pollfd` that outlives the
        // call, and `poll` only reads/writes through that one pointer.
        let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
        if ready <= 0 || pfd.revents & libc::POLLIN == 0 {
            break;
        }
        let mut chunk = [0u8; 1024];
        // SAFETY: reading at most `chunk.len()` bytes into an owned, live buffer.
        let n = unsafe {
            libc::read(
                libc::STDIN_FILENO,
                chunk.as_mut_ptr().cast::<libc::c_void>(),
                chunk.len(),
            )
        };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        if is_done(&buf) {
            break;
        }
    }
    buf
}

/// Build the XTVERSION query, wrapped in tmux passthrough when inside tmux (every
/// inner `ESC` doubled). Pure, so the escaping is unit-testable.
fn xtversion_query(in_tmux: bool) -> String {
    let q = "\x1b[>q";
    if in_tmux {
        format!("\x1bPtmux;{}\x1b\\", q.replace('\x1b', "\x1b\x1b"))
    } else {
        q.to_string()
    }
}

/// Extract the name from an XTVERSION reply: `DCS > | <name> ST`, where ST is
/// either `ESC \` or BEL. `None` until the whole reply has arrived — which is what
/// lets the read loop stop at exactly the right moment instead of truncating.
fn parse_xtversion(buf: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(buf);
    let start = s.find("\x1bP>|")? + 4;
    let rest = &s[start..];
    let end = rest.find("\x1b\\").or_else(|| rest.find('\x07'))?;
    let name = rest[..end].trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Pull `CSI 4 ; h ; w t` (pixels) and `CSI 8 ; rows ; cols t` (cells) out of a
/// reply buffer and divide them into a cell size. Pure, so it is unit-testable.
fn parse_cell_size(buf: &[u8]) -> Option<(u16, u16)> {
    let s = String::from_utf8_lossy(buf);
    let pair = |lead: &str| -> Option<(u32, u32)> {
        let at = s.find(lead)? + lead.len();
        let rest = &s[at..];
        let end = rest.find('t')?;
        let mut it = rest[..end].split(';');
        Some((
            it.next()?.trim().parse().ok()?,
            it.next()?.trim().parse().ok()?,
        ))
    };
    let (px_h, px_w) = pair("\x1b[4;")?;
    let (rows, cols) = pair("\x1b[8;")?;
    if rows == 0 || cols == 0 {
        return None;
    }
    let (cw, ch) = (px_w / cols, px_h / rows);
    // A degenerate answer (a zero or 1-pixel cell) is worse than no answer: it
    // would mis-size every image. Leave the caller's detection alone.
    (cw >= 2 && ch >= 2).then_some((cw as u16, ch as u16))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from iTerm2 on a 2× Retina panel: 1066×612 px over 96×25 cells →
    /// 11×24, while `CSI 16t` reports 22×46 (physical pixels). Sizing images from
    /// the latter makes every one of them twice the size it was given room for.
    #[test]
    fn derives_cell_size_from_area_replies() {
        assert_eq!(
            parse_cell_size(b"\x1b[4;612;1066t\x1b[8;25;96t"),
            Some((11, 24))
        );
    }

    #[test]
    fn cell_size_needs_both_replies() {
        assert_eq!(parse_cell_size(b"\x1b[4;612;1066t"), None);
        assert_eq!(parse_cell_size(b"\x1b[8;25;96t"), None);
        assert_eq!(parse_cell_size(b""), None);
    }

    /// A terminal that answers with zeros, or a 1-pixel cell, is answering wrong —
    /// take no answer over a wrong one.
    #[test]
    fn degenerate_cell_size_is_rejected() {
        assert_eq!(parse_cell_size(b"\x1b[4;612;1066t\x1b[8;0;0t"), None);
        assert_eq!(parse_cell_size(b"\x1b[4;25;96t\x1b[8;25;96t"), None);
    }

    #[test]
    fn parses_xtversion_replies() {
        assert_eq!(
            parse_xtversion(b"\x1bP>|iTerm2 3.6.11\x1b\\"),
            Some("iTerm2 3.6.11".to_string())
        );
        // BEL is a valid string terminator too.
        assert_eq!(
            parse_xtversion(b"\x1bP>|WezTerm 20260623\x07"),
            Some("WezTerm 20260623".to_string())
        );
        assert_eq!(
            parse_xtversion(b"\x1bP>|ghostty 1.2.0\x1b\\"),
            Some("ghostty 1.2.0".to_string())
        );
    }

    /// Incomplete means keep reading, not truncate — the read loop's stop
    /// condition is this function returning `Some`.
    #[test]
    fn an_incomplete_xtversion_reply_is_none() {
        assert_eq!(parse_xtversion(b"\x1bP>|iTerm2 3.6"), None);
        assert_eq!(parse_xtversion(b""), None);
    }

    #[test]
    fn the_query_is_tmux_wrapped_inside_tmux() {
        assert_eq!(xtversion_query(false), "\x1b[>q");
        assert_eq!(xtversion_query(true), "\x1bPtmux;\x1b\x1b[>q\x1b\\");
    }
}
