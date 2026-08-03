//! delryn binary — terminal setup, the frame-paced event loop, and CLI entry.
//! All logic lives in the `delryn` library crate.

use std::io;
use std::path::Path;
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

/// `--help` / `-h` text. Mirrors the README's Usage section.
const USAGE: &str = "\
delryn — a fast, keyboard-driven terminal reader for EPUB, PDF, and MOBI/AZW3.

USAGE:
    delryn                       open the library
    delryn <FILE>                open a book straight away (EPUB / PDF / MOBI / AZW3)
    delryn <DIR>...              register folder(s) as library sources, then open

OPTIONS:
    -a, --add <DIR>...           register + index folder(s), no UI
        --rescan                 re-read metadata for every book, prune missing files
        --index                  build the full-text search index
        --export-annotations     dump all notes & bookmarks as Markdown to stdout
        --clear-cache            delete the cached page/figure/equation images
        --terminal               report what this terminal says about its graphics
    -h, --help                   show this help and exit
    -V, --version                show the version and exit

ENVIRONMENT:
    DELRYN_SYNC=0                disable synchronized output (DEC 2026)
    DELRYN_PDFIUM_DIR=<DIR>      where to look for the bundled libpdfium

Images, PDF pages, and graphical math need a terminal that speaks the Kitty
graphics protocol. Full documentation: https://github.com/dilmun/delryn";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let first = args.first().map(String::as_str);

    if matches!(first, Some("--help" | "-h")) {
        println!("{USAGE}");
        return Ok(());
    }
    if matches!(first, Some("--version" | "-V")) {
        println!("delryn {}", delryn::VERSION);
        return Ok(());
    }
    // An unrecognized option used to fall through to "open this as a book", so a
    // typo (`--rescn`) was reported as an unsupported *file type* rather than an
    // unknown flag. A bare "-" is left alone: it's a legal, if odd, filename.
    if let Some(flag) = first
        && flag.starts_with('-')
        && flag != "-"
        && !matches!(
            flag,
            "--add"
                | "-a"
                | "--rescan"
                | "--index"
                | "--export-annotations"
                | "--clear-cache"
                | "--terminal"
        )
    {
        eprintln!("delryn: unknown option '{flag}'\nTry 'delryn --help' for usage.");
        std::process::exit(2);
    }

    // `delryn --add <dir> [dir…]`: register library folder(s), scan, and exit.
    if matches!(first, Some("--add" | "-a")) {
        return add_library(&args[1..]);
    }

    // `delryn --clear-cache`: drop every cached raster, then exit. The caches
    // rebuild on demand, so this only ever costs re-rendering.
    if matches!(first, Some("--clear-cache")) {
        return clear_caches();
    }

    // `delryn --terminal`: what this terminal says about itself and its graphics.
    // Images in a terminal emulator fail in ways a screenshot can't explain — a
    // protocol claimed but not implemented, a cell size reported in the wrong
    // units — so this prints the answers rather than asking anyone to guess, and
    // names the override to try. Runs the queries outside the alternate screen.
    if matches!(first, Some("--terminal")) {
        print!("{}", delryn::media::terminal_report());
        return Ok(());
    }

    // `delryn --rescan`: re-read metadata for every known book (backfills new
    // fields like series/publisher for an already-indexed library), then exit.
    if matches!(first, Some("--rescan")) {
        let config = load_config_reporting();
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
    if matches!(first, Some("--index")) {
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
    if matches!(first, Some("--export-annotations")) {
        if let Ok(store) = Store::open_default() {
            let mut last_path = String::new();
            let mut last_folder: Option<String> = None;
            for (path, a) in store.all_bookmarks() {
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
                println!("- §{} {label}", a.section + 1);
            }
        }
        return Ok(());
    }

    // Report an unreadable config now, on the primary screen, before the alt
    // screen hides it. `App::library` / `App::open_book` load it again, but by
    // then the damaged file has been moved aside, so they see a clean default and
    // the warning is said exactly once.
    let _ = load_config_reporting();
    // One version string for the whole app: `--version`, the crash log, and the
    // User-Agent the online providers send.
    delryn::online::set_version(delryn::VERSION);

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
        // A folder argument (one or more) registers library sources and opens the
        // library, scanning any newly added folders. A file argument opens that
        // book, as before.
        Some(path) if Path::new(path).is_dir() => {
            register_library_dirs(&args, true);
            App::library()
        }
        Some(path) => App::open_book(path, picker.is_some())?,
        None => App::library(),
    };
    // First run — an empty library — lands on the Sources manager so a new user's
    // first action is adding a folder. No-op once a folder is configured.
    app.open_sources_if_empty();
    // Index the configured folders in the background (incremental + dead-entry
    // prune) so the library appears instantly and refreshes as the scan lands.
    app.start_scan_startup();
    // Persist measured equation-image ink profiles, so reopening a book reads them from
    // disk instead of re-decoding every equation on the main thread (the open freeze).
    delryn::app::reader_ink_cache_set_dir(delryn::media::raster_cache_dir());
    // Spawn the background image builder from the detected protocol.
    app.image_builder = picker
        .clone()
        .map(|p| delryn::media::ImageBuilder::new(p, delryn::media::raster_cache_dir()));
    app.picker = picker;
    // Keep the on-disk caches bounded. Off the main thread and after the builder
    // has claimed its directory: the sweep walks and deletes thousands of files,
    // which has no business delaying the first frame.
    sweep_caches_in_background(app.config.cache_limit_mb);

    // Synchronized output (DEC 2026) can be toggled off for terminals that
    // mishandle it: `DELRYN_SYNC=0 delryn …`.
    let sync = std::env::var("DELRYN_SYNC")
        .map(|v| v != "0")
        .unwrap_or(true);

    // Ask to be told about SIGTERM/SIGHUP rather than dying on them: the default
    // disposition kills the process outright, which left the terminal in the
    // alternate screen with mouse reporting and raw mode still on (the user had to
    // run `reset`), and skipped `on_exit` — losing the reading position and the
    // session's reading time. Now they just end the loop, so the ordinary teardown
    // below runs. Ctrl-C needs no handling: raw mode delivers it as a key event.
    install_termination_handlers();

    let mut terminal = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    // ratatui's panic hook restores raw mode + the alt screen, but not the app-specific
    // mouse capture or resident images — so a crash would leave the terminal streaming mouse
    // escapes into the shell. Chain a hook that takes those down first, then defers to it.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Persist the panic (message, location, backtrace) to a durable, append-only
        // crash log *before* anything restores the terminal — the TUI's stderr redirect
        // (`StderrRedirect`) is truncated on launch and shared between instances, so a
        // crash trace would otherwise be lost the next time the reader opens. This file
        // survives relaunches and concurrent instances, so a reproduced crash is always
        // recoverable for diagnosis.
        write_crash_log(info);
        let _ = execute!(
            io::stdout(),
            Print(delryn::media::delete_all_images_seq()),
            DisableMouseCapture
        );
        prev_hook(info);
    }));
    // Mute stray dependency stderr for the TUI's lifetime (see `StderrRedirect`).
    let stderr_guard = StderrRedirect::for_tui();
    let result = run(&mut terminal, &mut app, sync);
    // Free every terminal-resident image (pages + inline) so none linger after exit.
    let _ = execute!(io::stdout(), Print(delryn::media::delete_all_images_seq()));
    // Restore stderr before anything else prints, so real errors reach the terminal.
    drop(stderr_guard);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

