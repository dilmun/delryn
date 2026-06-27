//! Direct Kitty management of full PDF page images — the `icat` model.
//!
//! Inline figures flow with text and are rendered through `ratatui-image`'s
//! unicode-placeholder path. A full PDF page is different: it's the whole
//! viewport, it's swapped wholesale on a page turn, and it must never flash.
//!
//! So pages bypass `ratatui-image` entirely. Each page is **transmitted once**
//! to the terminal as a stored image (`a=t`) and **displayed by placement**
//! (`a=p`), which re-uses the stored data — exactly how kitty's own `icat`
//! works. Two properties this buys us:
//!
//! * **No re-transmit on a turn** — the page is already in the terminal, so the
//!   placement is instant.
//! * **No black gap** — on a turn we place the new page *before* removing the
//!   old one's placement, and if the new page isn't transmitted yet we simply
//!   leave the old one up. The previous page always stays on screen until the
//!   next one is ready.
//!
//! The deck holds no terminal handle; it returns escape strings for the render
//! loop to write (inside the synchronized-update frame, with the chrome).

use std::collections::HashSet;

use ratatui::layout::Rect;

use crate::media;

/// A page to show this frame: its section index and the absolute terminal cell
/// rect (already aspect-fitted + centred by the view) to place it in.
pub type Target = (usize, Rect);

/// Tracks which pages are resident in the terminal and which are on screen, and
/// emits the minimal escapes to keep that in sync with what the view wants.
pub struct PageDeck {
    /// Base for per-section image ids, kept clear of `ratatui-image`'s range.
    base: u32,
    /// Sections whose PNG data is currently stored in the terminal.
    resident: HashSet<usize>,
    /// Sections currently placed on screen, with where (so we can unplace them).
    on_screen: Vec<Target>,
}

impl Default for PageDeck {
    fn default() -> Self {
        PageDeck {
            base: 0x0F00_0000,
            resident: HashSet::new(),
            on_screen: Vec::new(),
        }
    }
}

impl PageDeck {
    fn id(&self, section: usize) -> u32 {
        self.base + section as u32
    }

    /// Reconcile the terminal with the view's intent and return the escapes to
    /// write. `targets` are the page(s) to show now; `window` is the range of
    /// sections to keep resident (warm) for instant turns; `png_for` yields a
    /// section's rasterized PNG once it's ready (`None` while still loading).
    pub fn render(
        &mut self,
        targets: &[Target],
        window: std::ops::Range<usize>,
        mut png_for: impl FnMut(usize) -> Option<Vec<u8>>,
    ) -> Vec<String> {
        let mut out = Vec::new();

        // 1. Transmit (store, invisibly) any windowed page not yet resident.
        for sec in window.clone() {
            if !self.resident.contains(&sec)
                && let Some(png) = png_for(sec)
            {
                out.push(media::transmit_image_seq(self.id(sec), &png));
                self.resident.insert(sec);
            }
        }

        // 2. The targets we can actually show now (data is resident).
        let ready: Vec<Target> = targets
            .iter()
            .copied()
            .filter(|(sec, _)| self.resident.contains(sec))
            .collect();

        // 3. Update placements only when the visible set changed — otherwise the
        //    placed images persist on their own (no per-frame re-emit). If none
        //    of the new targets are ready yet, leave the previous page up (the
        //    `!ready.is_empty()` guard) so a turn never blanks.
        if !ready.is_empty() && ready != self.on_screen {
            // Place the new page(s) first so they cover the old, *then* remove
            // the old placement(s) — no gap on the swap.
            for (sec, area) in &ready {
                out.push(media::place_image_seq(
                    self.id(*sec),
                    area.x + 1,
                    area.y + 1,
                    area.width,
                    area.height,
                ));
            }
            for (sec, _) in &self.on_screen {
                if !ready.iter().any(|(s, _)| s == sec) {
                    out.push(media::unplace_image_seq(self.id(*sec)));
                }
            }
            self.on_screen = ready;
        }

        // 4. Free pages that have fallen outside the window (and aren't on screen).
        let evict: Vec<usize> = self
            .resident
            .iter()
            .copied()
            .filter(|s| !window.contains(s) && !self.on_screen.iter().any(|(os, _)| os == s))
            .collect();
        for s in evict {
            out.push(media::delete_image_seq(self.id(s)));
            self.resident.remove(&s);
        }

        out
    }

    /// Remove every page image from the terminal (on leaving the reader / exit).
    /// Returns the escapes to write; the deck is left empty.
    pub fn clear(&mut self) -> Vec<String> {
        let out = self
            .resident
            .drain()
            .map(|s| media::delete_image_seq(self.base + s as u32))
            .collect();
        self.on_screen.clear();
        out
    }

    /// Whether anything is currently resident (so the loop can skip a clear).
    pub fn is_empty(&self) -> bool {
        self.resident.is_empty()
    }

    /// Whether `section`'s page data is already stored in the terminal.
    pub fn is_resident(&self, section: usize) -> bool {
        self.resident.contains(&section)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::new(0, 0, 40, 50)
    }

    #[test]
    fn transmits_then_places_within_window() {
        let mut deck = PageDeck::default();
        let esc = deck.render(&[(0, rect())], 0..3, |s| Some(vec![s as u8; 4]));
        let joined = esc.join("");
        // Page 0 transmitted (a=t) and placed (a=p); window pages 1,2 transmitted.
        assert!(joined.contains("a=t"), "should transmit");
        assert!(joined.contains("a=p"), "should place the target");
        assert!(deck.resident.contains(&0) && deck.resident.contains(&2));
        assert_eq!(deck.on_screen, vec![(0, rect())]);
    }

    #[test]
    fn turn_places_new_before_unplacing_old_no_blank() {
        let mut deck = PageDeck::default();
        // Warm pages 0 and 1.
        deck.render(&[(0, rect())], 0..3, |s| Some(vec![s as u8; 4]));
        // Turn to page 1: both already resident → place 1, then unplace 0.
        let esc = deck
            .render(&[(1, rect())], 0..4, |s| Some(vec![s as u8; 4]))
            .join("");
        let place_new = esc.find("a=p");
        let unplace_old = esc.find("d=i,i="); // unplace keeps data (lowercase d=i)
        assert!(place_new.is_some() && unplace_old.is_some());
        assert!(
            place_new < unplace_old,
            "new page placed before old removed"
        );
        assert_eq!(deck.on_screen, vec![(1, rect())]);
    }

    #[test]
    fn keeps_old_page_until_new_is_ready() {
        let mut deck = PageDeck::default();
        deck.render(&[(0, rect())], 0..2, |_| Some(vec![1, 2, 3, 4]));
        // Turn to page 5, which isn't loaded yet (png_for returns None): the old
        // page must stay on screen (no unplace), so there's no black gap.
        let esc = deck.render(&[(5, rect())], 4..7, |_| None);
        assert!(
            !esc.iter().any(|e| e.contains("a=p")),
            "nothing new to place"
        );
        assert_eq!(deck.on_screen, vec![(0, rect())], "old page stays up");
    }
}
