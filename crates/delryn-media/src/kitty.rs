//! Kitty graphics protocol escape sequences and terminal capability detection.

use std::fmt::Write as _;

use ratatui_image::picker::Picker;

use crate::termquery;

/// Detect the terminal's image protocol + cell size by querying stdio. Returns
/// `None` if there's no tty or detection fails (then images are unavailable).
/// Call before entering the alternate screen / raw mode.
///
/// The handshake's answer is authoritative **except** on terminals that claim
/// Kitty support without implementing the half `ratatui-image` renders with —
/// unicode placeholders. There the image transmits, is never placed, and nothing
/// appears at all: no picture, no fallback, and the rest of the UI looks perfect.
/// iTerm2 ≥ 3.6 is exactly that: it answers the Kitty query `OK`, so detection
/// picks Kitty and every cover vanishes. Its own protocol renders correctly, so
/// the protocol is overridden after the query rather than before — blacklisting
/// Kitty up front makes iTerm2 fall to Sixel (its device attributes advertise
/// it), and blacklisting both fails the query outright, losing the queried cell
/// size that sizes every image rect.
pub fn detect_picker() -> Option<Picker> {
    // Ask what the terminal *is* first, while stdin is still ours and before
    // `from_query_stdio` starts its own conversation.
    let name = identify_terminal();

    // Enable the OSC 11 background-colour query so the `terminal` theme can match
    // its real backdrop (read back via [`terminal_background`]). The query ends in
    // a Device Status Report every terminal answers, so it never hangs.
    let opts = ratatui_image::picker::cap_parser::QueryStdioOptions {
        terminal_background_color_osc: true,
        ..Default::default()
    };
    let mut picker = Picker::from_query_stdio_with_options(opts).ok()?;

    if wants_iterm2_protocol(name.as_deref()) {
        picker.set_protocol_type(ratatui_image::picker::ProtocolType::Iterm2);
    }
    // Last resort for a terminal whose self-report is wrong, or one not yet
    // characterised. Applied over the detected picker so the queried cell size —
    // which sizes every image rect — survives the override.
    if let Some(forced) = forced_protocol() {
        picker.set_protocol_type(forced);
    }
    // Same idea for the cell size, which sizes every image rect. Normally unset:
    // the size the terminal reports is what the protocol places by, and delryn
    // deliberately does not second-guess it — see `termquery::measured_cell_size`
    // for the mismatch that tempts you to, and `terminal_report` for the numbers.
    if let Some((cw, ch)) = forced_cell_size() {
        let protocol = picker.protocol_type();
        // `from_fontsize` is deprecated upstream, but `font_size` is private with
        // no setter and the suggested replacements can't express an explicit cell
        // size — so there is no supported way to honour the override.
        #[expect(deprecated)]
        let mut fixed = Picker::from_fontsize(ratatui_image::FontSize::new(cw, ch));
        // Keep the protocol resolved above: `from_fontsize` re-guesses it from the
        // environment, which is the thing we just went to some trouble to distrust.
        fixed.set_protocol_type(protocol);
        picker = fixed;
    }
    // Record how images may be moved. This is iTerm2 specifically, *not* everything
    // that renders iTerm2 inline images: WezTerm shares that protocol but implements
    // the Kitty one properly, and would needlessly lose cheap moves (and with them
    // continuous PDF stacking) if it were lumped in. See [`moves_need_retransmit`].
    MOVES_NEED_RETRANSMIT.store(
        is_iterm2(name.as_deref()),
        std::sync::atomic::Ordering::Relaxed,
    );
    Some(picker)
}

