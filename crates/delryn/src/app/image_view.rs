//! The image viewer's model: the figures gathered for it, the selection /
//! filter / scope state, and the lazily-built protocol for the shown image.
//! Pure view-model — terminal I/O lives in `view::image`.

use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::document::Block;
use crate::media::{RenderPolicy, decode, fit_to_box, image_dimensions, render_for_theme};

/// One figure available in the viewer: its display name, source location, raw
/// bytes (for save + protocol build), and pixel size.
pub struct Figure {
    /// Section (chapter) the figure lives in.
    pub section: usize,
    /// The figure's index among its section's images (matches `LineKind::Image`),
    /// so jumping lands on the figure rather than the chapter top.
    pub image_index: usize,
    /// Display name: caption → alt text → the "Figure N" fallback.
    pub name: String,
    /// The source gave this figure neither a caption nor alt text, so `name` is the
    /// positional "Figure N" fallback that [`label_figures`] assigns. Recorded rather
    /// than inferred from the name, so a figure whose alt text genuinely reads
    /// "Figure 3" is never renumbered out from under it.
    pub unnamed: bool,
    /// Full caption text (shown under the image; empty when none).
    pub caption: String,
    /// Raw encoded image bytes.
    pub bytes: Vec<u8>,
    /// Pixel dimensions, if decodable.
    pub dims: Option<(u32, u32)>,
}

/// A figure's stable identity — `(section, image_index)`, unique because `image_index` is
/// section-local. Positions in `figs` shift as the whole-book scan merges sections in, so
/// anything that has to survive that (the built protocol, the selection) is keyed by this
/// rather than by a list index.
pub type FigureId = (usize, usize);

impl Figure {
    pub fn id(&self) -> FigureId {
        (self.section, self.image_index)
    }
}

/// Whether an image block is one the viewer will actually open. Display-equation
/// rasters and data-less images still consume an `image_index` — that space has to
/// stay aligned with `LineKind::Image` — but the viewer is for real figures. Anything
/// that *offers* figures to the user has to filter by this too, or it ends up pointing
/// at an element the viewer silently resolves to some other figure.
fn is_openable(block: &Block) -> bool {
    matches!(block, Block::Image { data, math, .. } if !data.is_empty() && !*math)
}

/// Every image block paired with its section-local `image_index`. The index counts
/// *all* images (equations and empty ones included) so it matches `LineKind::Image`.
fn indexed_images(blocks: &[Block]) -> impl Iterator<Item = (usize, &Block)> {
    blocks
        .iter()
        .filter(|b| matches!(b, Block::Image { .. }))
        .enumerate()
}

/// The section-local `image_index` of every figure the viewer can open, in reading
/// order. Used by the badge pick-mode so it never badges an element a digit couldn't
/// open — see [`is_openable`].
pub fn openable_image_indices(blocks: &[Block]) -> Vec<usize> {
    indexed_images(blocks)
        .filter(|(_, b)| is_openable(b))
        .map(|(idx, _)| idx)
        .collect()
}

/// Give every unnamed figure a "Figure {n}" fallback name, numbered by its position in
/// the list. Split out from [`collect_figures`] because the whole-book scope assembles
/// the list one section at a time and out of reading order, so the numbering can only be
/// settled once the list is sorted — see [`ImageViewer::merge_section`].
pub fn label_figures(figs: &mut [Figure]) {
    for (i, f) in figs.iter_mut().enumerate() {
        if f.unnamed {
            f.name = format!("Figure {}", i + 1);
        }
    }
}

