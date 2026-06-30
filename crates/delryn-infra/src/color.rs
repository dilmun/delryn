//! Small colour maths shared across layers. Kept in `delryn-infra` (the
//! cross-cutting crate) so `delryn-media`'s image recolouring and the theme's
//! contrast checks compute luminance the one same way instead of each
//! re-inlining the Rec. 601 weights.

/// Perceptual luminance of an sRGB triple (Rec. 601 weights), in `0.0..=255.0`.
/// The single definition; both `delryn-media` and [`crate::theme`] call this.
pub fn luma(rgb: [u8; 3]) -> f32 {
    0.299 * rgb[0] as f32 + 0.587 * rgb[1] as f32 + 0.114 * rgb[2] as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_is_zero_white_is_full() {
        assert_eq!(luma([0, 0, 0]), 0.0);
        assert!((luma([255, 255, 255]) - 255.0).abs() < 0.01);
    }

    #[test]
    fn green_weighs_most() {
        assert!(luma([0, 255, 0]) > luma([255, 0, 0]));
        assert!(luma([255, 0, 0]) > luma([0, 0, 255]));
    }
}
