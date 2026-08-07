//! Direct-placement delivery for inline images (equation rasters + inline figures),
//! the inline analogue of [`crate::app::page_deck::PageDeck`].
//!
//! The old delivery drew each inline image as a field of unicode-placeholder cells the
//! terminal composited an image over. That is fine for a couple of figures, but a maths
//! chapter carries hundreds of tiny equation rasters per page; compositing that many
//! placeholder-referenced images re-composites ~W·H cells per image every scroll frame —
//! slow to paint and prone to leaving stale slices behind (visible as "corruption").
//!
//! This brings the PDF-page model to inline images: each distinct image is **transmitted
//! once** (`a=t`), **placed** with `a=p` at an absolute cell, **re-placed** cheaply on
//! scroll (repeating an `(image id, placement id)` pair replaces that placement, so a
//! move needs no delete), and **freed** when it leaves the screen (`d=I`). No per-cell
//! compositing, no re-transmit on scroll, no ghosts.
//!
//! Moving via `d=i` ("drop the placement, keep the data") is not portable: iTerm2 frees
//! the image data along with it, so every inline raster vanished on the first scrolled
//! row and never returned. See [`PageDeck`](crate::app::page_deck::PageDeck).

use std::collections::{HashMap, HashSet};

use ratatui::layout::Rect;

use crate::media::{self, ImgKey};

/// A source-pixel crop `(x, y, w, h)` for a partially-visible image at a pane edge.
pub type Crop = (u32, u32, u32, u32);

/// One inline image to place this frame: which cached image (`key`), the absolute
/// terminal cell rect to place it in, and an optional source-pixel crop when it is
/// clipped by a pane edge (so a partly-scrolled image shows partially, not whole).
#[derive(Clone, Copy, PartialEq)]
pub struct InlineTarget {
    pub key: ImgKey,
    pub rect: Rect,
    pub crop: Option<Crop>,
}

/// One placement's shape: the cell box it is scaled into and the source rectangle it
/// shows. Position is excluded — an image that only *moves* re-places correctly
/// everywhere, and that is the common case a scroll must keep cheap.
type Shape = (u16, u16, Option<Crop>);

fn shape(t: &InlineTarget) -> Shape {
    (t.rect.width, t.rect.height, t.crop)
}

/// Tracks which inline images are transmitted + placed, and emits the escapes to move
/// to a new set. Images are **transmitted once**: their data stays resident and only the
/// cheap *placement* changes as the view scrolls, so a row-by-row scroll re-places a few
/// bytes per image instead of re-sending every raster.
pub struct InlineDeck {
    /// What is placed on screen right now (key + rect + crop), in draw order.
    shown: Vec<InlineTarget>,
    /// Data resident in the terminal: cache key → the Kitty image id it was given.
    resident: HashMap<ImgKey, u32>,
    /// Next id offset to hand out (monotonic within the deck's reserved id block).
    next_id: u32,
    /// The shape of every placement each resident image actually has on screen, from
    /// the last [`render`](Self::render). Two things read it: placement ids are handed
    /// out `1..=n` per image, so an image placed *fewer* times than this would strand
    /// the surplus ids; and where a move needs a re-transmit, an image whose shapes
    /// changed at all has to be re-sent rather than re-placed.
    placements: HashMap<ImgKey, Vec<Shape>>,
    /// Whether this terminal needs an image's data re-sent to redraw it at a new
    /// shape — see [`media::moves_need_retransmit`].
    retransmit_moves: bool,
    /// The last [`render`](Self::render) hit the per-frame upload cap with images still
    /// waiting — the loop should keep redrawing until they land.
    deferred: bool,
}

impl Default for InlineDeck {
    fn default() -> Self {
        Self {
            shown: Vec::new(),
            resident: HashMap::new(),
            next_id: 0,
            placements: HashMap::new(),
            retransmit_moves: media::moves_need_retransmit(),
            deferred: false,
        }
    }
}

impl InlineDeck {
    /// Base of the deck's Kitty id block. Disjoint from ratatui-image's own ids
    /// (`< KITTY_ID_MAX`) and from [`PageDeck`](crate::app::page_deck)'s block
    /// (`0x0F00_0000 + section`), so no two mechanisms fight over an id.
    const ID_BASE: u32 = 0x1000_0000;
    /// Cap on **first-time transmits** per frame (re-placing a resident image is free),
    /// so landing on a maths-dense page uploads in a few frames instead of one big stall.
    const NEW_UPLOADS_PER_FRAME: usize = 6;

