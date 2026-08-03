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
mod resize;
mod sizing;
pub mod termquery;

pub use delryn_infra::config::{ImageFit, ImageMode};

pub mod cache;
pub use builder::{
    BuiltImage, ImageBuilder, ImagePlan, ImgKey, ImgSlot, raster_cache_dir,
    raster_cache_version_dir,
};
pub use cache::{DEFAULT_BUDGET_BYTES, sweep as sweep_caches};
pub use cover::{CoverImage, build_cover, decode_cover, wrap_cover};
pub use decode::{decode, image_dimensions};
pub use image::DynamicImage;
pub use kitty::{
    delete_all_images_seq, delete_image_seq, delete_placement_seq, detect_picker, place_image_seq,
    terminal_background, terminal_report, transmit_file_seq, transmit_image_seq,
};
pub use page::{PageKey, PageThemer, ThemedPage};
pub use profile::{InkProfile, ink_profile};
pub use recolor::{
    Ink, RenderPolicy, flatten_onto, is_line_art, recolor_ink, render_for_theme, theme_invert,
    theme_page_png,
};
pub use resize::{fit_to_box, resize_exact};
pub use sizing::{FitBox, SizeHint, SizeSpec, target_cells};