/// Collect the renderable figures from a section's blocks into `out`. A caption-less,
/// alt-less figure is left **unnamed** for [`label_figures`] to number.
pub fn collect_figures(blocks: &[Block], section: usize, out: &mut Vec<Figure>) {
    for (idx, b) in indexed_images(blocks) {
        if !is_openable(b) {
            continue;
        }
        let Block::Image {
            alt, data, caption, ..
        } = b
        else {
            continue;
        };
        let caption_text = caption
            .iter()
            .map(|s| s.text.as_str())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let name = if !caption_text.is_empty() {
            caption_text.clone()
        } else if !alt.trim().is_empty() {
            alt.trim().to_string()
        } else {
            String::new() // numbered by `label_figures` once the list is in order
        };
        let unnamed = name.is_empty();
        out.push(Figure {
            section,
            image_index: idx,
            name,
            unnamed,
            caption: caption_text,
            dims: image_dimensions(data),
            bytes: data.clone(),
        });
    }
}

/// The open image viewer.
pub struct ImageViewer {
    figs: Vec<Figure>,
    /// Indices into `figs` matching the active filter, in display order.
    view: Vec<usize>,
    pub sel: usize,
    /// Whole-book figure list (true) vs. just the current chapter (false).
    pub whole_book: bool,
    pub filter: String,
    pub filtering: bool,
    /// Editing the save destination path (a prompt prefilled with the default).
    pub saving: bool,
    pub save_path: String,
    /// Lazily-built protocol for the selected figure, with the (figure identity, render
    /// policy, display box in px) it was built for — so changing the image mode, or
    /// resizing the terminal, rebuilds it. Keyed by [`FigureId`] rather than a `figs`
    /// index so a filter change or a scan merge can reorder the list without the key
    /// silently coming to mean a *different* figure.
    proto: Option<StatefulProtocol>,
    proto_for: Option<(FigureId, RenderPolicy, (u32, u32))>,
    /// Terminal (Kitty) image ids the viewer has finished with — superseded by a
    /// figure change / mode toggle / filter, or the shown image at close. Drained
    /// into the app's delete stream so each rebuild frees its old resident image;
    /// otherwise every toggle leaks one until the terminal evicts *everything*
    /// (covers included) to reclaim graphics memory. Mirrors the reader/cover
    /// caches, which delete-on-eviction.
    pending_deletes: Vec<u32>,
    /// Transient status (e.g. "saved …"), shown in the title.
    pub flash: Option<String>,
}

