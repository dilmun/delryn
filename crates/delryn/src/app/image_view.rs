//! The image viewer's model: the figures gathered for it, the selection /
//! filter / scope state, and the lazily-built protocol for the shown image.
//! Pure view-model — terminal I/O lives in `view::image`.

use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::document::Block;
use crate::media::{RenderPolicy, decode, image_dimensions, render_for_theme};

/// One figure available in the viewer: its display name, source location, raw
/// bytes (for save + protocol build), and pixel size.
pub struct Figure {
    /// Section (chapter) the figure lives in.
    pub section: usize,
    /// The figure's index among its section's images (matches `LineKind::Image`),
    /// so jumping lands on the figure rather than the chapter top.
    pub image_index: usize,
    /// Display name: caption → alt text → "Figure N".
    pub name: String,
    /// Full caption text (shown under the image; empty when none).
    pub caption: String,
    /// Raw encoded image bytes.
    pub bytes: Vec<u8>,
    /// Pixel dimensions, if decodable.
    pub dims: Option<(u32, u32)>,
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

/// Collect the renderable figures from a section's blocks into `out`, naming any
/// caption-less, alt-less figure "Figure {n}" counted across the whole list.
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
            format!("Figure {}", out.len() + 1)
        };
        out.push(Figure {
            section,
            image_index: idx,
            name,
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
    /// Lazily-built protocol for the selected figure, with the (figs index,
    /// render policy) it was built for — so changing the image mode rebuilds it.
    proto: Option<StatefulProtocol>,
    proto_for: Option<(usize, RenderPolicy)>,
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
    pub fn new(figs: Vec<Figure>, whole_book: bool) -> Option<ImageViewer> {
        if figs.is_empty() {
            return None;
        }
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

    /// Build (or reuse) the protocol for the selected figure under `policy`
    /// (the active image mode — faithful / invert / auto).
    pub fn ensure_proto(
        &mut self,
        picker: &Picker,
        policy: RenderPolicy,
    ) -> Option<&mut StatefulProtocol> {
        let fi = *self.view.get(self.sel)?;
        if self.proto_for != Some((fi, policy)) {
            // Free the outgoing image's terminal data before overwriting the
            // protocol, else its Kitty id is leaked (a new random id is minted
            // below) and the resident image lingers forever.
            self.retire_current();
            self.proto = decode(&self.figs[fi].bytes).map(|img| {
                picker.new_resize_protocol(render_for_theme(&img, policy.tint, policy.mode))
            });
            self.proto_for = Some((fi, policy));
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

    /// Re-filter the list by caption / name substring (case-insensitive).
    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
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
        self.proto_for = None; // selection may now point at a different figure
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

    fn fig(name: &str) -> Figure {
        Figure {
            section: 0,
            image_index: 0,
            name: name.to_string(),
            caption: String::new(),
            bytes: png_bytes(),
            dims: Some((4, 4)),
        }
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

        let id_a = id_of(viewer.ensure_proto(&picker, policy(ImageMode::Faithful)));
        assert!(
            viewer.take_deletes().is_empty(),
            "nothing to free on first build"
        );

        let id_b = id_of(viewer.ensure_proto(&picker, policy(ImageMode::InvertBackgrounds)));
        assert_ne!(id_a, id_b, "mode change mints a new id");
        assert_eq!(
            viewer.take_deletes(),
            vec![id_a],
            "old image queued for deletion"
        );

        // Re-rendering the same mode neither rebuilds nor deletes.
        viewer.ensure_proto(&picker, policy(ImageMode::InvertBackgrounds));
        assert!(viewer.take_deletes().is_empty());

        // Closing frees the last shown image too.
        viewer.close();
        assert_eq!(viewer.take_deletes(), vec![id_b]);
    }

    // Navigating to another figure must free the one we moved off.
    #[test]
    fn navigating_between_figures_frees_the_previous_image() {
        let picker = kitty_picker();
        let mut viewer = ImageViewer::new(vec![fig("a"), fig("b")], false).unwrap();
        let pol = policy(ImageMode::Faithful);

        let id0 = id_of(viewer.ensure_proto(&picker, pol));
        let _ = viewer.take_deletes();

        viewer.move_sel(1);
        let id1 = id_of(viewer.ensure_proto(&picker, pol));
        assert_ne!(id0, id1);
        assert_eq!(viewer.take_deletes(), vec![id0]);
    }
}
