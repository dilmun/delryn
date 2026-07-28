//! The `Action` dispatcher: `App::apply` routes a resolved [`Action`] to its
//! effect (config toggles, navigation, overlays, persistence), with the reader
//! navigation cluster split into the free `apply_nav` so the router stays flat.
//! A layout-affecting change snapshots [`layout_key`] before/after so the reader
//! preserves its reading position across the re-wrap and repaints in full.

use super::super::*;
use crate::HighlightColor;
use crate::config::{Config, ViewMode};

/// The config knobs that change how a section wraps or how wide the reading
/// measure is. When any of them changes (a view-mode switch, a reading preset, a
/// width/spacing tweak, or toggling a piece of chrome), the section re-wraps, so
/// the reader preserves its reading position across the change via
/// [`Reader::hold_reflow_position`] and forces a full repaint.
///
/// The chrome flags belong here even though they aren't wrap *settings*: hiding
/// the sidebar or the status bar resizes the text area, which re-wraps the
/// section and moves every inline image exactly like a width change does.
#[derive(PartialEq)]
pub(crate) struct LayoutKey {
    view_mode: ViewMode,
    side_padding: u16,
    page_gap: u16,
    line_spacing: u8,
    paragraph_spacing: u8,
    justify: bool,
    tidy_spacing: bool,
    code_wrap: bool,
    table_wrap: bool,
    code_fold: bool,
    code_fold_threshold: usize,
    focus_mode: bool,
    show_sidebar: bool,
    show_status: bool,
}

pub(crate) fn layout_key(c: &Config) -> LayoutKey {
    LayoutKey {
        view_mode: c.view_mode,
        side_padding: c.side_padding,
        page_gap: c.page_gap,
        line_spacing: c.line_spacing,
        paragraph_spacing: c.paragraph_spacing,
        justify: c.justify,
        tidy_spacing: c.tidy_spacing,
        code_wrap: c.code_wrap,
        table_wrap: c.table_wrap,
        code_fold: c.code_fold,
        code_fold_threshold: c.code_fold_threshold,
        focus_mode: c.focus_mode,
        show_sidebar: c.show_sidebar,
        show_status: c.show_status,
    }
}

