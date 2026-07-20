//! Kitty graphics protocol escape sequences and terminal capability detection.

use std::fmt::Write as _;

use ratatui_image::picker::Picker;

/// Detect the terminal's image protocol + cell size by querying stdio. Returns
/// `None` if there's no tty or detection fails (then images are unavailable).
/// Call before entering the alternate screen / raw mode.
pub fn detect_picker() -> Option<Picker> {
    // Enable the OSC 11 background-colour query so the `terminal` theme can match
    // its real backdrop (read back via [`terminal_background`]). The query ends in
    // a Device Status Report every terminal answers, so it never hangs.
    let opts = ratatui_image::picker::cap_parser::QueryStdioOptions {
        terminal_background_color_osc: true,
        ..Default::default()
    };
    Picker::from_query_stdio_with_options(opts).ok()
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
/// Deliberately **no placement id** (`p=`): placements key on the
/// (image-id, placement-id) pair, so two images sharing a placement id make the
/// second delete the first (the two-page spread's left page went blank). Without
/// `p=`, each image gets its own placement and they coexist — the approach the
/// reference kitty PDF viewer (`termpdf.py`) uses. The cursor is saved/restored
/// (`\x1b7`/`\x1b8`) so the surrounding TUI is undisturbed.
pub fn place_image_seq(
    id: u32,
    col: u16,
    row: u16,
    cols: u16,
    rows: u16,
    crop: Option<(u32, u32, u32, u32)>,
) -> String {
    let src = match crop {
        Some((x, y, w, h)) => format!(",x={x},y={y},w={w},h={h}"),
        None => String::new(),
    };
    format!("\x1b7\x1b[{row};{col}H\x1b_Ga=p,i={id},c={cols},r={rows}{src},q=2\x1b\\\x1b8")
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
}
