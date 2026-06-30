//! The single active overlay/popup.
//!
//! At most one blocking overlay is open at any time, so the former thirteen
//! mutually-exclusive `Option<…>` fields on `App` collapse into this one enum —
//! making "two overlays open at once" unrepresentable (redesign Phase R-A; see
//! `TODO.md`). Each variant wraps the state its popup owns; the behaviour still
//! lives on `impl App` (see `app::dispatch` and the concern modules), which match
//! on `app.overlay` directly so the borrow checker keeps disjoint-field borrows.
//!
//! Two near-overlay modals stay their own `App` fields and are *not* folded in
//! here: `pending_confirm` layers above any overlay, and `dup_preview` parks the
//! resolver while a duplicate is previewed in the reader.

use crate::app::{
    AnnotState, BulkRename, CollInput, DupResolve, IgnoredView, ImageViewer, MetaEdit, Palette,
    Prompt, Settings, ShelfPicker, TagInput,
};
use crate::library::stats::LibraryStats;

/// The one overlay currently open above the Library/Reader, if any.
#[derive(Default)]
pub enum Overlay {
    /// No overlay open.
    #[default]
    None,
    /// Settings popup (was `app.settings`).
    Settings(Settings),
    /// Bottom-row text prompt — rename/move a bookmark (was `app.prompt`).
    Prompt(Prompt),
    /// Metadata editor (was `app.meta_edit`).
    MetaEdit(MetaEdit),
    /// Bulk-rename popup (was `app.bulk_rename`).
    BulkRename(BulkRename),
    /// Inline sidebar collection editor (was `app.lib_coll_edit`).
    CollEdit(CollInput),
    /// Inline tag-edit prompt (was `app.tag_edit`).
    TagEdit(TagInput),
    /// Duplicate-resolution overlay (was `app.dup_resolve`).
    DupResolve(DupResolve),
    /// Ignored-duplicate-groups manager (was `app.ignored_view`).
    IgnoredView(IgnoredView),
    /// Add-to-collection picker (was `app.shelf_picker`).
    ShelfPicker(ShelfPicker),
    /// Inline-image viewer (was `app.image_view`).
    ImageView(ImageViewer),
    /// Bookmarks overlay (was `app.annot`).
    Annot(AnnotState),
    /// Library-statistics overlay (was `app.stats`).
    Stats(LibraryStats),
    /// Command palette (was `app.palette`).
    Palette(Palette),
}