/// Whether *moving* a placed image on this terminal means re-transmitting its data.
///
/// The protocol defines a repeat of an `(image id, placement id)` pair as replacing
/// that placement, which makes a move a single cheap escape. iTerm2 honours that
/// only while the placement keeps its **geometry**: change `c=`/`r=` or the source
/// rectangle — as every row of a continuous scroll does — and it draws nothing at
/// all, leaving the previous pixels on screen. (Moving a placement of *unchanged*
/// geometry does work, so this is about geometry, not movement.) Freeing the image
/// and re-sending it is the only sequence it renders reliably.
///
/// Resolved once, from the protocol [`detect_picker`] settled on, so a terminal
/// forced with `DELRYN_IMAGE_PROTOCOL` gets the matching answer. `DELRYN_MOVE_RETRANSMIT=1|0`
/// forces it directly, for a terminal delryn hasn't been taught about.
pub fn moves_need_retransmit() -> bool {
    if let Some(forced) =
        std::env::var("DELRYN_MOVE_RETRANSMIT")
            .ok()
            .and_then(|v| match v.trim() {
                "1" | "true" | "yes" => Some(true),
                "0" | "false" | "no" => Some(false),
                _ => None,
            })
    {
        return forced;
    }
    MOVES_NEED_RETRANSMIT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Set from [`detect_picker`]. Defaults to `false` — the protocol-conformant
/// behaviour — so a process that never detected a terminal (tests) is unaffected.
static MOVES_NEED_RETRANSMIT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// `DELRYN_CELL_SIZE=WxH` (e.g. `22x46`) — the terminal's cell size in the pixel
/// units its **image protocol** places by, for a terminal that misreports it.
fn forced_cell_size() -> Option<(u16, u16)> {
    parse_cell_size_override(&std::env::var("DELRYN_CELL_SIZE").ok()?)
}

/// Pure half of [`forced_cell_size`], so the format is unit-testable without the
/// process-wide environment (which parallel tests cannot safely share).
fn parse_cell_size_override(raw: &str) -> Option<(u16, u16)> {
    let (w, h) = raw.split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    // A zero cell divides by zero downstream — treat it as unset.
    (w > 0 && h > 0).then_some((w, h))
}

/// The terminal's self-reported name, asked over the wire **only when the
/// environment can't be trusted** — that is, inside a multiplexer.
///
/// Outside one, `TERM_PROGRAM` (and friends) name the terminal correctly, and a
/// round-trip costs every launch the query timeout on any terminal that doesn't
/// implement XTVERSION — for an answer the environment already gave. Inside tmux
/// the environment describes whichever terminal started the *server*, so the wire
/// is the only honest source and the round-trip earns its keep.
fn identify_terminal() -> Option<String> {
    std::env::var_os("TMUX")?;
    termquery::terminal_name(termquery::QUERY_TIMEOUT)
}

/// Does this terminal render the **iTerm2 inline-image protocol**? Covers iTerm2
/// and WezTerm, which implements the same one.
///
/// Each is identified by a signal that survives a multiplexer, because tmux
/// overwrites `TERM_PROGRAM` with its own name: `LC_TERMINAL` for iTerm2,
/// `WEZTERM_EXECUTABLE` for WezTerm. What the terminal *reported* over the wire
/// outranks all of them — it is the only signal that stays correct inside tmux,
/// where the environment describes whichever terminal started the server rather
/// than the one attached now.
fn wants_iterm2_protocol(reported_name: Option<&str>) -> bool {
    iterm2_protocol_from(
        reported_name,
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("LC_TERMINAL").ok().as_deref(),
        std::env::var("WEZTERM_EXECUTABLE").ok().as_deref(),
    )
}

/// Pure half of [`wants_iterm2_protocol`], so the precedence is unit-testable
/// without touching the process-wide environment.
fn iterm2_protocol_from(
    reported_name: Option<&str>,
    term_program: Option<&str>,
    lc_terminal: Option<&str>,
    wezterm_exe: Option<&str>,
) -> bool {
    // A terminal that identified itself is authoritative — including when it names
    // something else, so tmux running under Ghostty is not misread as iTerm2 on
    // the strength of a stale environment variable.
    if let Some(name) = reported_name {
        return name.contains("iTerm") || name.contains("WezTerm");
    }
    term_program.is_some_and(|p| p.contains("iTerm") || p.contains("WezTerm"))
        || lc_terminal.is_some_and(|t| t.contains("iTerm"))
        || wezterm_exe.is_some_and(|w| !w.is_empty())
}

/// Is this terminal **iTerm2 itself**, as opposed to anything that merely renders
/// its inline-image protocol? Same precedence as [`wants_iterm2_protocol`] — the
/// terminal's own answer outranks the environment — but deliberately excludes
/// WezTerm, whose Kitty implementation is sound.
fn is_iterm2(reported_name: Option<&str>) -> bool {
    iterm2_from(
        reported_name,
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("LC_TERMINAL").ok().as_deref(),
    )
}

/// Pure half of [`is_iterm2`], so the precedence is unit-testable without touching
/// the process-wide environment.
fn iterm2_from(
    reported_name: Option<&str>,
    term_program: Option<&str>,
    lc_terminal: Option<&str>,
) -> bool {
    if let Some(name) = reported_name {
        return name.contains("iTerm");
    }
    term_program.is_some_and(|p| p.contains("iTerm"))
        || lc_terminal.is_some_and(|t| t.contains("iTerm"))
}

/// `DELRYN_IMAGE_PROTOCOL=kitty|iterm2|sixel|halfblocks` — force the protocol
/// when detection gets it wrong. The escape hatch for a terminal delryn hasn't
/// been taught about yet.
fn forced_protocol() -> Option<ratatui_image::picker::ProtocolType> {
    protocol_from_name(&std::env::var("DELRYN_IMAGE_PROTOCOL").ok()?)
}

/// Pure half of [`forced_protocol`], so the accepted names are unit-testable.
fn protocol_from_name(name: &str) -> Option<ratatui_image::picker::ProtocolType> {
    use ratatui_image::picker::ProtocolType;
    match name.trim().to_ascii_lowercase().as_str() {
        "kitty" => Some(ProtocolType::Kitty),
        "iterm2" | "iterm" => Some(ProtocolType::Iterm2),
        "sixel" => Some(ProtocolType::Sixel),
        "halfblocks" | "blocks" => Some(ProtocolType::Halfblocks),
        _ => None,
    }
}

/// What the terminal answered about itself and its geometry, for the `--version`
/// style diagnostic and for reporting a bug against a terminal delryn renders
/// wrongly. Runs the queries fresh, so call it outside the alternate screen.
///
/// The measured cell size is *reported*, never applied: `ratatui-image`'s queried
/// size is what every protocol places by, and second-guessing it automatically
/// causes worse bugs than the mismatch it corrects. When the two disagree, the
/// numbers here are what to pass to `DELRYN_CELL_SIZE`.
pub fn terminal_report() -> String {
    // Unlike startup, this asks outright: the whole point is to show what the
    // terminal says, including a terminal outside tmux that startup wouldn't ask.
    let name = termquery::terminal_name(termquery::QUERY_TIMEOUT);
    let measured = termquery::measured_cell_size(termquery::QUERY_TIMEOUT);
    let picker = detect_picker();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "terminal:  {}",
        name.as_deref().unwrap_or("(no XTVERSION reply)")
    );
    let _ = writeln!(
        out,
        "protocol:  {:?}",
        picker.as_ref().map(Picker::protocol_type)
    );
    let reported = picker.as_ref().map(|p| {
        let fs = p.font_size();
        (fs.width, fs.height)
    });
    let _ = writeln!(out, "cell (reported): {reported:?}");
    let _ = writeln!(out, "cell (measured): {measured:?}");
    if let (Some(r), Some(m)) = (reported, measured)
        && r != m
    {
        let _ = writeln!(
            out,
            "note: the two disagree — if images render at the wrong size, try \
             DELRYN_CELL_SIZE={}x{}",
            m.0, m.1
        );
    }
    out
}

