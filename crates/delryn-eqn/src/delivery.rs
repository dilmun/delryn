//! Terminal delivery: the Kitty graphics escape emitters and a transmitted-set **deck**
//! that mirrors what the terminal holds. Transmit an image once, then move it on scroll by
//! re-placing (never re-transmitting), free it when it leaves the screen, and pace first-
//! time uploads by a byte budget so a math-dense page doesn't stall the terminal. Fresh
//! code — shares nothing with the old decks. See `docs/MATH-RENDERING.md`.
//!
//! The deck is stateful but pure of I/O: [`Deck::reconcile`] returns the escape strings for
//! the caller's render loop to write inside its synchronized-update frame. Whether graphics
//! are available at all is a capability query the caller owns; where they aren't, it renders
//! the [`crate::Rendered::Text`] form instead — never blank.

use std::collections::HashMap;

use base64::Engine;

/// A source-pixel crop `(x, y, w, h)` for an image partly scrolled off a pane edge.
pub type Crop = (u32, u32, u32, u32);

/// One image to show this frame: a stable identity (a render-key hash), the cell it starts
/// at, its cell footprint, and an optional crop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub key: u64,
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
    pub crop: Option<Crop>,
}

/// Chunked `a=t` transmit (store, don't display): PNG (`f=100`) as inline base64 (`t=d`),
/// split into ≤4096-byte chunks with `m=1` continuation, quiet (`q=2`).
pub fn transmit_seq(id: u32, png: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let bytes = b64.as_bytes();
    let mut out = String::new();
    let chunks = bytes.chunks(4096).collect::<Vec<_>>();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = usize::from(i + 1 < chunks.len());
        let data = std::str::from_utf8(chunk).unwrap_or("");
        if i == 0 {
            out.push_str(&format!(
                "\x1b_Ga=t,i={id},f=100,t=d,q=2,m={more};{data}\x1b\\"
            ));
        } else {
            out.push_str(&format!("\x1b_Gm={more};{data}\x1b\\"));
        }
    }
    out
}

/// `a=p` place at a cell (no placement id, so spreads coexist), optional source crop; the
/// cursor is saved/restored so the surrounding TUI is undisturbed.
pub fn place_seq(id: u32, col: u16, row: u16, cols: u16, rows: u16, crop: Option<Crop>) -> String {
    let src = match crop {
        Some((x, y, w, h)) => format!(",x={x},y={y},w={w},h={h}"),
        None => String::new(),
    };
    format!("\x1b7\x1b[{row};{col}H\x1b_Ga=p,i={id},c={cols},r={rows}{src},q=2\x1b\\\x1b8")
}

/// `a=d,d=i` — delete only the placement (keeps the image data resident; the cheap move).
pub fn delete_placement_seq(id: u32) -> String {
    format!("\x1b_Ga=d,d=i,i={id}\x1b\\")
}

/// `a=d,d=I` — delete the placement *and* free the image data (leaving the screen).
pub fn delete_image_seq(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id}\x1b\\")
}

/// `a=d,d=A` — delete every image and free all data (teardown).
pub fn delete_all_seq() -> String {
    "\x1b_Ga=d,d=A\x1b\\".to_string()
}

/// Mirrors the terminal's image set and reconciles it to a target list, emitting the escapes
/// to get there: transmit-once + cheap re-place, freeing images that leave, pacing first-time
/// uploads by a byte budget.
pub struct Deck {
    /// The reserved Kitty id block start (kept disjoint from other image users by the caller).
    id_base: u32,
    /// Image data resident in the terminal: render key → the Kitty id it was given.
    resident: HashMap<u64, u32>,
    /// What is currently placed (key → its placement), to diff against the next target set.
    shown: HashMap<u64, (u16, u16, u16, u16, Option<Crop>)>,
    next_id: u32,
    /// The last reconcile left new uploads waiting (byte budget spent) — redraw until clear.
    deferred: bool,
}

impl Deck {
    /// A deck whose Kitty ids start at `id_base` (choose a block disjoint from other users).
    pub fn new(id_base: u32) -> Deck {
        Deck {
            id_base,
            resident: HashMap::new(),
            shown: HashMap::new(),
            next_id: 0,
            deferred: false,
        }
    }

    /// Whether the last [`reconcile`](Self::reconcile) left uploads deferred (loop should redraw).
    pub fn deferred(&self) -> bool {
        self.deferred
    }

