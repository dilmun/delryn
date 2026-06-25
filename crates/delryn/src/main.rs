//! delryn binary — terminal setup, the frame-paced event loop, and CLI entry.
//! All logic lives in the `delryn` library crate.

use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
use crossterm::style::Print;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::DefaultTerminal;

use delryn::app::App;
use delryn::config::Config;
use delryn::library;
use delryn::store::Store;
use delryn::view;

/// Minimum time between rendered frames (~120 fps cap).
const FRAME: Duration = Duration::from_millis(8);
/// How long to block waiting for input when there is nothing to redraw.
const IDLE: Duration = Duration::from_millis(250);

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // `delryn --add <dir>`: register a library folder, scan it, and exit.
    if matches!(args.first().map(String::as_str), Some("--add" | "-a")) {
        return add_library(args.get(1).map(String::as_str));
    }

    // `delryn --rescan`: re-read metadata for every known book (backfills new
    // fields like series/publisher for an already-indexed library), then exit.
    if matches!(args.first().map(String::as_str), Some("--rescan")) {
        let config = Config::load();
        match Store::open_default() {
            Ok(store) => {
                let n = library::rescan(&config.library_paths, &store);
                let gone = library::prune_missing(&config.library_paths, &store);
                println!("Re-indexed {n} book(s); pruned {gone} missing.");
            }
            Err(e) => eprintln!("could not open library database: {e}"),
        }
        return Ok(());
    }

    // `delryn --index`: build the full-text search index, then exit.
    if matches!(args.first().map(String::as_str), Some("--index")) {
        match Store::open_default() {
            Ok(store) => println!(
                "Full-text indexed {} book(s).",
                library::index_fulltext(&store)
            ),
            Err(e) => eprintln!("could not open library database: {e}"),
        }
        return Ok(());
    }

    // `delryn --export-annotations`: dump bookmarks/notes as Markdown, then exit.
    if matches!(
        args.first().map(String::as_str),
        Some("--export-annotations")
    ) {
        if let Ok(store) = Store::open_default() {
            let mut last_path = String::new();
            let mut last_folder: Option<String> = None;
            for (path, a) in store.all_annotations() {
                if path != last_path {
                    println!("\n# {path}\n");
                    last_path = path;
                    last_folder = None;
                }
                if last_folder.as_deref() != Some(a.folder.as_str()) {
                    let title = if a.folder.is_empty() {
                        "Bookmarks"
                    } else {
                        &a.folder
                    };
                    println!("## {title}\n");
                    last_folder = Some(a.folder.clone());
                }
                let label = if a.name.is_empty() { &a.quote } else { &a.name };
                if a.note.is_empty() {
                    println!("- §{} {label}", a.section + 1);
                } else {
                    println!("- §{} {label} — {}", a.section + 1, a.note);
                }
            }
        }
        return Ok(());
    }

    // Detect the terminal's image protocol before entering the alt screen. The
    // same query reports the terminal's background colour, which the `terminal`
    // theme uses to recolour/invert images against the real backdrop.
    let picker = delryn::media::detect_picker();
    if let Some(p) = &picker
        && let Some(bg) = delryn::media::terminal_background(p)
    {
        delryn::theme::set_terminal_background(bg);
    }

    let mut app = match args.first() {
        Some(path) => App::open_book(path)?,
        None => {
            // Clean out dead entries (deleted/moved files) so the library has no
            // un-openable duplicates. Cheap stat per book; skips offline roots.
            if let Ok(store) = Store::open_default() {
                library::prune_missing(&Config::load().library_paths, &store);
            }
            App::library()
        }
    };
    // Spawn the background image builder from the detected protocol.
    app.image_builder = picker.clone().map(delryn::media::ImageBuilder::new);
    app.picker = picker;

    // Synchronized output (DEC 2026) can be toggled off for terminals that
    // mishandle it: `DELRYN_SYNC=0 delryn …`.
    let sync = std::env::var("DELRYN_SYNC")
        .map(|v| v != "0")
        .unwrap_or(true);

    let mut terminal = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, &mut app, sync);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, app: &mut App, sync: bool) -> Result<()> {
    draw(terminal, app, sync)?;
    let mut last_draw = Instant::now();
    let mut dirty = false;
    // Tracks overlay open/closed so a toggle can force a full repaint — a closed
    // popup over an inline image otherwise leaves a ghost the cell-diff skips.
    let mut overlay_open = app.any_overlay_open();
    // Native system clipboard (reliable across terminals); OSC 52 is the
    // fallback when it's unavailable (e.g. SSH/headless).
    let mut clipboard = arboard::Clipboard::new().ok();

    while !app.should_quit {
        // Block for input — only until the next frame is due if a redraw is
        // pending or a scroll is animating, otherwise block long so an idle
        // reader costs ~0% CPU.
        let busy = app.animating()
            || app.online_active()
            || app.lib_grid_pending()
            || app.cover_pending()
            || app.preview_pending();
        let timeout = if dirty || busy {
            FRAME.saturating_sub(last_draw.elapsed())
        } else {
            IDLE
        };

        if event::poll(timeout)? {
            // Drain the whole burst before drawing.
            loop {
                match event::read()? {
                    Event::Key(key) => {
                        app.on_key(key);
                        dirty = true;
                    }
                    Event::Mouse(m) => {
                        app.on_mouse(m);
                        dirty = true;
                    }
                    Event::Resize(_, _) => dirty = true,
                    _ => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }

        // Pick up finished Open Library results (editor's Online tab).
        if app.poll_online() {
            dirty = true;
        }
        // Keep redrawing while the grid is still building visible covers.
        if app.lib_grid_pending() {
            dirty = true;
        }
        // Rebuild the detail-pane cover once the selection settles (debounced).
        if app.tick_cover() {
            dirty = true;
        }
        // Fetch the editor's Cover-tab preview when the highlighted result settles.
        app.tick_preview();

        // Ease pending scroll a few lines toward its target this frame.
        if app.step_scroll() {
            dirty = true;
        }
        // Keep drawing while a scroll eases or inline images are still building,
        // so finished images pop in without needing a keypress.
        if app.animating() {
            dirty = true;
        }

        // Free terminal-side images evicted from the cache.
        for id in app.take_image_deletes() {
            let _ = execute!(io::stdout(), Print(delryn::media::delete_image_seq(id)));
        }
        // Copy requested text to the system clipboard: native first, else OSC 52.
        if let Some(text) = app.take_clipboard() {
            let copied = clipboard
                .as_mut()
                .is_some_and(|c| c.set_text(text.clone()).is_ok());
            if !copied {
                let _ = execute!(io::stdout(), Print(delryn::clipboard::osc52(&text)));
            }
        }

        // A popup opening or closing forces a full repaint: terminal graphics
        // don't compose with the cell-diff, so a stale image/popup region would
        // otherwise linger until the next content change.
        let overlay_now = app.any_overlay_open();
        if overlay_now != overlay_open {
            overlay_open = overlay_now;
            terminal.clear()?;
            dirty = true;
        }

        if dirty && last_draw.elapsed() >= FRAME {
            draw(terminal, app, sync)?;
            last_draw = Instant::now();
            dirty = false;
        }
    }
    app.on_exit();
    Ok(())
}

/// Register a directory as a library folder, scan it, and report — no TUI.
fn add_library(dir: Option<&str>) -> Result<()> {
    let Some(dir) = dir else {
        eprintln!("usage: delryn --add <dir>");
        return Ok(());
    };
    let path = std::fs::canonicalize(dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| dir.to_string());

    let mut config = Config::load();
    if !config.library_paths.contains(&path) {
        config.library_paths.push(path.clone());
        config.save();
    }
    match Store::open_default() {
        Ok(store) => {
            let n = library::scan(&config.library_paths, &store);
            println!("Indexed {n} book(s). Library: {path}");
        }
        Err(e) => eprintln!("could not open library database: {e}"),
    }
    Ok(())
}

/// Render one frame, bracketed in synchronized output (DEC mode 2026) so the
/// terminal presents it atomically — no tearing or jitter while scrolling.
/// Terminals that don't support it ignore the escape sequences.
fn draw(terminal: &mut DefaultTerminal, app: &mut App, sync: bool) -> Result<()> {
    if sync {
        execute!(io::stdout(), BeginSynchronizedUpdate)?;
        terminal.draw(|f| view::render(f, app))?;
        execute!(io::stdout(), EndSynchronizedUpdate)?;
    } else {
        terminal.draw(|f| view::render(f, app))?;
    }
    Ok(())
}
