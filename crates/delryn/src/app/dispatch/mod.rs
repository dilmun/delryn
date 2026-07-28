//! Input dispatch: the central key router (`on_key`). The modal overlay key
//! handlers live in `overlays`, the `Action` dispatcher in `apply` — this module
//! is the entry point that reads the current focus/overlay and delegates.

use super::*;

mod apply;
pub(crate) use apply::{LayoutKey, layout_key};
mod overlays;

impl App {
    /// Handle the overlay resize key: `f` on a bordered window that isn't typing,
    /// or `Ctrl-f` on any bordered window. Returns whether it consumed the key.
    fn try_overlay_resize(&mut self, key: KeyEvent) -> bool {
        if !matches!(key.code, KeyCode::Char('f')) || !self.overlay.is_resizable_window() {
            return false;
        }
        let ctrl = key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL);
        let bare = key.modifiers.is_empty();
        if ctrl || (bare && !self.overlay_is_typing()) {
            self.overlay_large = !self.overlay_large;
            true
        } else {
            false
        }
    }

    /// Whether the active overlay is currently capturing typed text (so a bare
    /// `f` inserts the character instead of resizing the window).
    fn overlay_is_typing(&self) -> bool {
        match &self.overlay {
            Overlay::Palette(_) | Overlay::BulkRename(_) => true,
            Overlay::ShelfPicker(p) => p.new_name.is_some(),
            Overlay::Annot(a) => a.filtering,
            Overlay::MetaEdit(e) => e.is_typing(),
            // The Sources tab's inline "add folder" path input.
            Overlay::Settings(s) => s.adding.is_some(),
            _ => false,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        // A pending yes/no confirmation is modal: it answers before any popup.
        if self.pending_confirm.is_some() {
            self.confirm_key(key);
            return;
        }
        // Any bordered overlay window: `f` toggles compact ⇄ larger size (Ctrl-f
        // also works while a text field is being typed into), before its own keys.
        if self.try_overlay_resize(key) {
            return;
        }
        if matches!(self.overlay, Overlay::Settings(_)) {
            self.settings_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::Prompt(_)) {
            self.prompt_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::MetaEdit(_)) {
            self.meta_edit_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::BulkRename(_)) {
            self.bulk_rename_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::CollEdit(_)) {
            self.lib_coll_edit_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::TagEdit(_)) {
            self.tag_edit_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::DupResolve(_)) {
            self.dup_resolve_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::IgnoredView(_)) {
            self.ignored_view_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::ShelfPicker(_)) {
            self.shelf_picker_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::ImageView(_)) {
            self.image_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::Annot(_)) {
            self.annot_key(key);
            return;
        }
        // The stats overlay is read-only: any key dismisses it.
        if matches!(self.overlay, Overlay::Stats(_)) {
            self.overlay = Overlay::None;
            return;
        }
        if matches!(self.overlay, Overlay::Palette(_)) {
            self.palette_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::WordLookup(_)) {
            self.word_lookup_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::CodeView(_)) {
            self.code_view_key(key);
            return;
        }
        // The in-book search prompt is a focused text input: it must capture
        // every key (including shortcut letters like 'i' / ';' / ':') before any
        // global shortcut below gets a chance to fire.
        if self.mode == Mode::Reader && self.reader.as_ref().is_some_and(|r| r.search.searching) {
            self.search_key(key);
            return;
        }
        // The sidebar's contents filter is a focused text input for the same reason
        // as the search prompt above — it must see letters before global shortcuts.
        if self.mode == Mode::Reader
            && self
                .reader
                .as_ref()
                .is_some_and(|r| r.sidebar_filter.is_some())
        {
            self.sidebar_filter_key(key);
            return;
        }
        // Visual (vim-style) selection captures every key as a motion/command until
        // it's committed or cancelled, so global shortcuts don't fire mid-select.
        if self.mode == Mode::Reader && self.reader.as_ref().is_some_and(|r| r.selection_active()) {
            self.visual_key(key);
            return;
        }
        // The `F`/`I` number-badge pick-mode captures digits (so they don't feed the
        // vim count) until a choice is made or it's cancelled.
        if self.mode == Mode::Reader && self.reader.as_ref().is_some_and(|r| r.hint_active()) {
            self.hint_key(key);
            return;
        }
        // ':' opens the command palette in the library.
        if self.mode == Mode::Library && key.code == KeyCode::Char(':') {
            self.open_palette();
            return;
        }
        // `I` opens the figure pick-mode (capital, matching `F` for folds).
        if self.mode == Mode::Reader && key.code == KeyCode::Char('I') {
            self.open_images();
            return;
        }
        // `O` opens the code block in view in the fullscreen code viewer.
        if self.mode == Mode::Reader && key.code == KeyCode::Char('O') {
            self.open_code_view();
            return;
        }
        if key.code == KeyCode::Char(';') {
            let scope = self.mode;
            self.overlay = Overlay::Settings(Settings {
                scope,
                tab: 0,
                row: first_setting_row(scope, 0, &self.config),
                adding: None,
                filter: None,
                query: String::new(),
            });
            return;
        }
        match self.mode {
            Mode::Reader => {
                // Clear any transient flash message on the next keypress.
                if let Some(r) = self.reader.as_mut() {
                    r.flash = None;
                }
                // While previewing a book from the duplicate resolver, Esc also
                // returns (in normal reading Esc clears the selection anchor).
                let action = if self.dup_preview.is_some() && key.code == KeyCode::Esc {
                    input::Action::Back
                } else {
                    input::map_key(key, &mut self.pending)
                };
                self.apply(action);
                // An activated external link asks for confirmation before opening.
                if let Some(url) = self.reader.as_mut().and_then(|r| r.take_pending_open()) {
                    let shown = crate::view::truncate(&url, 60);
                    self.ask_confirm(
                        &format!("Open in browser: {shown}?"),
                        super::confirm::ConfirmAction::OpenUrl(url),
                    );
                }
                // Returning to the library (Back) should reflect the latest state —
                // and restore the duplicate overlay if this was a preview.
                if self.mode == Mode::Library {
                    if let Some(dr) = self.dup_preview.take() {
                        self.overlay = Overlay::DupResolve(dr);
                    }
                    self.refresh_library();
                }
            }
            Mode::Library => {
                // Clear any transient flash (e.g. cover-embed result) on input.
                self.library.flash = None;
                self.library_key(key);
            }
        }
    }
}
