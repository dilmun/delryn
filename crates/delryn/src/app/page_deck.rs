//! Direct Kitty management of full PDF page images — the `termpdf.py` model.
//!
//! Inline figures flow with text and go through `ratatui-image`'s
//! unicode-placeholder path. A full PDF page is different: it's the whole
//! viewport and it's swapped wholesale on a page turn. So pages bypass
//! `ratatui-image` and drive the kitty graphics protocol directly, the way the
//! reference terminal PDF viewer (`termpdf.py`) does:
//!
//! * Each shown page is **transmitted** (`a=t`, image id = a per-section value)
//!   and **placed** (`a=p`) with **no placement id** — so two pages of a spread
//!   coexist instead of one deleting the other's placement.
//! * Pages are **transmitted once**: their data stays resident in the terminal
//!   (tracked per section + policy), so moving a page — a page turn, or a
//!   row-by-row continuous scroll that re-crops it every frame — only deletes the
//!   old *placement* (`d=i`, keeps the data) and re-places, never re-sending the
//!   multi-MB raster. Data is re-sent only when a page first appears or its
//!   theme/mode changes; a page scrolled out of view has its data freed (`d=I`).
//! * A page is only transmitted once its raster is ready; a page needing
//!   (re)transmit that isn't ready holds the whole frame (no escapes), so a turn
//!   never blanks. Rasters are pre-loaded by the reader's neighbour prefetch.
//!
//! The deck holds no terminal handle; it returns escape strings for the render
//! loop to write inside the synchronized-update frame, with the chrome.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::layout::Rect;

use crate::media;

/// Monotonic counter for unique page temp-file names.
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Filename stem for page temp files. **Must** contain `tty-graphics-protocol`:
/// the kitty graphics spec requires it in the path before a terminal will read
/// (and delete) a `t=t` temporary file, and Ghostty enforces it — without it the
/// transmit is silently rejected and the page never appears (a black screen).
const TEMP_STEM: &str = "tty-graphics-protocol-delryn";

/// Whether to force the inline (`t=d`) transmit medium instead of the temp-file
/// one — an escape hatch (`DELRYN_KITTY_DIRECT`) for terminals that don't support
/// file transmission. Read once.
fn use_direct() -> bool {
    use std::sync::OnceLock;
    static DIRECT: OnceLock<bool> = OnceLock::new();
    *DIRECT.get_or_init(|| std::env::var_os("DELRYN_KITTY_DIRECT").is_some())
}

/// Transmit `png` under `id`, preferring the temp-file medium so a turn pushes a
/// tiny escape (the file path) instead of multi-MB of base64 — the ~60ms-per-turn
/// stall that made held `j`/`k` drag. Falls back to inline `t=d` if the file
/// can't be written or the escape hatch is set.
fn transmit_seq(id: u32, png: &[u8]) -> String {
    if !use_direct()
        && let Some(seq) = transmit_via_file(id, png)
    {
        return seq;
    }
    media::transmit_image_seq(id, png)
}

/// Write `png` to a fresh temp file and return the file-transmit escape. The
/// terminal reads then deletes the file (`t=t`), so each call uses a unique name.
///
/// These live in [`paths::runtime_dir`] — our own `0700` directory — rather than
/// straight in `$TMPDIR`. On Linux that is the shared `/tmp`, where a predictable
/// name can be pre-created as a symlink by another local user and this truncating
/// write would follow it. The kitty spec's requirement that the *path* contain
/// `tty-graphics-protocol` is still met by the filename.
fn transmit_via_file(id: u32, png: &[u8]) -> Option<String> {
    let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let path = crate::paths::runtime_dir().join(format!("{TEMP_STEM}-{n}.png"));
    std::fs::write(&path, png).ok()?;
    Some(media::transmit_file_seq(id, &path.to_string_lossy()))
}