/// The terminal's background colour, if it answered the OSC 11 query during
/// [`detect_picker`]. Lets the `terminal` theme — which has no colours of its own
/// — recolour/invert images against the real backdrop instead of white paper.
pub fn terminal_background(picker: &Picker) -> Option<[u8; 3]> {
    picker.capabilities().iter().find_map(|c| match c {
        ratatui_image::picker::Capability::Background(r, g, b) => Some([*r, *g, *b]),
        _ => None,
    })
}

/// Kitty escape sequence to delete an image (and free its data) by id.
pub fn delete_image_seq(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id}\x1b\\")
}

/// Kitty escape to remove image `id`'s **placements** while **keeping its data**
/// resident (lowercase `d=i`). Used to *move* an already-transmitted page: delete
/// the old placement, then [`place_image_seq`] it at the new spot — no re-transmit.
/// This is what makes continuous scrolling cheap (each row change re-places a few
/// bytes instead of re-sending multi-MB rasters).
pub fn delete_placement_seq(id: u32) -> String {
    format!("\x1b_Ga=d,d=i,i={id}\x1b\\")
}

/// Kitty escape to delete **every** image and free all its data (`d=A`, no id).
/// A blunt teardown for reader exit / a full restage, where individually tracking
/// ids isn't worth it — leaves no resident image (and so no ghost) behind.
pub fn delete_all_images_seq() -> String {
    "\x1b_Ga=d,d=A\x1b\\".to_string()
}