    /// Nothing changed since the last frame — the caller can skip emitting escapes.
    pub fn shows(&self, targets: &[InlineTarget]) -> bool {
        self.shown.as_slice() == targets
    }

    /// Whether anything is placed (so the loop knows a [`clear`](Self::clear) is needed).
    pub fn is_empty(&self) -> bool {
        self.resident.is_empty() && self.shown.is_empty()
    }

    /// Whether the last [`render`](Self::render) left new images un-uploaded (per-frame
    /// cap), so the main loop keeps redrawing until the batch lands — no keypress needed.
    pub fn deferred(&self) -> bool {
        self.deferred
    }

    /// Reconcile the terminal to `targets`, returning the escapes to write. `png_for`
    /// yields a not-yet-resident image's PNG payload (`None` if it isn't built yet — it
    /// is retried next frame). Frees images that left the screen, re-places moved ones
    /// (no re-transmit), and transmits new ones up to [`NEW_UPLOADS_PER_FRAME`].
    pub fn render(
        &mut self,
        targets: &[InlineTarget],
        mut png_for: impl FnMut(ImgKey) -> Option<Vec<u8>>,
    ) -> Vec<String> {
        self.deferred = false;
        if self.shows(targets) {
            return Vec::new();
        }
        let now: HashSet<ImgKey> = targets.iter().map(|t| t.key).collect();
        let mut out = Vec::new();

        // Free every resident image no longer on screen (data + placements), so a
        // scrolled-away equation leaves no ghost behind.
        let leaving: Vec<(ImgKey, u32)> = self
            .resident
            .iter()
            .filter(|(k, _)| !now.contains(k))
            .map(|(k, id)| (*k, *id))
            .collect();
        for (k, id) in leaving {
            out.push(media::delete_image_seq(id));
            self.resident.remove(&k);
        }
        // Re-placing covers a move, and covers occurrence 1..=n of a repeated image, but
        // two cases it cannot cover need the image freed and re-sent:
        //
        // * placed fewer times than last frame — placements n+1.. would stay on screen
        //   with nothing to overwrite them, and there is no portable way to drop a
        //   single placement (the `d=i` that would is the one iTerm2 mis-frees);
        // * a changed shape, where the terminal won't re-render a re-placed image.
        //
        // Both stay off the scrolling path: an image that only moves keeps its shapes.
        let mut want: HashMap<ImgKey, Vec<Shape>> = HashMap::new();
        for t in targets {
            want.entry(t.key).or_default().push(shape(t));
        }
        let stale: Vec<(ImgKey, u32)> = self
            .resident
            .iter()
            .filter(|(k, _)| {
                let had = self.placements.get(*k).map_or(&[][..], Vec::as_slice);
                let now = want.get(*k).map_or(&[][..], Vec::as_slice);
                if self.retransmit_moves {
                    now != had
                } else {
                    now.len() < had.len()
                }
            })
            .map(|(k, id)| (*k, *id))
            .collect();
        for (k, id) in stale {
            out.push(media::delete_image_seq(id));
            self.resident.remove(&k);
        }

        // Place each target. The SAME image at several spots needs a DISTINCT placement id
        // per occurrence — otherwise every placement shares `p=0` and only the last shows
        // (the repeated-symbol drop-out). A not-yet-resident image is transmitted first
        // (capped per frame); a not-yet-built one is skipped and retried next frame.
        let mut placed = Vec::with_capacity(targets.len());
        let mut new_uploads = 0usize;
        let mut placement: HashMap<u32, u32> = HashMap::new(); // image id → next placement id
        for t in targets {
            let id = if let Some(&id) = self.resident.get(&t.key) {
                id
            } else {
                if new_uploads >= Self::NEW_UPLOADS_PER_FRAME {
                    self.deferred = true;
                    continue;
                }
                let Some(png) = png_for(t.key) else {
                    continue; // not built yet — retry next frame
                };
                let id = Self::ID_BASE.wrapping_add(self.next_id);
                self.next_id = self.next_id.wrapping_add(1);
                out.push(media::transmit_image_seq(id, &png));
                self.resident.insert(t.key, id);
                new_uploads += 1;
                id
            };
            let p = placement.entry(id).or_insert(0);
            *p += 1;
            // `place_image_seq` takes 1-based cell coords.
            out.push(media::place_image_seq(
                id,
                t.rect.x + 1,
                t.rect.y + 1,
                t.rect.width,
                t.rect.height,
                t.crop,
                *p,
            ));
            placed.push(*t);
        }
        // Record what was *actually* placed, not what was wanted: an image still waiting
        // on the upload cap has no placements to strand.
        self.placements.clear();
        for t in &placed {
            self.placements.entry(t.key).or_default().push(shape(t));
        }
        self.shown = placed;
        out
    }