    /// Reconcile the terminal to show `targets`, returning the escapes to write. `png_for`
    /// yields a target's transmit bytes, consulted only for a key not yet resident (a
    /// not-ready image is skipped this frame and retried next). First-time uploads are paced
    /// by `byte_budget`; at least one always uploads so progress never stalls.
    pub fn reconcile(
        &mut self,
        targets: &[Target],
        mut png_for: impl FnMut(u64) -> Option<Vec<u8>>,
        byte_budget: usize,
    ) -> Vec<String> {
        self.deferred = false;
        let mut out = Vec::new();
        let want: HashMap<u64, &Target> = targets.iter().map(|t| (t.key, t)).collect();

        // Free images that left the screen (data + placement).
        let leaving: Vec<u64> = self
            .resident
            .keys()
            .copied()
            .filter(|k| !want.contains_key(k))
            .collect();
        for k in leaving {
            if let Some(id) = self.resident.remove(&k) {
                out.push(delete_image_seq(id));
            }
            self.shown.remove(&k);
        }

        let mut new_bytes = 0usize;
        let mut next_shown = HashMap::new();
        for t in targets {
            let placement = (t.col, t.row, t.cols, t.rows, t.crop);
            if let Some(&id) = self.resident.get(&t.key) {
                // Resident already: re-place only if it moved (cheap; no re-transmit).
                if self.shown.get(&t.key) != Some(&placement) {
                    out.push(delete_placement_seq(id));
                    out.push(place_seq(id, t.col, t.row, t.cols, t.rows, t.crop));
                }
                next_shown.insert(t.key, placement);
            } else {
                // New upload: pace by bytes (at least one always lands).
                if new_bytes > 0 && new_bytes >= byte_budget {
                    self.deferred = true;
                    continue;
                }
                let Some(png) = png_for(t.key) else {
                    continue; // not built yet — retry next frame (its cells stay blank)
                };
                let id = self.id_base.wrapping_add(self.next_id);
                self.next_id = self.next_id.wrapping_add(1);
                new_bytes += png.len();
                out.push(transmit_seq(id, &png));
                self.resident.insert(t.key, id);
                out.push(place_seq(id, t.col, t.row, t.cols, t.rows, t.crop));
                next_shown.insert(t.key, placement);
            }
        }
        self.shown = next_shown;
        out
    }

    /// Free every resident image (leaving the reader / teardown).
    pub fn clear(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.resident.is_empty() {
            out.push(delete_all_seq());
        }
        self.resident.clear();
        self.shown.clear();
        self.deferred = false;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tgt(key: u64, col: u16) -> Target {
        Target {
            key,
            col,
            row: 1,
            cols: 4,
            rows: 2,
            crop: None,
        }
    }

    #[test]
    fn escape_shapes() {
        assert!(transmit_seq(7, b"abcd").starts_with("\x1b_Ga=t,i=7,f=100,t=d"));
        assert!(place_seq(7, 3, 5, 4, 2, None).contains("a=p,i=7,c=4,r=2"));
        assert!(place_seq(7, 3, 5, 4, 2, Some((1, 2, 3, 4))).contains("x=1,y=2,w=3,h=4"));
        assert!(delete_placement_seq(7).contains("d=i,i=7"));
        assert!(delete_image_seq(7).contains("d=I,i=7"));
        assert!(delete_all_seq().contains("d=A"));
    }

    #[test]
    fn transmit_once_then_reuse_on_move() {
        let mut deck = Deck::new(0x1000);
        let t = [tgt(1, 2)];
        let esc = deck.reconcile(&t, |_| Some(vec![1; 10]), 1 << 20).join("");
        assert_eq!(esc.matches("a=t").count(), 1, "transmitted once");
        assert!(esc.contains("a=p"), "and placed");

        // Move it: re-place only (delete old placement + place), no re-transmit.
        let moved = [tgt(1, 9)];
        let esc = deck
            .reconcile(
                &moved,
                |_| panic!("must not re-fetch a resident image"),
                1 << 20,
            )
            .join("");
        assert_eq!(esc.matches("a=t").count(), 0, "no re-transmit on scroll");
        assert!(
            esc.contains("d=i") && esc.contains("a=p"),
            "re-placed: {esc}"
        );
    }

    #[test]
    fn leaving_image_is_freed() {
        let mut deck = Deck::new(0x1000);
        deck.reconcile(&[tgt(1, 2)], |_| Some(vec![1; 4]), 1 << 20);
        let esc = deck.reconcile(&[], |_| None, 1 << 20).join("");
        assert!(esc.contains("d=I"), "gone image frees its data: {esc}");
    }

    #[test]
    fn uploads_are_byte_paced() {
        let mut deck = Deck::new(0x1000);
        // Three 50-byte images, budget 100 → 2 this frame: the budget is checked *before*
        // adding, so uploads continue while the running total is under it (0 and 50 pass;
        // 100 blocks the third).
        let big = vec![9u8; 50];
        let targets: Vec<Target> = (0..3).map(|i| tgt(i, i as u16)).collect();
        let esc = deck
            .reconcile(&targets, |_| Some(big.clone()), 100)
            .join("");
        assert_eq!(
            esc.matches("a=t").count(),
            2,
            "byte budget caps this frame's uploads"
        );
        assert!(deck.deferred(), "the rest are deferred");
        // Next frame the resident two stay, the third uploads.
        let esc = deck
            .reconcile(&targets, |_| Some(big.clone()), 100)
            .join("");
        assert_eq!(esc.matches("a=t").count(), 1, "the remaining image lands");
        assert!(!deck.deferred());
    }

    #[test]
    fn unready_image_is_skipped_and_retried() {
        let mut deck = Deck::new(0x1000);
        let esc = deck.reconcile(&[tgt(1, 2)], |_| None, 1 << 20).join("");
        assert!(!esc.contains("a=t"), "not built yet → nothing transmitted");
        let esc = deck
            .reconcile(&[tgt(1, 2)], |_| Some(vec![1; 4]), 1 << 20)
            .join("");
        assert!(esc.contains("a=t"), "uploads once ready");
    }
}