// ── Direct Kitty image management (for full-page PDF rendering) ───────────────
//
// The unicode-placeholder path (inline figures) is for images that flow with
// text. A full PDF page is better managed directly, the way kitty's own `icat`
// does it: transmit the page to the terminal *once* as a stored image (`a=t`),
// then *display* it with a cheap placement (`a=p`) that re-uses the stored data.
// Swapping pages then never re-transmits — and the previous page can stay on
// screen until the next placement lands, so a page turn has no black gap.

/// Kitty: transmit `png` to the terminal and store it under `id` **without
/// displaying it** (`a=t`). Chunked at the protocol's 4096-base64-char limit.
/// Show it later with [`place_image_seq`]; `id` and the data persist until
/// [`delete_image_seq`]. `q=2` suppresses the terminal's responses.
pub fn transmit_image_seq(id: u32, png: &[u8]) -> String {
    use base64::Engine;
    // 4096 base64 chars ⇒ 3072 source bytes per chunk.
    const CHUNK: usize = (4096 / 4) * 3;
    let chunks = png.chunks(CHUNK);
    let n = chunks.len().max(1);
    let mut out = String::with_capacity(png.len() * 4 / 3 + n * 24);
    for (i, chunk) in chunks.enumerate() {
        out.push_str("\x1b_Gq=2,");
        if i == 0 {
            // a=t: transmit only (store, don't display). f=100: PNG (kitty reads
            // the dimensions from the header). t=d: data is inline (direct).
            let _ = write!(out, "i={id},a=t,f=100,t=d,");
        }
        let more = u8::from(i + 1 < n);
        let _ = write!(out, "m={more};");
        base64::engine::general_purpose::STANDARD.encode_string(chunk, &mut out);
        out.push_str("\x1b\\");
    }
    out
}

/// Kitty: transmit an image stored at `path` under `id` **without displaying it**
/// (`a=t`), reading the pixel data from a *temporary file* (`t=t`) instead of
/// streaming it inline. The terminal opens the file directly and **deletes it
/// after reading**, so the escape carries only the (base64) path — a few dozen
/// bytes — rather than megabytes of base64. This is what makes fast page turns
/// cheap: streaming a full-page raster inline (`t=d`) blocks the loop ~60ms per
/// turn; via a file it's a small write plus a tiny escape. `f=100` = PNG.
pub fn transmit_file_seq(id: u32, path: &str) -> String {
    use base64::Engine;
    let payload = base64::engine::general_purpose::STANDARD.encode(path.as_bytes());
    format!("\x1b_Gq=2,i={id},a=t,f=100,t=t;{payload}\x1b\\")
}

