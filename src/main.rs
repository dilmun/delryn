//! delryn binary — terminal setup, the frame-paced event loop, and CLI entry.
//! All logic lives in the `delryn` library crate.

use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
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

    // `delryn --index`: build the full-text search index, then exit.
    if matches!(args.first().map(String::as_str), Some("--index")) {
        match Store::open_default() {
            Ok(store) => println!("Full-text indexed {} book(s).", library::index_fulltext(&store)),
            Err(e) => eprintln!("could not open library database: {e}"),
        }
        return Ok(());
    }

    // `delryn --export-annotations`: dump bookmarks/notes as Markdown, then exit.
    if matches!(args.first().map(String::as_str), Some("--export-annotations")) {
        if let Ok(store) = Store::open_default() {
            let mut last = String::new();
            for (path, a) in store.all_annotations() {
                if path != last {
                    println!("\n# {path}\n");
                    last = path;
                }
                if a.note.is_empty() {
                    println!("- §{} {}", a.section + 1, a.quote);
                } else {
                    println!("- §{} {} — {}", a.section + 1, a.quote, a.note);
                }
            }
        }
        return Ok(());
    }

    // Detect the terminal's image protocol before entering the alt screen.
    let picker = delryn::media::detect_picker();

    let mut app = match args.first() {
        Some(path) => App::open_book(path)?,
        None => App::library(),
    };
    app.picker = picker;

    // Synchronized output (DEC 2026) can be toggled off for terminals that
    // mishandle it: `DELRYN_SYNC=0 delryn …`.
    let sync = std::env::var("DELRYN_SYNC").map(|v| v != "0").unwrap_or(true);

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

    while !app.should_quit {
        // Block for input — only until the next frame is due if a redraw is
        // pending or a scroll is animating, otherwise block long so an idle
        // reader costs ~0% CPU.
        let timeout = if dirty || app.animating() {
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

        // Ease pending scroll a few lines toward its target this frame.
        if app.step_scroll() {
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