/// Best-effort sweep of any page temp files the terminal didn't delete (it should
/// remove `t=t` files itself). Cheap; run when tearing the deck down. Scoped to our
/// own runtime directory, so it can only ever remove files we wrote.
fn temp_cleanup() {
    if let Ok(entries) = std::fs::read_dir(crate::paths::runtime_dir()) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(TEMP_STEM) && name.ends_with(".png") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Append a line to `<runtime dir>/delryn-kitty.log` when `DELRYN_KITTY_LOG` is
/// set — a diagnostic for the placement decisions, since terminal graphics can't
/// be observed from tests. Zero cost when the env var is unset.
fn dbg_log(msg: &dyn std::fmt::Display) {
    if std::env::var_os("DELRYN_KITTY_LOG").is_none() {
        return;
    }
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::paths::runtime_dir().join("delryn-kitty.log"))
    {
        let _ = writeln!(f, "{msg}");
    }
}

/// A page to show this frame: its section index, the absolute terminal cell rect
/// (already aspect-fitted + centred by the view) to place it in, and an optional
/// source-pixel crop `(x, y, w, h)` of the raster (zoom/pan shows a sub-window;
/// `None` = the whole page).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageTarget {
    pub section: usize,
    pub rect: Rect,
    pub crop: Option<(u32, u32, u32, u32)>,
    /// Width of the raster `crop` is expressed in — the same width
    /// [`page_png`](crate::app::Reader::page_png) will serve for this section.
    ///
    /// A crop is meaningless without knowing which raster it indexes, and zooming
    /// swaps the base raster for a wider crisp one (`reader::crisp`), so this is
    /// part of the page's *data* identity, not of its placement.
    pub raster_w: u32,
}

/// Tracks which pages are transmitted + on screen, and emits the escapes to move
/// to a new set. Pages are **transmitted once**: their raster data stays resident
/// in the terminal and only the cheap *placement* changes as the view scrolls, so a
/// row-by-row continuous scroll re-places a few bytes per page instead of re-sending
/// multi-MB rasters every frame.
#[derive(Default)]
pub struct PageDeck {
    /// The placements currently on screen (section + cell rect + crop). Drives the
    /// "already showing this" check and the loop's caught-up check.
    shown: Vec<PageTarget>,
    /// Image data resident in the terminal, per section, keyed by what makes the
    /// bytes what they are: the theme/image policy it was themed under, and the
    /// width of the raster it was rasterized at. A page stays here across scroll
    /// frames (data reused); its data is re-sent only when it first appears, its
    /// policy (theme/mode) changes, or zoom swaps in a different-resolution raster.
    resident: HashMap<usize, (media::RenderPolicy, u32)>,
    /// The policy the shown placements were rendered under (part of `shows`).
    shown_policy: Option<media::RenderPolicy>,
}

impl PageDeck {
    /// Per-section kitty image id, in the high block `0x0F00_0000..` reserved for
    /// the deck. `ratatui-image`'s widget ids (inline figures, covers, the viewer)
    /// are drawn strictly below this block (`picker::random_kitty_id`), so the two
    /// namespaces are disjoint and a delete in one can't clobber the other.
    fn id(section: usize) -> u32 {
        0x0F00_0000 + section as u32
    }

    /// Whether the deck is already showing exactly `targets` under `policy` — i.e.
    /// nothing to do this frame. A policy change (theme/mode) or a crop/position
    /// change (scroll/pan/zoom) makes the targets differ, so it re-renders.
    pub fn shows(&self, targets: &[PageTarget], policy: media::RenderPolicy) -> bool {
        self.shown.as_slice() == targets && self.shown_policy == Some(policy)
    }