    /// Forget what is placed on screen while keeping the transmitted image *data*
    /// resident, so the next [`render`](Self::render) re-places everything without
    /// re-transmitting. Pairs with the loop's full repaint: clearing the screen drops
    /// the terminal's *placements* but not its stored images, so without this the
    /// "nothing changed" fast path in `render` would emit nothing and the images would
    /// stay gone until some target happened to move.
    pub fn restage(&mut self) {
        self.shown.clear();
        // The repaint dropped the terminal's placements, so there are none left to
        // strand — starting the count from zero keeps a shrinking target set from
        // freeing images that no longer have surplus placements anyway.
        self.placements.clear();
    }

    /// Free every resident image (`d=I` per id) and forget all placements — for leaving
    /// the reader or a full restage. Returns the escapes to write.
    pub fn clear(&mut self) -> Vec<String> {
        let out = self
            .resident
            .values()
            .map(|id| media::delete_image_seq(*id))
            .collect();
        self.resident.clear();
        self.shown.clear();
        self.placements.clear();
        self.deferred = false; // nothing left to upload — don't keep the loop redrawing
        out
    }
}

// The deck's id block must sit above every id ratatui-image itself hands out.
const _: () = assert!(InlineDeck::ID_BASE >= ratatui_image::picker::KITTY_ID_MAX);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{ImageFit, ImageMode, ImgSlot, Ink, RenderPolicy};

    fn key(idx: usize) -> ImgKey {
        ImgKey {
            kind: ImgSlot::InlineMath,
            section: 0,
            idx,
            avail: 80,
            max_rows: 50,
            max_px: 0,
            target_pct: 85,
            math_scale: 100,
            fit_mode: ImageFit::Fit,
            policy: RenderPolicy {
                tint: Ink {
                    ink: [0, 0, 0],
                    paper: [255, 255, 255],
                },
                mode: ImageMode::default(),
            },
        }
    }

    fn target(idx: usize, x: u16, y: u16) -> InlineTarget {
        InlineTarget {
            key: key(idx),
            rect: Rect::new(x, y, 1, 1),
            crop: None,
        }
    }

    #[test]
    fn transmits_once_then_is_idle() {
        let mut deck = InlineDeck::default();
        let t = [target(0, 3, 4)];
        let first = deck.render(&t, |_| Some(vec![1, 2, 3]));
        // First frame: one transmit + one placement.
        assert!(first.iter().any(|e| e.contains("a=t")), "transmits data");
        assert!(first.iter().any(|e| e.contains("a=p")), "places it");
        // Second frame, unchanged: nothing to do.
        assert!(deck.shows(&t));
        assert!(
            deck.render(&t, |_| panic!("must not re-fetch a resident image"))
                .is_empty()
        );
    }

    /// A move is a bare re-place: repeating an `(image id, placement id)` pair
    /// replaces that placement. It must delete nothing — `d=i` claims to keep the
    /// image data but iTerm2 frees it, which blanked every inline raster on the
    /// first scrolled row and, the deck still holding it resident, for good.
    #[test]
    fn moving_re_places_without_deleting_or_re_transmitting() {
        let mut deck = InlineDeck::default();
        deck.render(&[target(0, 3, 4)], |_| Some(vec![1, 2, 3]));
        let out = deck.render(&[target(0, 3, 5)], |_| panic!("resident: no re-fetch"));
        assert!(out.iter().any(|e| e.contains("a=p")), "re-places");
        assert!(!out.iter().any(|e| e.contains("a=t")), "no re-transmit");
        assert!(
            !out.iter().any(|e| e.contains("a=d")),
            "a move deletes nothing: {out:?}"
        );
    }

    /// The blanket `d=i` sweep this replaced also cleared the *surplus* placements of
    /// an image drawn fewer times than last frame (placement ids run `1..=n` per
    /// image, so occurrences n+1.. would otherwise linger with nothing to overwrite
    /// them). Freeing the image outright is the portable way to drop them.
    #[test]
    fn fewer_occurrences_frees_the_image_so_no_placement_is_stranded() {
        let mut deck = InlineDeck::default();
        let twice = [target(0, 3, 4), target(0, 3, 8)];
        let out = deck.render(&twice, |_| Some(vec![1, 2, 3]));
        assert_eq!(
            out.iter().filter(|e| e.contains("a=p")).count(),
            2,
            "same image placed at both spots"
        );

        let once = [target(0, 3, 4)];
        let out = deck.render(&once, |_| Some(vec![1, 2, 3]));
        assert!(
            out.iter().any(|e| e.contains("d=I")),
            "the image is freed rather than stranding placement 2: {out:?}"
        );
        assert!(out.iter().any(|e| e.contains("a=t")), "and re-transmitted");
        assert!(out.iter().any(|e| e.contains("a=p")), "and re-placed once");
    }

    /// A full repaint clears the screen, which drops the terminal's placements but
    /// keeps its stored image data. Without `restage` the deck still believed those
    /// placements were live, so the unchanged-targets fast path emitted nothing and
    /// the images stayed gone — repeatedly toggling chrome smeared the screen.
    #[test]
    fn restage_re_places_everything_without_re_transmitting() {
        let mut deck = InlineDeck::default();
        let t = [target(0, 3, 4)];
        deck.render(&t, |_| Some(vec![1, 2, 3]));
        assert!(deck.shows(&t), "placed and settled");

        deck.restage();
        assert!(!deck.shows(&t), "the screen is no longer trusted");
        let out = deck.render(&t, |_| panic!("data stays resident: no re-fetch"));
        assert!(out.iter().any(|e| e.contains("a=p")), "re-places the image");
        assert!(
            !out.iter().any(|e| e.contains("a=t")),
            "without re-transmit"
        );
    }

    #[test]
    fn frees_images_that_leave_the_screen() {
        let mut deck = InlineDeck::default();
        deck.render(&[target(0, 3, 4)], |_| Some(vec![1, 2, 3]));
        let out = deck.render(&[], |_| None);
        assert!(out.iter().any(|e| e.contains("d=I")), "frees the image");
        assert!(deck.is_empty());
    }

    #[test]
    fn caps_new_uploads_per_frame_and_defers() {
        let mut deck = InlineDeck::default();
        let many: Vec<InlineTarget> = (0..InlineDeck::NEW_UPLOADS_PER_FRAME + 3)
            .map(|i| target(i, 0, i as u16))
            .collect();
        let out = deck.render(&many, |_| Some(vec![9]));
        let transmits = out.iter().filter(|e| e.contains("a=t")).count();
        assert_eq!(transmits, InlineDeck::NEW_UPLOADS_PER_FRAME, "capped");
        assert!(deck.deferred(), "flags more work pending");
    }

    #[test]
    fn same_image_at_many_spots_gets_distinct_placements() {
        // A symbol reused across a page (`ℝ`) is one cached image placed at several
        // spots at once. Each occurrence must get its own placement id — otherwise they
        // all collapse onto `p=0` and only the last shows (the repeated-symbol drop-out).
        let mut deck = InlineDeck::default();
        let k = key(0);
        let a = InlineTarget {
            key: k,
            rect: Rect::new(1, 1, 1, 1),
            crop: None,
        };
        let b = InlineTarget {
            key: k,
            rect: Rect::new(5, 1, 1, 1),
            crop: None,
        };
        let c = InlineTarget {
            key: k,
            rect: Rect::new(9, 1, 1, 1),
            crop: None,
        };
        let out = deck.render(&[a, b, c], |_| Some(vec![1]));
        // Transmitted once …
        assert_eq!(
            out.iter().filter(|e| e.contains("a=t")).count(),
            1,
            "one transmit for the shared image"
        );
        // … placed three times, each with a distinct placement id, so all three show.
        assert!(out.iter().any(|e| e.contains("p=1")), "placement p=1");
        assert!(out.iter().any(|e| e.contains("p=2")), "placement p=2");
        assert!(out.iter().any(|e| e.contains("p=3")), "placement p=3");
    }
}