/// The cache directories this build actually reads. Anything else under
/// `<config>/rasters` is from a superseded format and is swept away.
fn live_cache_dirs() -> Vec<String> {
    vec![
        delryn::media::raster_cache_version_dir(),
        delryn::app::ink_cache_version_dir(),
    ]
}

/// Bound the on-disk caches on a worker thread.
///
/// Both caches are version-stamped, which stopped *stale* entries being read but
/// never deleted them, and never capped the live ones either — one real library
/// held 1.7 GB, 1.03 GB of it in five superseded versions. This drops those and
/// evicts least-recently-used entries down to the configured ceiling.
fn sweep_caches_in_background(limit_mb: u32) {
    let Some(root) = delryn::media::raster_cache_dir() else {
        return;
    };
    let live = live_cache_dirs();
    let budget = u64::from(limit_mb) * 1024 * 1024;
    std::thread::spawn(move || {
        delryn::media::sweep_caches(&root, &live, budget);
    });
}

/// Delete every on-disk cache, live versions included, and report what went.
/// Caches are rebuildable by construction, so this only ever costs re-rendering.
fn clear_caches() -> Result<()> {
    let Some(root) = delryn::media::raster_cache_dir() else {
        println!("No cache directory configured.");
        return Ok(());
    };
    if !root.exists() {
        println!("Nothing cached.");
        return Ok(());
    }
    // Sweeping with no live directories and a 1-byte budget empties everything.
    let freed = delryn::media::sweep_caches(&root, &[], 1);
    let _ = std::fs::remove_dir_all(&root);
    println!(
        "Cleared {:.1} MB from {}.",
        freed as f64 / (1024.0 * 1024.0),
        root.display()
    );
    Ok(())
}