impl App {
    pub(super) fn apply(&mut self, action: Action) {
        // Throttle PDF flips to the display rate (see `pdf_flip_ready`): a held
        // key advances one visible page per drawn frame rather than skipping.
        let flip_ready = self.pdf_flip_ready();
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let before = reader.section;
        let mut save = false;
        // Whole-page rasters (PDF) always navigate by page flip, never eased
        // line-scroll: easing re-renders the full-page image every frame, which
        // blanks/flickers it. So page-snap whenever paged mode is on *or* the
        // document is page-image-based, regardless of the continuous-scroll knob.
        let paged = self.config.paged || reader.is_paged_image();
        // Snapshot the wrap-affecting settings so we can preserve the reading
        // position if this action re-wraps the text (see below).
        let layout_before = layout_key(&self.config);
        match action {
            Action::Quit => self.should_quit = true,
            Action::Back => {
                // Accumulate reading time for the session before leaving.
                if let (Some(start), Some(store)) = (self.session.started, &self.session.store) {
                    let secs = start.elapsed().as_secs() as i64;
                    if secs > 0 && !self.session.book_path.is_empty() {
                        store.add_read_time(&self.session.book_path, secs);
                    }
                }
                self.session.started = Some(Instant::now());
                // Drop the reader's built images; the deck frees the terminal-resident
                // ones next frame (it sees `mode != Reader`), so none linger into the
                // library view.
                reader.evict_all_images();
                self.mode = Mode::Library;
                save = true;
            }
            // Reader navigation (scroll / half- and full-page / top-bottom / goto).
            Action::Down(_)
            | Action::Up(_)
            | Action::HalfDown
            | Action::HalfUp
            | Action::PageDown
            | Action::PageUp
            | Action::Top
            | Action::Bottom
            | Action::Goto(_) => apply_nav(reader, action, paged, flip_ready),
            Action::ToggleStatus => self.config.show_status = !self.config.show_status,
            Action::CycleView => {
                self.config.view_mode = self.config.view_mode.next();
                save = true;
            }
            Action::CycleTheme => {
                // Theme is global: cycling it (here or in the library) recolours
                // every book and persists immediately to config — never per-book.
                self.config.theme = self.config.theme.next();
                self.config.save();
            }
            Action::CycleReadingMode => {
                let mode = self.config.reading_mode().next();
                self.config.apply_reading_mode(mode);
                reader.flash = Some(format!("mode: {}", mode.label()));
                save = true;
            }
            Action::ToggleFocus => self.config.focus_mode = !self.config.focus_mode,
            // `]` widens the text (less margin), `[` narrows it (more margin).
            Action::WidthUp => {
                self.config.side_padding = self.config.side_padding.saturating_sub(1);
            }
            Action::WidthDown => {
                self.config.side_padding =
                    (self.config.side_padding + 1).min(crate::config::MAX_SIDE_PADDING);
            }
            Action::LineSpacingDown => {
                self.config.line_spacing = self.config.line_spacing.saturating_sub(1);
            }
            Action::LineSpacingUp => {
                self.config.line_spacing =
                    (self.config.line_spacing + 1).min(crate::config::MAX_LINE_SPACING);
            }
            Action::ToggleSidebar => {
                self.config.show_sidebar = !self.config.show_sidebar;
                if !self.config.show_sidebar {
                    reader.focus = Focus::Content;
                }
            }
            Action::FocusToggle => {
                // Tab moves focus into the sidebar (showing it first if hidden),
                // then back to the content. Entering the sidebar, start the
                // cursor at the entry tracking the current reading position.
                if !self.config.show_sidebar {
                    self.config.show_sidebar = true;
                    reader.focus = Focus::Sidebar;
                    reader.sidebar_sel = reader.active_outline_row().unwrap_or(0);
                    reader.center_sidebar();
                } else if reader.focus == Focus::Content {
                    reader.focus = Focus::Sidebar;
                    reader.sidebar_sel = reader.active_outline_row().unwrap_or(0);
                    reader.center_sidebar();
                } else {
                    reader.focus = Focus::Content;
                }
            }
            Action::Activate => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_activate();
                } else {
                    // In the text pane, Enter follows the link cursor's anchor.
                    reader.activate_anchor();
                }
            }
            Action::NextAnchor => reader.next_anchor(),
            Action::PrevAnchor => reader.prev_anchor(),
            Action::ClearAnchor => {
                reader.clear_anchor();
            }
            Action::Expand => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_expand();
                } else if reader.continuous_paged_active() {
                    if reader.cont_pannable_x() {
                        reader.cont_pan_right(); // l pans a zoomed-in page right
                    }
                } else if reader.is_paged_image() && reader.can_pan_horizontally() {
                    reader.pan_right(1); // l pans a zoomed page right
                }
            }
            Action::Collapse => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_collapse();
                } else if reader.continuous_paged_active() {
                    if reader.cont_pannable_x() {
                        reader.cont_pan_left(); // h pans a zoomed-in page left
                    }
                } else if reader.is_paged_image() && reader.can_pan_horizontally() {
                    reader.pan_left(1); // h pans a zoomed page left
                }
            }
            Action::ZoomIn | Action::ZoomOut | Action::ZoomReset | Action::FitCycle => {
                if reader.continuous_paged_active() {
                    // Continuous scales the whole stack (both single + two-page).
                    match action {
                        Action::ZoomIn => reader.cont_zoom_in(),
                        Action::ZoomOut => reader.cont_zoom_out(),
                        Action::ZoomReset => reader.cont_zoom_reset(),
                        Action::FitCycle => {} // continuous has no fit modes
                        _ => {}
                    }
                    reader.flash = Some(
                        reader
                            .cont_zoom_label()
                            .map(|z| format!("zoom {z}"))
                            .unwrap_or_else(|| "fit width".into()),
                    );
                } else if reader.is_paged_image() && self.config.view_mode == ViewMode::Center {
                    match action {
                        Action::ZoomIn => reader.zoom_in(),
                        Action::ZoomOut => reader.zoom_out(),
                        Action::ZoomReset => reader.zoom_reset(),
                        Action::FitCycle => reader.cycle_fit(),
                        _ => {}
                    }
                    reader.flash = Some(reader.page_view.label());
                } else if reader.is_paged_image() {
                    reader.flash = Some("zoom needs single-page view (v)".into());
                }
            }
            Action::ToggleTrim => {
                if reader.is_paged_image() {
                    self.config.pdf_trim = !self.config.pdf_trim;
                    reader.flash = Some(
                        if self.config.pdf_trim {
                            "trim margins: on"
                        } else {
                            "trim margins: off"
                        }
                        .into(),
                    );
                    save = true;
                }
            }
            Action::HistBack => reader.history_back(),
            Action::HistForward => reader.history_forward(),
            // With the sidebar focused, `/` filters the contents list instead of
            // opening the in-book search — the same key, scoped to the focused pane.
            Action::Search => {
                if reader.focus == Focus::Sidebar {
                    reader.start_sidebar_filter();
                } else {
                    reader.start_search();
                }
            }
            Action::SearchNext => reader.search_next(),
            Action::SearchPrev => reader.search_prev(),
            Action::AddBookmark => {
                if let Some(store) = &self.session.store
                    && !self.session.book_path.is_empty()
                {
                    let quote = reader.current_quote();
                    // Guard against duplicates: a repeat `m` at the same anchor is a
                    // no-op (otherwise a held/double key stacks identical marks).
                    if reader.has_bookmark(reader.section, &quote) {
                        reader.flash = Some("already bookmarked".into());
                    } else {
                        store.add_bookmark(&self.session.book_path, reader.section, &quote);
                        reader.flash = Some("bookmark added".into());
                        // `reader` is borrowed here, so push the refreshed set
                        // directly rather than via the `&mut self` helper.
                        reader.set_annotations(store.list_annotations(&self.session.book_path));
                    }
                }
            }
            Action::AddNote => {
                // Capture the current line as the note's anchor, then prompt for
                // the commentary (stored on commit — see `prompt_commit`).
                if !self.session.book_path.is_empty() {
                    let quote = reader.current_quote();
                    self.overlay = Overlay::Prompt(Prompt {
                        kind: PromptKind::NewNote {
                            section: reader.section,
                            quote,
                        },
                        input: crate::ui::TextInput::from(String::new()),
                    });
                }
            }
            Action::AddHighlight => {
                if let Some(store) = &self.session.store
                    && !self.session.book_path.is_empty()
                {
                    let (section, quote) = (reader.section, reader.current_line_text());
                    // A repeat `H` at the same anchor advances the colour, then
                    // clears it — so find any existing highlight there first.
                    let existing = store
                        .list_annotations(&self.session.book_path)
                        .into_iter()
                        .find(|a| a.is_highlight() && a.section == section && a.quote == quote);
                    let current = existing
                        .as_ref()
                        .map(|a| HighlightColor::from_index(a.color));
                    match (HighlightColor::cycle(current), existing) {
                        // Advance an existing highlight to the next colour.
                        (Some(next), Some(a)) => {
                            store.set_annotation_color(a.id, next.index());
                            reader.flash = Some(format!("highlight: {}", next.label()));
                        }
                        // Add a new highlight in the first colour.
                        (Some(next), None) => {
                            store.add_highlight(
                                &self.session.book_path,
                                section,
                                &quote,
                                next.index(),
                            );
                            reader.flash = Some(format!("highlight: {}", next.label()));
                        }
                        // Past the last colour: remove the highlight.
                        (None, Some(a)) => {
                            store.delete_annotation(a.id);
                            reader.flash = Some("highlight removed".into());
                        }
                        (None, None) => {}
                    }
                    reader.set_annotations(store.list_annotations(&self.session.book_path));
                }
            }
            Action::StartSelection => reader.start_selection(),
            Action::OpenAnnotations => {
                if let Some(store) = &self.session.store {
                    let items = store.list_annotations(&self.session.book_path);
                    self.overlay = Overlay::Annot(AnnotState::new(items, AnnotTab::Bookmarks));
                }
            }
            Action::CopyCode => {
                reader.copy_visible_code();
            }
            Action::ToggleCodeWrap => {
                self.config.code_wrap = !self.config.code_wrap;
                reader.code_hscroll = 0;
                reader.flash = Some(
                    if self.config.code_wrap {
                        "code: wrap"
                    } else {
                        "code: no-wrap (< > to pan)"
                    }
                    .to_string(),
                );
                save = true;
            }
            Action::ToggleFold => {
                self.config.code_fold = !self.config.code_fold;
                reader.code_fold = self.config.code_fold;
                reader.code_fold_flip.clear(); // a clean global switch, like wrap
                // Pin the central code block so the whole-page reflow doesn't shove
                // the reader's focus (falls back to the generic fraction hold below
                // when no code is in view).
                if let Some(idx) = reader.fold_target() {
                    reader.hold_code_block(idx);
                }
                // Line counts change → inline images move; delete + rebuild their
                // Kitty placements (they composite above the cells) and repaint.
                reader.restage_visible_images();
                reader.request_repaint();
                reader.flash = Some(
                    if self.config.code_fold {
                        "code: fold long blocks"
                    } else {
                        "code: unfold all"
                    }
                    .to_string(),
                );
                save = true;
            }
            Action::ToggleFoldBlock => {
                // `F` opens the fold pick-mode. With one block in view it just
                // toggles it (each block anchors itself — see `hold_code_block`);
                // with several it badges them and awaits a digit (see `hint_key`).
                reader.flash = match reader.hint_start(HintKind::Code) {
                    HintStart::None => Some("no code block in view".into()),
                    HintStart::Single(idx) => Some(reader.toggle_fold_at(idx)),
                    HintStart::Entered(n) => Some(format!("fold: press 1–{n} · Esc")),
                };
            }
            // Horizontal panning only applies to non-wrapped code.
            Action::PanLeft => {
                reader.code_hscroll = reader.code_hscroll.saturating_sub(8);
            }
            Action::PanRight => {
                if !self.config.code_wrap {
                    reader.code_hscroll = (reader.code_hscroll + 8).min(400);
                }
            }
            Action::ToggleChapterLock => {
                self.config.chapter_lock = !self.config.chapter_lock;
                reader.flash = Some(
                    if self.config.chapter_lock {
                        "chapter lock: on"
                    } else {
                        "chapter lock: off"
                    }
                    .to_string(),
                );
                save = true;
            }
            Action::TogglePaged => {
                self.config.paged = !self.config.paged;
                if self.config.paged {
                    reader.snap_to_page(); // start on a clean page boundary
                }
                reader.flash = Some(
                    if self.config.paged {
                        "page mode: on"
                    } else {
                        "page mode: off (continuous)"
                    }
                    .to_string(),
                );
                save = true;
            }
            Action::NextChapter => reader.next_chapter(),
            Action::PrevChapter => reader.prev_chapter(),
            Action::NextElement => {
                reader.next_element();
            }
            Action::PrevElement => {
                reader.prev_element();
            }
            Action::None => {}
        }

        // A page flip while zoomed: start the new page at the top (a forward flip)
        // or bottom (a backward flip) so vertical panning reads continuously.
        if reader.is_paged_image() && reader.section != before && reader.page_zoomed() {
            reader.reset_pan_to(reader.section > before);
        }

        // If this action changed a wrap-affecting setting (view mode, width,
        // spacing, preset), the section re-wraps next frame — anchor the reading
        // position so it stays put instead of drifting to a stale line offset.
        if layout_key(&self.config) != layout_before {
            reader.hold_reflow_position();
            // The re-wrap moves every inline image, and terminal graphics don't
            // compose with the cell-diff: without a full repaint the old placements
            // linger over the new text and the diff skips cells it believes are
            // unchanged, so repeated toggles smear two layouts together.
            reader.request_repaint();
        }

        // Persist on chapter change or a settings change (cheap).
        if (save || reader.section != before)
            && let Some(store) = &self.session.store
            && !self.session.book_path.is_empty()
        {
            let _ = store.save_progress(
                &self.session.book_path,
                reader.section,
                reader.within_frac(),
                self.config.view_mode,
                self.config.theme.name,
            );
        }
    }
}