impl ImageViewer {
    pub fn new(mut figs: Vec<Figure>, whole_book: bool) -> Option<ImageViewer> {
        if figs.is_empty() {
            return None;
        }
        label_figures(&mut figs);
        let view = (0..figs.len()).collect();
        Some(ImageViewer {
            figs,
            view,
            sel: 0,
            whole_book,
            filter: String::new(),
            filtering: false,
            saving: false,
            save_path: String::new(),
            proto: None,
            proto_for: None,
            pending_deletes: Vec::new(),
            flash: None,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.view.is_empty()
    }

    /// Merge one scanned section's figures into the list, keeping it in reading order.
    ///
    /// The whole-book scan reports sections as they finish, so the list grows under the
    /// user while they are looking at it. Three things therefore have to survive a merge:
    /// the list stays sorted by `(section, image_index)` so it always reads in book
    /// order; the *selection* follows the figure it was on rather than its old row
    /// number; and the built protocol is untouched unless the selected figure itself
    /// changed (it is keyed by identity, so a shifted position alone can't invalidate it,
    /// and the shown image doesn't flicker as chapters land).
    ///
    /// Replaces any figures already held for that section, so a re-scan is idempotent.
    pub fn merge_section(&mut self, section: usize, figures: Vec<Figure>) {
        let anchor = self.current().map(Figure::id);
        self.figs.retain(|f| f.section != section);
        self.figs.extend(figures);
        self.figs.sort_by_key(Figure::id);
        // Numbering is positional, so it can only be settled once the list is in order.
        label_figures(&mut self.figs);
        self.refilter();
        if let Some(id) = anchor
            && let Some(pos) = self.view.iter().position(|&fi| self.figs[fi].id() == id)
        {
            self.sel = pos;
        }
    }

    /// Rebuild `view` from `figs` under the active filter, clamping the selection.
    fn refilter(&mut self) {
        let f = self.filter.to_lowercase();
        self.view = (0..self.figs.len())
            .filter(|&i| {
                f.is_empty()
                    || self.figs[i].name.to_lowercase().contains(&f)
                    || self.figs[i].caption.to_lowercase().contains(&f)
            })
            .collect();
        if self.sel >= self.view.len() {
            self.sel = self.view.len().saturating_sub(1);
        }
    }

    /// Position (1-based) and count within the current filter, for the title.
    pub fn position(&self) -> (usize, usize) {
        (self.sel + 1, self.view.len())
    }

    /// Figures in display (filtered) order, paired with their display index.
    pub fn visible(&self) -> impl Iterator<Item = (usize, &Figure)> {
        self.view
            .iter()
            .enumerate()
            .map(|(i, &fi)| (i, &self.figs[fi]))
    }

    pub fn current(&self) -> Option<&Figure> {
        self.view.get(self.sel).map(|&fi| &self.figs[fi])
    }

    /// Select the figure nearest the given section image index — so the viewer
    /// opens on the figure you're reading, not the chapter's first.
    pub fn select_image(&mut self, image_index: usize) {
        let target = image_index as isize;
        let mut best_pos = None;
        let mut best_dist = usize::MAX;
        for (pos, &fi) in self.view.iter().enumerate() {
            let dist = (self.figs[fi].image_index as isize - target).unsigned_abs();
            if dist < best_dist {
                best_dist = dist;
                best_pos = Some(pos);
            }
        }
        if let Some(pos) = best_pos {
            self.sel = pos;
        }
    }

    pub fn move_sel(&mut self, delta: isize) {
        if self.view.is_empty() {
            return;
        }
        let n = self.view.len() as isize;
        self.sel = (self.sel as isize + delta).rem_euclid(n) as usize;
    }

    /// Build (or reuse) the protocol for the selected figure under `policy` (the active
    /// image mode — faithful / invert / auto), scaled for a `box_px`-pixel display area.
    ///
    /// The figure is **fitted to the display box before it is themed**. A book figure is
    /// routinely 1–2 megapixels while the box is a few hundred pixels across, and theming
    /// is a per-pixel pass: done at source resolution it cost 20–770 ms *per arrow key*,
    /// on the render thread, which is what made stepping through figures stall. Fitting
    /// first makes that pass cost what the screen shows. It also hands `StatefulImage` a
    /// raster that already matches its area, so the widget's own resample becomes a no-op
    /// and the Lanczos3 fit here is what the reader sees — `Resize::Scale(None)` falls back
    /// to `FilterType::Nearest`, which is the wrong kernel for the text and line art book
    /// figures are made of.
    pub fn ensure_proto(
        &mut self,
        picker: &Picker,
        policy: RenderPolicy,
        box_px: (u32, u32),
    ) -> Option<&mut StatefulProtocol> {
        let fi = *self.view.get(self.sel)?;
        let id = self.figs[fi].id();
        if self.proto_for != Some((id, policy, box_px)) {
            // Free the outgoing image's terminal data before overwriting the
            // protocol, else its Kitty id is leaked (a new random id is minted
            // below) and the resident image lingers forever.
            self.retire_current();
            self.proto = decode(&self.figs[fi].bytes).map(|img| {
                let fitted = fit_to_box(&img, box_px.0, box_px.1);
                picker.new_resize_protocol(render_for_theme(&fitted, policy.tint, policy.mode))
            });
            self.proto_for = Some((id, policy, box_px));
        }
        self.proto.as_mut()
    }

    /// Queue the currently shown figure's terminal image for deletion and forget
    /// the built protocol, so the next [`ensure_proto`](Self::ensure_proto)
    /// rebuilds it. A no-op for non-Kitty protocols (no image id).
    fn retire_current(&mut self) {
        if let Some(id) = self.proto.as_ref().and_then(StatefulProtocol::image_id) {
            self.pending_deletes.push(id);
        }
        self.proto = None;
        self.proto_for = None;
    }

    /// Take the terminal image ids the viewer is done with, to feed the app's
    /// delete stream. Call [`close`](Self::close) first when tearing the viewer
    /// down so the last shown image is freed too.
    pub fn take_deletes(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.pending_deletes)
    }

    /// Prepare the viewer for teardown: retire the shown image so its terminal
    /// data is freed (drained via [`take_deletes`](Self::take_deletes)).
    pub fn close(&mut self) {
        self.retire_current();
    }

    /// Re-filter the list by caption / name substring (case-insensitive). The built
    /// protocol survives: it is keyed by figure identity, so if the selection still lands
    /// on the same figure the image is reused, and if it doesn't the key no longer
    /// matches and it rebuilds.
    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.refilter();
    }