    /// Whether anything is on screen (so the loop can skip a clear).
    pub fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }

    /// The sections currently placed on screen, in order — for the loop to tell
    /// whether the deck has caught up to the pages it should be showing.
    pub fn shown_sections(&self) -> Vec<usize> {
        self.shown.iter().map(|t| t.section).collect()
    }

    /// Forget what is placed on screen, keeping transmitted page data resident, so
    /// the next [`render`](Self::render) re-places it. Pairs with a full repaint —
    /// clearing the screen drops the terminal's placements, and without this the
    /// `shows` fast path would leave the page blank. See `InlineDeck::restage`.
    pub fn restage(&mut self) {
        self.shown.clear();
        self.shown_policy = None;
    }

    /// Reconcile the terminal to show `targets`, returning the escapes to write.
    /// `png_for` yields a section's themed PNG once ready — but it's only consulted
    /// for a page that needs (re)transmitting (new, or re-themed); a page already
    /// resident under `policy` is simply re-placed, no PNG needed. If a page that
    /// *does* need transmitting isn't ready, the current screen is left untouched
    /// (no escapes, no state change) so a turn never blanks.
    pub fn render(
        &mut self,
        targets: &[PageTarget],
        policy: media::RenderPolicy,
        mut png_for: impl FnMut(usize) -> Option<Vec<u8>>,
    ) -> Vec<String> {
        if self.shows(targets, policy) {
            return Vec::new();
        }
        // Readiness first (no mutation): every page needing (re)transmit must have
        // its PNG ready. A page already resident under this policy is skipped.
        let mut pngs: HashMap<usize, Vec<u8>> = HashMap::new();
        for t in targets {
            if self.resident.get(&t.section) == Some(&(policy, t.raster_w)) {
                continue; // these exact bytes are resident — no PNG needed
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = pngs.entry(t.section) {
                match png_for(t.section) {
                    Some(png) => {
                        slot.insert(png);
                    }
                    None => return Vec::new(),
                }
            }
        }

        // What each target needs resident, so a page whose bytes changed is freed
        // rather than re-placed with a crop that indexes a raster it no longer holds.
        let now: HashMap<usize, (media::RenderPolicy, u32)> = targets
            .iter()
            .map(|t| (t.section, (policy, t.raster_w)))
            .collect();
        let had: std::collections::HashSet<usize> = self.shown.iter().map(|t| t.section).collect();
        let mut out = Vec::new();

        // Free pages leaving the view, or holding stale bytes — a superseded theme,
        // or the base raster where the crisp one is now placed (frees their data +
        // placements); the rest keep their resident data.
        let drop: Vec<usize> = self
            .resident
            .iter()
            .filter(|(s, have)| now.get(s) != Some(have))
            .map(|(s, _)| *s)
            .collect();
        for sec in drop {
            out.push(media::delete_image_seq(Self::id(sec)));
            self.resident.remove(&sec);
        }

        // Place each target: move an already-placed resident page (delete its old
        // placement, keep data), transmit a not-yet-resident page's data, then place.
        for t in targets {
            let sec = t.section;
            if had.contains(&sec) && self.resident.contains_key(&sec) {
                out.push(media::delete_placement_seq(Self::id(sec)));
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = self.resident.entry(sec) {
                out.push(transmit_seq(
                    Self::id(sec),
                    pngs.get(&sec).expect("readiness checked above"),
                ));
                slot.insert((policy, t.raster_w));
            }
            out.push(media::place_image_seq(
                Self::id(sec),
                t.rect.x + 1,
                t.rect.y + 1,
                t.rect.width,
                t.rect.height,
                t.crop,
                0, // pages have distinct image ids; the default placement is fine
            ));
        }
        self.shown = targets.to_vec();
        self.shown_policy = Some(policy);

        dbg_log(&format!(
            "show {:?} mode={:?} paper={:?} ({} escapes, {} resident)",
            targets
                .iter()
                .map(|t| (
                    t.section,
                    t.rect.x,
                    t.rect.y,
                    t.rect.width,
                    t.rect.height,
                    t.crop
                ))
                .collect::<Vec<_>>(),
            policy.mode,
            policy.tint.paper,
            out.len(),
            self.resident.len(),
        ));
        out
    }

    /// Remove every page image from the terminal (on leaving the reader / exit).
    pub fn clear(&mut self) -> Vec<String> {
        let out = self
            .resident
            .keys()
            .map(|&s| media::delete_image_seq(Self::id(s)))
            .collect();
        self.resident.clear();
        self.shown.clear();
        self.shown_policy = None;
        temp_cleanup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The deck's id block must sit at or above the bound below which
    /// `ratatui-image` draws its random widget ids, so the two Kitty id
    /// namespaces are disjoint — a delete/transmit in one can never clobber a
    /// live image in the other.
    #[test]
    fn deck_ids_are_disjoint_from_widget_ids() {
        assert!(PageDeck::id(0) >= ratatui_image::picker::KITTY_ID_MAX);
    }

    fn rect(x: u16) -> Rect {
        Rect::new(x, 0, 40, 50)
    }

    /// The base raster width the un-zoomed targets in these tests stand for.
    const BASE_W: u32 = 1600;

    /// A whole-page (un-cropped) target at column `x`, at the base raster width.
    fn tgt(section: usize, x: u16) -> PageTarget {
        PageTarget {
            section,
            rect: rect(x),
            crop: None,
            raster_w: BASE_W,
        }
    }

    /// A page policy for the deck tests; the deck only compares it, so the colours
    /// are arbitrary.
    fn policy(mode: media::ImageMode) -> media::RenderPolicy {
        media::RenderPolicy {
            tint: media::Ink {
                ink: [0, 0, 0],
                paper: [255, 255, 255],
            },
            mode,
        }
    }

    fn auto() -> media::RenderPolicy {
        policy(media::ImageMode::Auto)
    }

    /// A ready page transmits (`a=t`) then places (`a=p`); a spread does both for
    /// each page with distinct image ids and no placement id (`p=`).
    #[test]
    fn shows_spread_transmits_and_places_each_page() {
        let mut deck = PageDeck::default();
        let esc = deck
            .render(&[tgt(10, 0), tgt(11, 40)], auto(), |s| {
                Some(vec![s as u8; 4])
            })
            .join("");
        assert_eq!(esc.matches("a=t").count(), 2, "both pages transmitted");
        assert_eq!(esc.matches("a=p").count(), 2, "both pages placed");
        assert!(!esc.contains(",p="), "no placement id (would collide)");
        assert!(
            esc.contains(&format!("i={}", PageDeck::id(10)))
                && esc.contains(&format!("i={}", PageDeck::id(11))),
            "distinct image ids per page"
        );
        assert_eq!(deck.shown, vec![tgt(10, 0), tgt(11, 40)]);
    }

    /// A turn deletes the old pages, then transmits + places the new ones.
    #[test]
    fn turn_deletes_old_then_shows_new() {
        let mut deck = PageDeck::default();
        deck.render(&[tgt(10, 0), tgt(11, 40)], auto(), |s| {
            Some(vec![s as u8; 4])
        });
        let esc = deck
            .render(&[tgt(11, 0), tgt(12, 40)], auto(), |s| {
                Some(vec![s as u8; 4])
            })
            .join("");
        assert!(esc.contains("a=d"), "old pages deleted");
        assert_eq!(esc.matches("a=p").count(), 2, "new spread placed");
        assert_eq!(deck.shown, vec![tgt(11, 0), tgt(12, 40)]);
    }

    /// If any new page isn't rasterized yet, keep the old pages up (no blank).
    #[test]
    fn keeps_old_pages_until_all_ready() {
        let mut deck = PageDeck::default();
        deck.render(&[tgt(10, 0)], auto(), |_| Some(vec![1, 2, 3, 4]));
        // Page 11 not ready (png_for None for it): no swap, old page stays.
        let esc = deck.render(&[tgt(11, 0), tgt(12, 40)], auto(), |s| {
            (s == 11).then(|| vec![0; 4])
        });
        assert!(esc.is_empty(), "no escapes while a target page is unready");
        assert_eq!(deck.shown, vec![tgt(10, 0)], "old page still shown");
    }

    /// Re-rendering the same targets under the same policy is a no-op (the images
    /// persist on screen).
    #[test]
    fn unchanged_targets_emit_nothing() {
        let mut deck = PageDeck::default();
        deck.render(&[tgt(10, 0)], auto(), |s| Some(vec![s as u8; 4]));
        let esc = deck.render(&[tgt(10, 0)], auto(), |s| Some(vec![s as u8; 4]));
        assert!(esc.is_empty());
    }

    /// Cycling the theme / image mode changes the page *bytes* but not the
    /// (section, rect) targets — the deck must still re-transmit the re-themed page
    /// rather than leaving the stale one up.
    #[test]
    fn policy_change_retransmits_same_targets() {
        let mut deck = PageDeck::default();
        deck.render(&[tgt(10, 0)], auto(), |s| Some(vec![s as u8; 4]));
        let esc = deck
            .render(
                &[tgt(10, 0)],
                policy(media::ImageMode::InvertBackgrounds),
                |s| Some(vec![s as u8; 4]),
            )
            .join("");
        assert!(esc.contains("a=d"), "old page deleted before re-transmit");
        assert!(esc.contains("a=t"), "re-themed page re-transmitted");
        assert!(esc.contains("a=p"), "and re-placed");
    }

    /// Scrolling a page (same section + policy, new crop/position) re-places it
    /// **without re-transmitting** its data — the whole point of the resident cache.
    /// `png_for` returns `None` here to prove it isn't consulted for a resident page.
    #[test]
    fn scroll_reuses_resident_data_without_resending() {
        let mut deck = PageDeck::default();
        deck.render(&[tgt(10, 0)], auto(), |s| Some(vec![s as u8; 4]));
        let moved = PageTarget {
            crop: Some((0, 10, 4, 4)),
            ..tgt(10, 0)
        };
        let esc = deck.render(&[moved], auto(), |_| None).join("");
        assert!(!esc.is_empty(), "the re-place is emitted");
        assert!(!esc.contains("a=t"), "resident page is not re-transmitted");
        assert!(esc.contains("d=i"), "old placement removed, data kept");
        assert!(esc.contains("a=p"), "re-placed at the new crop");
    }

    /// Zooming in swaps the base raster for a wider crisp one, and the new crop is
    /// in *that* raster's coordinates. Section, policy and rect can all be
    /// unchanged, so residency keyed on section+policy alone would keep the base
    /// image up and apply crisp-sized crop coordinates to it — the terminal then
    /// shows the wrong region, stretched to fill the cell box (reported as
    /// "the text gets squashed after zooming in 3 times").
    #[test]
    fn a_crisper_raster_is_retransmitted_not_cropped_against_the_old_one() {
        let mut deck = PageDeck::default();
        let base = PageTarget {
            section: 10,
            rect: rect(0),
            crop: Some((0, 0, 1600, 2070)),
            raster_w: BASE_W,
        };
        deck.render(&[base], auto(), |_| Some(vec![1; 4]));

        // Same page, same theme, same cell box — only the raster behind it changed.
        let crisp = PageTarget {
            crop: Some((410, 530, 1600, 2070)),
            raster_w: BASE_W * 2,
            ..base
        };
        let esc = deck.render(&[crisp], auto(), |_| Some(vec![2; 4])).join("");

        assert!(esc.contains("a=d"), "the stale base raster is freed");
        assert!(esc.contains("a=t"), "the crisp raster is transmitted");
        assert!(esc.contains("a=p"), "and placed");
        assert_eq!(
            deck.resident.get(&10),
            Some(&(auto(), BASE_W * 2)),
            "the deck knows which raster it now holds"
        );
    }

    /// The other half of the same rule: a pan at an unchanged raster width must
    /// still reuse the resident data (this is what makes panning cheap).
    #[test]
    fn panning_within_one_raster_still_reuses_resident_data() {
        let mut deck = PageDeck::default();
        let t = PageTarget {
            section: 10,
            rect: rect(0),
            crop: Some((0, 0, 800, 1000)),
            raster_w: BASE_W * 2,
        };
        deck.render(&[t], auto(), |_| Some(vec![1; 4]));
        let panned = PageTarget {
            crop: Some((200, 300, 800, 1000)),
            ..t
        };
        // `png_for` returns None to prove the resident data is reused.
        let esc = deck.render(&[panned], auto(), |_| None).join("");
        assert!(!esc.contains("a=t"), "no re-transmit for a pan");
        assert!(esc.contains("a=p"), "just a re-place at the new crop");
    }

    /// A cropped target (zoom/pan) places a source sub-rectangle: the placement
    /// carries the Kitty `x=/y=/w=/h=` params, and a crop change re-renders.
    #[test]
    fn cropped_target_emits_source_rectangle() {
        let mut deck = PageDeck::default();
        let t = PageTarget {
            section: 10,
            rect: rect(0),
            crop: Some((100, 200, 300, 400)),
            raster_w: BASE_W,
        };
        let esc = deck
            .render(&[t], auto(), |s| Some(vec![s as u8; 4]))
            .join("");
        assert!(
            esc.contains("x=100,y=200,w=300,h=400"),
            "crop → source rectangle; got {esc:?}"
        );
        // Panning (a different crop, same section/rect/policy) re-renders.
        assert!(!deck.shows(
            &[PageTarget {
                crop: Some((100, 260, 300, 400)),
                ..t
            }],
            auto()
        ));
    }
}