/// Reader navigation — scroll, half/full-page motion, top/bottom, and `Ng` jump.
/// In paged mode (or for page-image documents) vertical motion flips whole pages;
/// a held PDF flip is throttled to the drawn frame via `flip_ready`. Split out of
/// [`App::apply`] so its action dispatch stays a flat router.
/// Rows scrolled per `j`/`k` tap in continuous-paged (PDF stacking) mode. A held
/// key repeats at the OS rate, so speed ≈ this × repeat-rate; the deck re-places
/// pages without re-transmitting (transmit-once), so a bigger step stays smooth.
const PAGED_STEP: usize = 6;

fn apply_nav(reader: &mut Reader, action: Action, paged: bool, flip_ready: bool) {
    // Continuous-paged (PDF page stacking): vertical motion scrolls the vertical
    // page stack in row units rather than flipping whole pages. Unlike a flip it
    // needs no frame throttle — the scroll offset is absolute state, so buffered
    // key-repeats just advance it further, never skipping content — and gating it
    // on the deck (`flip_ready`) could soft-lock when the anchor scrolls into the
    // inter-page gap and so isn't among the deck's shown pages. Other actions
    // (Top/Bottom/Goto/sidebar) fall through unchanged.
    if reader.continuous_paged_active() && reader.focus == Focus::Content {
        // Home/End jump to the first / last page (a plain PDF's g/G are no-ops
        // since there are no lines to scroll; here the stack gives them meaning).
        match action {
            Action::Top => {
                reader.jump_to(0, None);
                return;
            }
            Action::Bottom => {
                let last = reader.section_count().saturating_sub(1);
                reader.jump_to(last, None);
                // Scroll-to-end reuses the last-page clamp in the down-roll math.
                reader.scroll_down(usize::MAX);
                return;
            }
            _ => {}
        }
        let half = (reader.viewport_lines.max(2) / 2) as isize;
        let full = reader.viewport_lines.max(2).saturating_sub(1) as isize;
        let delta = match action {
            Action::Down(n) => Some((n as isize).max(1) * PAGED_STEP as isize),
            Action::Up(n) => Some(-(n as isize).max(1) * PAGED_STEP as isize),
            Action::HalfDown => Some(half),
            Action::HalfUp => Some(-half),
            Action::PageDown => Some(full),
            Action::PageUp => Some(-full),
            _ => None,
        };
        if let Some(delta) = delta {
            if delta >= 0 {
                reader.scroll_down(delta as usize);
            } else {
                reader.scroll_up((-delta) as usize);
            }
            return;
        }
    }

    let page_forward = |r: &mut Reader| {
        if flip_ready {
            r.page_forward();
        }
    };
    let page_backward = |r: &mut Reader| {
        if flip_ready {
            r.page_backward();
        }
    };
    match action {
        // A count prefix (`10j`) jumps that many pages; a bare/held key flips one.
        // When the page is zoomed, pan down first and only flip at the bottom
        // edge (the new page starts at the top — reset centrally in `apply`).
        Action::Down(n) => match reader.focus {
            Focus::Content if paged => {
                if !reader.try_pan_down(n) {
                    if n > 1 {
                        reader.page_jump(n as isize);
                    } else {
                        page_forward(reader);
                    }
                }
            }
            Focus::Content => reader.queue_scroll(n as isize),
            Focus::Sidebar => reader.sidebar_move(n as isize),
        },
        Action::Up(n) => match reader.focus {
            Focus::Content if paged => {
                if !reader.try_pan_up(n) {
                    if n > 1 {
                        reader.page_jump(-(n as isize));
                    } else {
                        page_backward(reader);
                    }
                }
            }
            Focus::Content => reader.queue_scroll(-(n as isize)),
            Focus::Sidebar => reader.sidebar_move(-(n as isize)),
        },
        // Half/full-page: move the TOC cursor when the sidebar is focused, else
        // scroll (or page, for image documents) the content.
        Action::HalfDown => match reader.focus {
            Focus::Sidebar => reader.sidebar_move((reader.page_lines.max(2) / 2) as isize),
            _ if paged => page_forward(reader),
            _ => reader.scroll_down(reader.page_lines.max(2) / 2),
        },
        Action::HalfUp => match reader.focus {
            Focus::Sidebar => reader.sidebar_move(-((reader.page_lines.max(2) / 2) as isize)),
            _ if paged => page_backward(reader),
            _ => reader.scroll_up(reader.page_lines.max(2) / 2),
        },
        Action::PageDown => match reader.focus {
            Focus::Sidebar => reader.sidebar_move(reader.page_lines.max(1) as isize),
            _ if paged => page_forward(reader),
            _ => reader.scroll_down(reader.page_lines.max(1)),
        },
        Action::PageUp => match reader.focus {
            Focus::Sidebar => reader.sidebar_move(-(reader.page_lines.max(1) as isize)),
            _ if paged => page_backward(reader),
            _ => reader.scroll_up(reader.page_lines.max(1)),
        },
        Action::Top => {
            if reader.focus == Focus::Sidebar {
                reader.sidebar_sel = 0;
            } else {
                reader.scroll = 0;
            }
        }
        Action::Bottom => {
            if reader.focus == Focus::Sidebar {
                reader.sidebar_sel = reader.outline.len().saturating_sub(1);
            } else {
                reader.scroll = reader.max_scroll();
            }
        }
        // `Ng`: jump to page/section N (1-based), clamped. Records history.
        Action::Goto(n) => {
            let last = reader.section_count().saturating_sub(1);
            reader.jump_to(n.saturating_sub(1).min(last), None);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hiding the sidebar or the status bar, or entering focus mode, resizes the
    /// text area — the section re-wraps and every inline image moves. Those flags
    /// were missing from the layout key, so `f` (focus) and `z` (status) skipped
    /// both the reading-position anchor and the full repaint, and terminal graphics
    /// don't compose with the cell-diff: repeated toggles smeared two layouts
    /// together and left ghost images behind.
    #[test]
    fn chrome_toggles_count_as_layout_changes() {
        let base = Config::default();
        for (name, changed) in [
            (
                "focus_mode",
                Config {
                    focus_mode: !base.focus_mode,
                    ..Config::default()
                },
            ),
            (
                "show_sidebar",
                Config {
                    show_sidebar: !base.show_sidebar,
                    ..Config::default()
                },
            ),
            (
                "show_status",
                Config {
                    show_status: !base.show_status,
                    ..Config::default()
                },
            ),
        ] {
            assert!(
                layout_key(&changed) != layout_key(&base),
                "toggling {name} must count as a layout change"
            );
        }
    }

    /// The wrap settings still belong to the key, and an unrelated knob doesn't.
    #[test]
    fn layout_key_tracks_wrap_settings_but_not_unrelated_ones() {
        let base = Config::default();
        let wider = Config {
            side_padding: base.side_padding + 1,
            ..Config::default()
        };
        assert!(layout_key(&wider) != layout_key(&base), "width re-wraps");
        let recoloured = Config {
            theme: base.theme.next(),
            ..Config::default()
        };
        assert!(
            layout_key(&recoloured) == layout_key(&base),
            "a theme change re-colours but never re-wraps"
        );
    }
}