    /// The selected figure decoded to `(width, height, RGBA)` for the clipboard.
    pub fn current_rgba(&self) -> Option<(u32, u32, Vec<u8>)> {
        let img = decode(&self.current()?.bytes)?.to_rgba8();
        let (w, h) = (img.width(), img.height());
        Some((w, h, img.into_raw()))
    }

    /// The default save path: `<Pictures|Documents|home>/<figure name>.<ext>`.
    pub fn default_save_path(&self) -> String {
        let (base, ext) = match self.current() {
            Some(f) => (sanitize(&f.name), guess_ext(&f.bytes)),
            None => ("figure".to_string(), "img"),
        };
        default_save_dir()
            .join(format!("{base}.{ext}"))
            .to_string_lossy()
            .into_owned()
    }

    /// Write the selected figure to `path` (`~` expanded, parents created);
    /// returns a status line.
    pub fn save_to(&self, path: &str) -> String {
        let Some(fig) = self.current() else {
            return "no figure".to_string();
        };
        let path = expand_tilde(path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, &fig.bytes) {
            Ok(()) => format!("saved {}", path.display()),
            Err(e) => format!("save failed: {e}"),
        }
    }
}

/// Guess a file extension from image magic bytes.
fn guess_ext(bytes: &[u8]) -> &'static str {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => "png",
        [0xFF, 0xD8, 0xFF, ..] => "jpg",
        [b'G', b'I', b'F', ..] => "gif",
        [b'R', b'I', b'F', b'F', ..] => "webp",
        _ => "img",
    }
}

/// A filesystem-safe base name from a figure name (alphanumerics + spaces→`_`).
fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .take(60)
        .collect();
    let s = s.trim_matches('_');
    if s.is_empty() {
        "figure".into()
    } else {
        s.into()
    }
}

/// The default directory for saved figures: `~/Pictures`, else `~/Documents`,
/// else the home dir, else the current directory — whatever exists.
fn default_save_dir() -> std::path::PathBuf {
    use std::path::PathBuf;
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for sub in ["Pictures", "Documents"] {
            let d = home.join(sub);
            if d.is_dir() {
                return d;
            }
        }
        return home;
    }
    PathBuf::from(".")
}