/// Set when SIGTERM or SIGHUP arrives, polled by the event loop.
static TERMINATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Signal handler. The only thing it does is set an atomic flag — the one useful
/// operation that is genuinely async-signal-safe. All the real work (restoring the
/// terminal, saving progress) happens back on the main thread once the loop sees it.
#[cfg(unix)]
extern "C" fn on_terminate(_sig: libc::c_int) {
    TERMINATE.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Route SIGTERM/SIGHUP to [`on_terminate`] instead of the default "die now".
#[cfg(unix)]
fn install_termination_handlers() {
    // SAFETY: `signal` installs our handler for two specific signals. The handler
    // does nothing but store to a `static` atomic, so it is async-signal-safe and
    // has no lifetime or aliasing requirements.
    // Cast via a concrete fn *pointer*: a bare fn item is zero-sized, and casting
    // it straight to an integer is the mistake `clippy::fn_to_numeric_cast_any`
    // warns about.
    let handler = on_terminate as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

#[cfg(not(unix))]
fn install_termination_handlers() {}

/// Has a termination signal arrived? Checked once per event-loop turn.
fn terminating() -> bool {
    TERMINATE.load(std::sync::atomic::Ordering::SeqCst)
}

/// Load the config, printing any "couldn't read your settings" warning to stderr.
///
/// Loading is deliberately infallible — a reader whose config is damaged should
/// still start — but it must not be *silent*: the fallback drops every setting and
/// the whole library source list, and the user's only clue would be an empty
/// library. [`Config::load_checked`] moves the damaged file aside first, so the
/// warning can point at a file that still exists.
fn load_config_reporting() -> Config {
    let (config, warning) = Config::load_checked();
    if let Some(w) = warning {
        eprintln!("delryn: {w}");
    }
    config
}

/// The durable crash-log path: `<runtime dir>/delryn-crash.log`. Append-only so a
/// reproduced crash is never clobbered by a relaunch or a second running instance
/// (unlike the truncated stderr redirect).
fn crash_log_path() -> std::path::PathBuf {
    delryn::paths::runtime_dir().join("delryn-crash.log")
}

/// Append one panic (message, source location, and a full backtrace) to the crash log.
/// Called from the panic hook while the terminal is still in raw mode, so it must not
/// print to stderr/stdout; it only writes the file. Best-effort — any I/O error is
/// swallowed, since we're already unwinding.
fn write_crash_log(info: &std::panic::PanicHookInfo<'_>) {
    use std::io::Write;
    let msg = info
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>");
    let loc = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown location>".to_string());
    // `force_capture` reads RUST_BACKTRACE-independent frames, so a user needn't set the
    // env var to get a usable trace.
    let bt = std::backtrace::Backtrace::force_capture();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crash_log_path())
    {
        let _ = writeln!(
            f,
            "\n===== delryn panic =====\nversion: {}\nat: {loc}\nmessage: {msg}\nbacktrace:\n{bt}\n========================",
            delryn::VERSION,
        );
    }
}

/// While alive, points the process's stderr (fd 2) at a log file, so a dependency's stray
/// `eprintln!` — e.g. the equation engine's RaTeX font-loader diagnostics
/// (`[ratex-unicode-font] found via builtin path: …`) — can't scribble into the
/// alternate-screen TUI and desync ratatui's cell-diff renderer (which surfaces as torn text
/// and garbled prose mid-scroll). Restored on drop, so any error or panic printed after we
/// leave the TUI still reaches the real terminal.
#[cfg(unix)]
struct StderrRedirect {
    saved: Option<i32>,
}

#[cfg(unix)]
impl StderrRedirect {
    fn for_tui() -> Self {
        use std::os::fd::AsRawFd;
        let log = delryn::paths::runtime_dir().join("delryn.log");
        let Ok(file) = std::fs::File::create(log) else {
            return Self { saved: None };
        };
        // SAFETY: `dup`/`dup2` act on the process's own stderr. `dup` saves the original
        // stderr so `drop` can restore it; `dup2` repoints fd 2 at the log file's open
        // description. `file`'s own fd is closed at end of scope, but fd 2 is an independent
        // descriptor for the same description, so the log stays writable via fd 2 until the
        // guard restores the original.
        unsafe {
            let saved = libc::dup(libc::STDERR_FILENO);
            if saved < 0 {
                return Self { saved: None };
            }
            libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO);
            Self { saved: Some(saved) }
        }
    }
}