/// Kitty: display the already-transmitted image `id` at terminal cell
/// (`col`,`row`) (1-based), scaled to fill `cols`×`rows` cells (`a=p`).
///
/// `crop` is an optional source rectangle `(x, y, w, h)` **in image pixels**: only
/// that region of the transmitted raster is shown (scaled to the `cols`×`rows`
/// box). This is how page zoom/pan places a sub-window of the full page raster.
/// `None` shows the whole image (byte-identical to the un-cropped placement).
///
/// Placements key on the `(image-id, placement-id)` pair. `placement = 0` omits `p=`
/// (the default placement): fine when every placed image has a **distinct** id (the PDF
/// pages of a spread), where sharing the default placement id would make the second image
/// delete the first (the left page went blank). When the **same** image is placed at many
/// spots at once — an inline symbol like `ℝ` reused across a page — each occurrence needs
/// its **own non-zero `placement`** id, or they all share `p=0` and every placement but the
/// last is overwritten. The cursor is saved/restored (`\x1b7`/`\x1b8`) so the surrounding
/// TUI is undisturbed.
pub fn place_image_seq(
    id: u32,
    col: u16,
    row: u16,
    cols: u16,
    rows: u16,
    crop: Option<(u32, u32, u32, u32)>,
    placement: u32,
) -> String {
    let src = match crop {
        Some((x, y, w, h)) => format!(",x={x},y={y},w={w},h={h}"),
        None => String::new(),
    };
    let pid = if placement > 0 {
        format!(",p={placement}")
    } else {
        String::new()
    };
    format!("\x1b7\x1b[{row};{col}H\x1b_Ga=p,i={id}{pid},c={cols},r={rows}{src},q=2\x1b\\\x1b8")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file transmit carries only the (base64) path — a tiny escape, not the
    /// multi-MB base64 blast of the inline `t=d` medium.
    #[test]
    fn file_transmit_carries_only_the_base64_path() {
        use base64::Engine;
        let seq = transmit_file_seq(0x0F00_0001, "/tmp/delryn-kitty-7.png");
        assert!(seq.contains("a=t"), "transmit (store, don't display)");
        assert!(seq.contains("t=t"), "temporary-file medium");
        assert!(seq.contains("f=100"), "PNG format");
        assert!(seq.contains("i=251658241"), "image id");
        let payload = base64::engine::general_purpose::STANDARD.encode("/tmp/delryn-kitty-7.png");
        assert!(seq.contains(&payload), "base64-encoded path payload");
        assert!(seq.len() < 120, "tiny escape, not a multi-MB blast");
    }

    use ratatui_image::picker::ProtocolType;

    /// What the terminal says over the wire outranks every environment variable —
    /// the whole reason for asking. Inside tmux the environment describes whichever
    /// terminal started the server, so a Ghostty session that was once started from
    /// iTerm2 must not be driven down the iTerm2 path.
    #[test]
    fn a_terminals_own_answer_outranks_the_environment() {
        assert!(iterm2_protocol_from(
            Some("iTerm2 3.6.11"),
            None,
            None,
            None
        ));
        assert!(iterm2_protocol_from(
            Some("WezTerm 20260623"),
            None,
            None,
            None
        ));
        assert!(
            !iterm2_protocol_from(
                Some("ghostty 1.2.0"),
                Some("iTerm.app"),
                Some("iTerm2"),
                None
            ),
            "a terminal that named itself is believed over a stale environment"
        );
    }

    /// With no XTVERSION reply, fall back to the variables that survive tmux.
    #[test]
    fn without_a_reply_the_multiplexer_safe_variables_decide() {
        assert!(iterm2_protocol_from(None, Some("iTerm.app"), None, None));
        assert!(iterm2_protocol_from(None, None, Some("iTerm2"), None));
        assert!(iterm2_protocol_from(
            None,
            None,
            None,
            Some("/usr/bin/wezterm")
        ));
        // tmux overwrites TERM_PROGRAM with its own name — which names no terminal
        // we override for, so the queried protocol stands.
        assert!(!iterm2_protocol_from(None, Some("tmux"), None, None));
        assert!(!iterm2_protocol_from(None, None, None, None));
        assert!(!iterm2_protocol_from(None, None, None, Some("")));
    }

    /// WezTerm renders iTerm2 inline images but implements the Kitty protocol
    /// correctly, so it must not inherit iTerm2's workaround — doing so would cost
    /// it continuous PDF stacking for a defect it does not have.
    #[test]
    fn only_iterm2_itself_needs_a_retransmit_to_move_an_image() {
        assert!(iterm2_from(Some("iTerm2 3.6.0"), None, None));
        assert!(!iterm2_from(Some("WezTerm 20240203"), None, None));
        assert!(!iterm2_from(Some("ghostty 1.0"), None, None));
        // A terminal that answered outranks the environment, both ways.
        assert!(!iterm2_from(Some("WezTerm"), Some("iTerm.app"), None));
        assert!(iterm2_from(None, Some("iTerm.app"), None));
        assert!(iterm2_from(None, None, Some("iTerm2")));
        assert!(!iterm2_from(None, Some("WezTerm"), None));
        assert!(!iterm2_from(None, None, None));
    }

    #[test]
    fn the_protocol_override_accepts_the_documented_names() {
        assert_eq!(protocol_from_name("kitty"), Some(ProtocolType::Kitty));
        assert_eq!(protocol_from_name(" ITerm2 "), Some(ProtocolType::Iterm2));
        assert_eq!(protocol_from_name("sixel"), Some(ProtocolType::Sixel));
        assert_eq!(
            protocol_from_name("halfblocks"),
            Some(ProtocolType::Halfblocks)
        );
        assert_eq!(
            protocol_from_name("nonsense"),
            None,
            "unknown ⇒ no override"
        );
        assert_eq!(protocol_from_name(""), None);
    }

    #[test]
    fn the_cell_size_override_parses_and_rejects_nonsense() {
        assert_eq!(parse_cell_size_override("22x46"), Some((22, 46)));
        assert_eq!(parse_cell_size_override(" 11 X 24 "), Some((11, 24)));
        // A zero would divide by zero downstream; junk means "unset", not "0x0".
        assert_eq!(parse_cell_size_override("0x24"), None);
        assert_eq!(parse_cell_size_override("22x0"), None);
        assert_eq!(parse_cell_size_override("22"), None);
        assert_eq!(parse_cell_size_override(""), None);
    }
}
