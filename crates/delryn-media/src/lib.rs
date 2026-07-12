//! Terminal image support: protocol detection and decoding. Wraps
//! `ratatui-image` so the rest of the app doesn't depend on it directly.
//! See `DESIGN.md` §0 (graphics protocols).

mod builder;
mod cover;
mod decode;
mod kitty;
mod page;
mod profile;
mod recolor;
mod sizing;

pub use delryn_infra::config::{ImageFit, ImageMode};

pub use builder::{BuiltImage, ImageBuilder, ImagePlan, ImgKey};
pub use cover::{CoverImage, build_cover};
pub use decode::{decode, image_dimensions};
pub use kitty::{
    delete_image_seq, delete_placement_seq, detect_picker, place_image_seq, terminal_background,
    transmit_file_seq, transmit_image_seq,
};
pub use page::{PageKey, PageThemer, ThemedPage};
pub use profile::{InkProfile, ink_profile};
pub use recolor::{
    Ink, RenderPolicy, flatten_onto, is_line_art, recolor_ink, render_for_theme, theme_invert,
    theme_page_png,
};
pub use sizing::{FitBox, SizeHint, SizeSpec, target_cells};
