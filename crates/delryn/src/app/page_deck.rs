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
//! * On a turn we **delete the old pages and transmit + place the new ones**.
//!   No placement-id bookkeeping, no "move", no resident-window cache — the
//!   simplicity is the point; every bug so far came from that machinery.
//! * The swap only happens once **all** the new pages are rasterized; until
//!   then the previous pages stay up, so a turn never blanks. The page rasters
//!   are pre-loaded by the reader's neighbour prefetch, so a ready turn is a
//!   cheap re-transmit of cached PNG bytes (no re-render).
//!
//! The deck holds no terminal handle; it returns escape strings for the render
//! loop to write inside the synchronized-update frame, with the chrome.

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

/// Write `png` to a fresh temp file (in `$TMPDIR`, which Ghostty accepts) and
/// return the file-transmit escape. The terminal reads then deletes the file
/// (`t=t`), so each call uses a unique name.
fn transmit_via_file(id: u32, png: &[u8]) -> Option<String> {
    let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!("{TEMP_STEM}-{n}.png"));
    std::fs::write(&path, png).ok()?;
    Some(media::transmit_file_seq(id, &path.to_string_lossy()))
}

/// Best-effort sweep of any page temp files the terminal didn't delete (it should
/// remove `t=t` files itself). Cheap; run when tearing the deck down.
fn temp_cleanup() {
    if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(TEMP_STEM) && name.ends_with(".png") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Append a line to `/tmp/delryn-kitty.log` when `DELRYN_KITTY_LOG` is set — a
/// diagnostic for the placement decisions, since terminal graphics can't be
/// observed from tests. Zero cost when the env var is unset.
fn dbg_log(msg: &dyn std::fmt::Display) {
    if std::env::var_os("DELRYN_KITTY_LOG").is_none() {
        return;
    }
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/delryn-kitty.log")
    {
        let _ = writeln!(f, "{msg}");
    }
}

/// A page to show this frame: its section index and the absolute terminal cell
/// rect (already aspect-fitted + centred by the view) to place it in.
pub type Target = (usize, Rect);

/// Tracks which pages are currently transmitted + on screen, and emits the
/// escapes to swap to a new set.
#[derive(Default)]
pub struct PageDeck {
    /// Pages currently transmitted to and displayed by the terminal. Image id is
    /// `id(section)`.
    shown: Vec<Target>,
    /// The theme/image policy the shown pages were transmitted under. Part of the
    /// "already showing this" check so cycling the theme or image mode — which
    /// changes the *bytes* but not the (section, rect) targets — still re-transmits
    /// the re-themed pages instead of leaving the old ones up.
    shown_policy: Option<media::RenderPolicy>,
}

impl PageDeck {
    /// Per-section kitty image id, in a range clear of `ratatui-image`'s.
    fn id(section: usize) -> u32 {
        0x0F00_0000 + section as u32
    }

    /// Whether the deck is already showing exactly `targets` under `policy` — i.e.
    /// nothing to do this frame. A policy change (theme/mode) re-transmits even
    /// when the targets are unchanged, because the page *bytes* differ.
    pub fn shows(&self, targets: &[Target], policy: media::RenderPolicy) -> bool {
        self.shown.as_slice() == targets && self.shown_policy == Some(policy)
    }

    /// Whether anything is on screen (so the loop can skip a clear).
    pub fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }

    /// The sections currently placed on screen, in order — for the loop to tell
    /// whether the deck has caught up to the pages it should be showing.
    pub fn shown_sections(&self) -> Vec<usize> {
        self.shown.iter().map(|(s, _)| *s).collect()
    }

    /// Reconcile the terminal to show `targets`, returning the escapes to write.
    /// `png_for` yields a section's rasterized PNG once it's ready. If any target
    /// isn't ready yet, the current pages are left up (no escapes) so a turn
    /// never blanks; otherwise the old pages are deleted and the new ones
    /// transmitted + placed.
    pub fn render(
        &mut self,
        targets: &[Target],
        policy: media::RenderPolicy,
        mut png_for: impl FnMut(usize) -> Option<Vec<u8>>,
    ) -> Vec<String> {
        if self.shows(targets, policy) {
            return Vec::new();
        }
        // All new pages must be rasterized before we swap — else keep the
        // current pages up rather than blanking a not-yet-ready slot.
        let mut pngs = Vec::with_capacity(targets.len());
        for &(sec, _) in targets {
            match png_for(sec) {
                Some(png) => pngs.push(png),
                None => return Vec::new(),
            }
        }

        let mut out = Vec::new();
        for (sec, _) in &self.shown {
            out.push(media::delete_image_seq(Self::id(*sec)));
        }
        for (&(sec, rect), png) in targets.iter().zip(&pngs) {
            out.push(transmit_seq(Self::id(sec), png));
            out.push(media::place_image_seq(
                Self::id(sec),
                rect.x + 1,
                rect.y + 1,
                rect.width,
                rect.height,
            ));
        }
        self.shown = targets.to_vec();
        self.shown_policy = Some(policy);

        dbg_log(&format!(
            "show {:?} mode={:?} paper={:?} ({} escapes)",
            targets
                .iter()
                .map(|(s, r)| (*s, r.x, r.y, r.width, r.height))
                .collect::<Vec<_>>(),
            policy.mode,
            policy.tint.paper,
            out.len(),
        ));
        out
    }

    /// Remove every page image from the terminal (on leaving the reader / exit).
    pub fn clear(&mut self) -> Vec<String> {
        let out = self
            .shown
            .iter()
            .map(|(s, _)| media::delete_image_seq(Self::id(*s)))
            .collect();
        self.shown.clear();
        self.shown_policy = None;
        temp_cleanup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16) -> Rect {
        Rect::new(x, 0, 40, 50)
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
            .render(&[(10, rect(0)), (11, rect(40))], auto(), |s| {
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
        assert_eq!(deck.shown, vec![(10, rect(0)), (11, rect(40))]);
    }

    /// A turn deletes the old pages, then transmits + places the new ones.
    #[test]
    fn turn_deletes_old_then_shows_new() {
        let mut deck = PageDeck::default();
        deck.render(&[(10, rect(0)), (11, rect(40))], auto(), |s| {
            Some(vec![s as u8; 4])
        });
        let esc = deck
            .render(&[(11, rect(0)), (12, rect(40))], auto(), |s| {
                Some(vec![s as u8; 4])
            })
            .join("");
        assert!(esc.contains("a=d"), "old pages deleted");
        assert_eq!(esc.matches("a=p").count(), 2, "new spread placed");
        assert_eq!(deck.shown, vec![(11, rect(0)), (12, rect(40))]);
    }

    /// If any new page isn't rasterized yet, keep the old pages up (no blank).
    #[test]
    fn keeps_old_pages_until_all_ready() {
        let mut deck = PageDeck::default();
        deck.render(&[(10, rect(0))], auto(), |_| Some(vec![1, 2, 3, 4]));
        // Page 11 not ready (png_for None for it): no swap, old page stays.
        let esc = deck.render(&[(11, rect(0)), (12, rect(40))], auto(), |s| {
            (s == 11).then(|| vec![0; 4])
        });
        assert!(esc.is_empty(), "no escapes while a target page is unready");
        assert_eq!(deck.shown, vec![(10, rect(0))], "old page still shown");
    }

    /// Re-rendering the same targets under the same policy is a no-op (the images
    /// persist on screen).
    #[test]
    fn unchanged_targets_emit_nothing() {
        let mut deck = PageDeck::default();
        deck.render(&[(10, rect(0))], auto(), |s| Some(vec![s as u8; 4]));
        let esc = deck.render(&[(10, rect(0))], auto(), |s| Some(vec![s as u8; 4]));
        assert!(esc.is_empty());
    }

    /// Cycling the theme / image mode changes the page *bytes* but not the
    /// (section, rect) targets — the deck must still re-transmit the re-themed page
    /// rather than leaving the stale one up.
    #[test]
    fn policy_change_retransmits_same_targets() {
        let mut deck = PageDeck::default();
        deck.render(&[(10, rect(0))], auto(), |s| Some(vec![s as u8; 4]));
        let esc = deck
            .render(
                &[(10, rect(0))],
                policy(media::ImageMode::InvertBackgrounds),
                |s| Some(vec![s as u8; 4]),
            )
            .join("");
        assert!(esc.contains("a=d"), "old page deleted before re-transmit");
        assert!(esc.contains("a=t"), "re-themed page re-transmitted");
        assert!(esc.contains("a=p"), "and re-placed");
    }
}
