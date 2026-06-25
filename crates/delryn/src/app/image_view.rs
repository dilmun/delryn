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

/// Collect the renderable figures from a section's blocks into `out`, naming any
/// caption-less, alt-less figure "Figure {n}" counted across the whole list.
pub fn collect_figures(blocks: &[Block], section: usize, out: &mut Vec<Figure>) {
    // Counts every image (incl. equation/empty ones) to match `LineKind::Image`.
    let mut image_index = 0usize;
    for b in blocks {
        let Block::Image {
            alt,
            data,
            caption,
            math,
            ..
        } = b
        else {
            continue;
        };
        let idx = image_index;
        image_index += 1;
        // Skip equations-as-images (display math); the viewer is for real figures.
        if data.is_empty() || *math {
            continue;
        }
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
    /// Lazily-built protocol for the selected figure, with the (figs index,
    /// render policy) it was built for — so changing the image mode rebuilds it.
    proto: Option<StatefulProtocol>,
    proto_for: Option<(usize, RenderPolicy)>,
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
            proto: None,
            proto_for: None,
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
            self.proto = decode(&self.figs[fi].bytes).map(|img| {
                picker.new_resize_protocol(render_for_theme(&img, policy.tint, policy.mode))
            });
            self.proto_for = Some((fi, policy));
        }
        self.proto.as_mut()
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

    /// Save the selected figure to the current directory; returns a status line.
    pub fn save_current(&self) -> Option<String> {
        let fig = self.current()?;
        let ext = guess_ext(&fig.bytes);
        let base: String = fig
            .name
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .take(60)
            .collect();
        let base = base.trim_matches('_');
        let file = format!("{}.{ext}", if base.is_empty() { "figure" } else { base });
        match std::fs::write(&file, &fig.bytes) {
            Ok(()) => Some(format!(
                "saved {}",
                std::fs::canonicalize(&file)
                    .map(|p| p.display().to_string())
                    .unwrap_or(file)
            )),
            Err(e) => Some(format!("save failed: {e}")),
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