/// Expand a leading `~` (or `~/…`) to the home directory.
fn expand_tilde(path: &str) -> std::path::PathBuf {
    use std::path::PathBuf;
    if let Some(rest) = path.strip_prefix('~')
        && let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
    {
        return home.join(rest.trim_start_matches('/'));
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{ImageMode, Ink};
    use ratatui_image::picker::{Picker, ProtocolType};

    // A small opaque PNG so `decode` succeeds and a real Kitty protocol (the
    // only kind that carries an image id) gets built.
    fn png_bytes() -> Vec<u8> {
        let img = image::DynamicImage::new_rgb8(4, 4);
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    /// A picker forced to the Kitty protocol so built protocols expose image ids
    /// (headless test terminals otherwise fall back to halfblocks → no id).
    fn kitty_picker() -> Picker {
        let mut p = Picker::halfblocks();
        p.set_protocol_type(ProtocolType::Kitty);
        p
    }

    /// A figure at `(section, image_index)` — distinct coordinates matter, since that
    /// pair is the identity the built protocol and the selection are keyed by.
    fn fig_at(name: &str, section: usize, image_index: usize) -> Figure {
        Figure {
            section,
            image_index,
            name: name.to_string(),
            unnamed: false,
            caption: String::new(),
            bytes: png_bytes(),
            dims: Some((4, 4)),
        }
    }

    fn fig(name: &str) -> Figure {
        fig_at(name, 0, 0)
    }

    fn policy(mode: ImageMode) -> RenderPolicy {
        RenderPolicy {
            tint: Ink {
                ink: [0, 0, 0],
                paper: [255, 255, 255],
            },
            mode,
        }
    }

    /// A stand-in display box; the identity/lifecycle tests only care that the same box
    /// reuses a protocol and a different figure/mode mints a new one.
    const BOX: (u32, u32) = (64, 64);

    fn id_of(proto: Option<&mut StatefulProtocol>) -> u32 {
        proto
            .and_then(|p| p.image_id())
            .expect("kitty protocol carries an image id")
    }

    // Toggling the image mode rebuilds under a fresh id and must free the old
    // terminal image — the leak that made covers/inline images vanish once the
    // terminal's graphics quota filled with orphaned figures.
    #[test]
    fn mode_toggle_frees_the_previous_terminal_image() {
        let picker = kitty_picker();
        let mut viewer = ImageViewer::new(vec![fig("a")], false).unwrap();

        let id_a = id_of(viewer.ensure_proto(&picker, policy(ImageMode::Faithful), BOX));
        assert!(
            viewer.take_deletes().is_empty(),
            "nothing to free on first build"
        );

        let id_b = id_of(viewer.ensure_proto(&picker, policy(ImageMode::InvertBackgrounds), BOX));
        assert_ne!(id_a, id_b, "mode change mints a new id");
        assert_eq!(
            viewer.take_deletes(),
            vec![id_a],
            "old image queued for deletion"
        );

        // Re-rendering the same mode neither rebuilds nor deletes.
        viewer.ensure_proto(&picker, policy(ImageMode::InvertBackgrounds), BOX);
        assert!(viewer.take_deletes().is_empty());

        // Closing frees the last shown image too.
        viewer.close();
        assert_eq!(viewer.take_deletes(), vec![id_b]);
    }

    // Navigating to another figure must free the one we moved off.
    #[test]
    fn navigating_between_figures_frees_the_previous_image() {
        let picker = kitty_picker();
        let mut viewer =
            ImageViewer::new(vec![fig_at("a", 0, 0), fig_at("b", 0, 1)], false).unwrap();
        let pol = policy(ImageMode::Faithful);

        let id0 = id_of(viewer.ensure_proto(&picker, pol, BOX));
        let _ = viewer.take_deletes();

        viewer.move_sel(1);
        let id1 = id_of(viewer.ensure_proto(&picker, pol, BOX));
        assert_ne!(id0, id1);
        assert_eq!(viewer.take_deletes(), vec![id0]);
    }

    /// The whole-book scan reports sections out of reading order (nearest the reader
    /// first), so a merge has to slot them into book order rather than append.
    #[test]
    fn merged_sections_land_in_reading_order() {
        let mut v = ImageViewer::new(vec![fig_at("c3", 3, 0)], true).unwrap();
        v.merge_section(7, vec![fig_at("c7", 7, 0)]);
        v.merge_section(1, vec![fig_at("c1a", 1, 0), fig_at("c1b", 1, 1)]);
        let order: Vec<&str> = v.visible().map(|(_, f)| f.name.as_str()).collect();
        assert_eq!(order, vec!["c1a", "c1b", "c3", "c7"]);
    }

    /// The list grows under the user while they are looking at it, so the selection must
    /// follow the *figure* it was on, not its row number.
    #[test]
    fn the_selection_follows_its_figure_across_a_merge() {
        let mut v = ImageViewer::new(vec![fig_at("c5", 5, 0)], true).unwrap();
        assert_eq!(v.current().map(|f| f.name.clone()), Some("c5".into()));
        // Two earlier chapters land, pushing the selected figure down two rows.
        v.merge_section(1, vec![fig_at("c1", 1, 0)]);
        v.merge_section(2, vec![fig_at("c2", 2, 0)]);
        assert_eq!(v.sel, 2, "row number moved");
        assert_eq!(
            v.current().map(|f| f.name.clone()),
            Some("c5".into()),
            "but the selected figure did not"
        );
    }

    /// Re-reporting a section replaces its figures rather than duplicating them, so a
    /// re-scan is idempotent.
    #[test]
    fn re_merging_a_section_replaces_it() {
        let mut v = ImageViewer::new(vec![fig_at("a", 0, 0)], true).unwrap();
        v.merge_section(1, vec![fig_at("old", 1, 0)]);
        v.merge_section(1, vec![fig_at("new", 1, 0)]);
        let names: Vec<&str> = v.visible().map(|(_, f)| f.name.as_str()).collect();
        assert_eq!(names, vec!["a", "new"]);
    }

    /// "Figure N" is a positional fallback, so it renumbers as earlier sections arrive —
    /// but a figure that carries a real name (even one that looks like "Figure 3") is
    /// never touched.
    #[test]
    fn only_unnamed_figures_are_renumbered_by_a_merge() {
        let mut unnamed = fig_at("", 5, 0);
        unnamed.unnamed = true;
        // An authored name that *looks* like a fallback — it must survive renumbering.
        let mut v = ImageViewer::new(vec![unnamed, fig_at("Figure 3", 5, 1)], true).unwrap();
        assert_eq!(
            v.visible()
                .map(|(_, f)| f.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Figure 1", "Figure 3"],
            "the unnamed one is numbered by position; the authored one is left alone"
        );
        // An earlier chapter lands, pushing both down one row.
        v.merge_section(1, vec![fig_at("c1", 1, 0)]);
        assert_eq!(
            v.visible()
                .map(|(_, f)| f.name.as_str())
                .collect::<Vec<_>>(),
            vec!["c1", "Figure 2", "Figure 3"],
            "the fallback renumbers 1 -> 2 for its new position; the authored name does not"
        );
    }

    /// A merge reorders `figs`, so a protocol keyed by list position would silently start
    /// pointing at a different figure. Keyed by identity, the shown image is untouched.
    #[test]
    fn a_merge_does_not_rebuild_the_shown_image() {
        let picker = kitty_picker();
        let mut v = ImageViewer::new(vec![fig_at("c5", 5, 0)], true).unwrap();
        let pol = policy(ImageMode::Faithful);
        let id0 = id_of(v.ensure_proto(&picker, pol, BOX));
        let _ = v.take_deletes();

        v.merge_section(1, vec![fig_at("c1", 1, 0)]);
        assert_eq!(
            id_of(v.ensure_proto(&picker, pol, BOX)),
            id0,
            "same figure, same image — no flicker as chapters land"
        );
        assert!(v.take_deletes().is_empty(), "and nothing was freed");
    }

    // The figure is rasterised for a specific display box, so a resized terminal must
    // rebuild it (and free the old one) rather than stretch the raster it built for the
    // previous size.
    #[test]
    fn a_resized_display_box_rebuilds_and_frees_the_old_image() {
        let picker = kitty_picker();
        let mut viewer = ImageViewer::new(vec![fig("a")], false).unwrap();
        let pol = policy(ImageMode::Faithful);

        let id0 = id_of(viewer.ensure_proto(&picker, pol, BOX));
        let _ = viewer.take_deletes();

        // Same box: reused, nothing freed.
        assert_eq!(id_of(viewer.ensure_proto(&picker, pol, BOX)), id0);
        assert!(viewer.take_deletes().is_empty());

        let id1 = id_of(viewer.ensure_proto(&picker, pol, (128, 128)));
        assert_ne!(id0, id1, "a new box mints a new id");
        assert_eq!(viewer.take_deletes(), vec![id0]);
    }
}