#[cfg(unix)]
impl Drop for StderrRedirect {
    fn drop(&mut self) {
        if let Some(saved) = self.saved {
            // SAFETY: `saved` is a live dup of the original stderr; restore it onto fd 2 and
            // close the temporary dup.
            unsafe {
                libc::dup2(saved, libc::STDERR_FILENO);
                libc::close(saved);
            }
        }
    }
}

#[cfg(not(unix))]
struct StderrRedirect;

#[cfg(not(unix))]
impl StderrRedirect {
    fn for_tui() -> Self {
        Self
    }
}

fn run(terminal: &mut DefaultTerminal, app: &mut App, sync: bool) -> Result<()> {
    draw(terminal, app, sync, false)?;
    let mut last_draw = Instant::now();
    let mut dirty = false;
    // A pending full redraw (layout change / overlay toggle). Held across
    // iterations so a request made mid-throttle still reaches the next frame.
    let mut full_redraw = false;
    // Tracks overlay open/closed so a toggle can force a full repaint — a closed
    // popup over an inline image otherwise leaves a ghost the cell-diff skips.
    let mut modal_shown = app.modal_open();
    // Native system clipboard (reliable across terminals); OSC 52 is the
    // fallback when it's unavailable (e.g. SSH/headless).
    let mut clipboard = arboard::Clipboard::new().ok();

    while !app.should_quit && !terminating() {
        // Block for input — only until the next frame is due if a redraw is
        // pending or a scroll is animating, otherwise block long so an idle
        // reader costs ~0% CPU.
        let busy = app.animating()
            || app.online_active()
            || app.define_active()
            || app.lib_grid_pending()
            || app.cover_pending()
            || app.preview_pending()
            || app.dup_scan_pending()
            || app.scan_pending()
            || app.discover_pending()
            || app.figure_scan_pending();
        let timeout = if dirty || busy {
            FRAME.saturating_sub(last_draw.elapsed())
        } else {
            IDLE
        };

        // A signal interrupts the blocking poll, surfacing as `Interrupted`. That
        // is the handler doing its job, not a failure — go round and let the loop
        // condition see the flag.
        let ready = match event::poll(timeout) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        if ready {
            // Drain the whole burst before drawing.
            loop {
                match event::read()? {
                    Event::Key(key) => {
                        app.on_key(key);
                        dirty = true;
                    }
                    Event::Mouse(m) => {
                        // Only repaint when the event changed something — an
                        // any-motion mouse-move flood must not spin the render loop.
                        if app.on_mouse(m) {
                            dirty = true;
                        }
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
        // Pick up a finished word lookup (dictionary + Wikipedia).
        if app.poll_define() {
            dirty = true;
        }
        // Advance the thorough duplicate scan (cover hashing on a worker thread).
        if app.poll_dup_scan() {
            dirty = true;
        }
        // Pick up a finished background library scan (folder (re)indexing).
        if app.poll_scan() {
            dirty = true;
        }
        // Pick up a finished search for book folders (opens the folder picker).
        if app.poll_discover() {
            dirty = true;
        }
        // Wrap + cache any covers the background loader finished this frame, and keep
        // redrawing while it's still loading visible/prefetched covers — so they pop in
        // without blocking navigation on I/O + decode.
        if app.poll_grid_covers() {
            dirty = true;
        }
        if app.lib_grid_pending() {
            dirty = true;
        }
        // Rebuild the detail-pane cover once the selection settles (debounced).
        if app.tick_cover() {
            dirty = true;
        }
        // Merge whole-book figures into the open image viewer as the background scan
        // finishes each section, so book scope fills in instead of freezing on open.
        if app.poll_figure_scan() {
            dirty = true;
        }
        // Fetch the editor's Cover-tab preview when the highlighted result settles.
        app.tick_preview();

        // Bank the reading position periodically, so an abrupt end (a signal, a
        // closed terminal, a power cut) costs seconds rather than the whole
        // chapter. Cheap: an `Instant` comparison until the interval elapses.
        app.tick_autosave();

        // Ease pending scroll a few lines toward its target this frame.
        if app.step_scroll() {
            dirty = true;
        }
        // Keep drawing while a scroll eases or inline images are still building,
        // so finished images pop in without needing a keypress.
        if app.animating() {
            dirty = true;
        }
        // Drain finished PDF page rasterizations and keep drawing until the
        // look-ahead pages are resident in the terminal, so page turns are
        // instant (the page is already there, just placed).
        if app.poll_pages() {
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
        // Copy a requested image (from the image viewer) to the system clipboard.
        if let Some((w, h, rgba)) = app.take_clipboard_image()
            && let Some(c) = clipboard.as_mut()
        {
            let _ = c.set_image(arboard::ImageData {
                width: w as usize,
                height: h as usize,
                bytes: std::borrow::Cow::Owned(rgba),
            });
        }

        // A popup opening or closing forces a full repaint: terminal graphics
        // don't compose with the cell-diff, so a stale image/popup region would
        // otherwise linger until the next content change.
        let modal_now = app.modal_open();
        if modal_now != modal_shown {
            modal_shown = modal_now;
            full_redraw = true;
            dirty = true;
        }
        // A reflow in place — a code fold/unfold, or a chrome/width toggle that
        // resizes the text area — moves every inline image; force the same full
        // repaint so old placements don't linger until the next scroll.
        if app.take_repaint() {
            full_redraw = true;
            dirty = true;
        }

        // The request rides along to the next drawn frame rather than clearing the
        // screen here: the throttle can hold a bare, terminal-coloured screen for a
        // whole frame interval, which mashing a chrome toggle turns into a strobe.
        if dirty && last_draw.elapsed() >= FRAME {
            draw(terminal, app, sync, full_redraw)?;
            full_redraw = false;
            last_draw = Instant::now();
            dirty = false;
        }
    }
    app.on_exit();
    Ok(())
}

/// Register a directory as a library folder, scan it, and report — no TUI.
/// `delryn --add <dir> [dir…]`: register the given folder(s) as library sources,
/// scan, and exit. Unlike the positional form, offline folders are still
/// registered (the flag is an explicit intent, not a file-vs-folder guess).
fn add_library(dirs: &[String]) -> Result<()> {
    if dirs.is_empty() {
        eprintln!("usage: delryn --add <dir> [dir…]");
        return Ok(());
    }
    // Report (and back up) an unreadable config before anything writes over it.
    let _ = load_config_reporting();
    register_library_dirs(dirs, false);
    let config = Config::load();
    match Store::open_default() {
        Ok(store) => {
            let n = library::scan(&config.library_paths, &store);
            println!("Indexed {n} book(s).");
        }
        Err(e) => eprintln!("could not open library database: {e}"),
    }
    Ok(())
}

/// Add each folder in `args` to the library source list (normalized + deduped),
/// saving the config only when something changed. With `require_dir`, non-folder
/// arguments are skipped — used by the positional launch form, where a file
/// argument means "open this book", not "add this source". The scan is left to
/// the caller ([`App::library`] or [`add_library`]).
fn register_library_dirs(args: &[String], require_dir: bool) {
    let mut config = Config::load();
    let mut changed = false;
    for arg in args {
        if require_dir && !Path::new(arg).is_dir() {
            continue;
        }
        let root = library::normalize_root(arg);
        if !config.library_paths.contains(&root) {
            config.library_paths.push(root);
            changed = true;
        }
    }
    if changed {
        config.save();
    }
}

/// Render one frame, bracketed in synchronized output (DEC mode 2026) so the
/// terminal presents it atomically — no tearing or jitter while scrolling.
/// Terminals that don't support it ignore the escape sequences.
fn draw(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    sync: bool,
    full_redraw: bool,
) -> Result<()> {
    if sync {
        execute!(io::stdout(), BeginSynchronizedUpdate)?;
    }
    // A layout change needs every cell rewritten and every image re-placed: the
    // clear resets ratatui's cell-diff, and terminal graphics live outside that
    // grid, so the decks must forget what they believe is on screen or their
    // "nothing changed" fast path would leave the images gone.
    //
    // It has to happen *inside* the synchronized frame. A clear paints the
    // terminal's own background, so presenting it on its own — as a separate
    // frame, or for however long the frame throttle holds it — flashes bright on
    // any theme darker than the terminal.
    if full_redraw {
        terminal.clear()?;
        app.restage_images();
    }
    let t_draw = Instant::now();
    terminal.draw(|f| view::render(f, app))?;
    let draw_us = t_draw.elapsed().as_micros();
    // Full PDF pages are managed directly via the kitty protocol (temp-file
    // transmit + placement), not through ratatui's cell buffer. Building the
    // escapes writes the page temp files; emit them inside the synchronized frame
    // so a page appears atomically with the chrome.
    let t_build = Instant::now();
    let mut escapes = app.page_escapes();
    // Inline images (equation rasters + inline figures) place in the same synchronized
    // frame as the PDF pages and chrome, so a maths page appears atomically.
    escapes.extend(app.inline_escapes());
    let build_us = t_build.elapsed().as_micros();
    let esc_bytes: usize = escapes.iter().map(String::len).sum();
    let t_write = Instant::now();
    for esc in escapes {
        execute!(io::stdout(), Print(esc))?;
    }
    if sync {
        execute!(io::stdout(), EndSynchronizedUpdate)?;
    }
    // Profiling for the PDF page path: draw time, escape-build time (includes the
    // temp-file writes), stdout-write time, and bytes pushed to the terminal per
    // frame. Gated on DELRYN_KITTY_LOG; zero cost otherwise.
    if esc_bytes > 0 && std::env::var_os("DELRYN_KITTY_LOG").is_some() {
        log_page_timing(draw_us, build_us, t_write.elapsed().as_micros(), esc_bytes);
    }
    Ok(())
}

/// Append one PDF-frame timing line to `<runtime dir>/delryn-kitty.log` (see `draw`).
fn log_page_timing(draw_us: u128, build_us: u128, write_us: u128, esc_bytes: usize) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(delryn::paths::runtime_dir().join("delryn-kitty.log"))
    {
        let _ = writeln!(
            f,
            "timing draw={draw_us}us build={build_us}us write={write_us}us bytes={esc_bytes}"
        );
    }
}
