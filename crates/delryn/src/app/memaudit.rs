//! Headless lifecycle memory audit: open a book, read it, close it, repeat.
//!
//! The point is to make retention measurable without a terminal and without a human
//! driving the TUI. Each phase reports the process's resident size, so a leak shows
//! up as a floor that rises across cycles rather than as a single suspicious number.
//!
//! Run it deliberately, naming the books to read (`:`-separated). It opens real books and
//! is far too slow for the normal suite, so it does nothing unless asked:
//!
//! ```text
//! DELRYN_MEMAUDIT="$HOME/books/maths.epub:$HOME/books/paper.pdf" \
//!   cargo test -p delryn --lib memaudit -- --nocapture --test-threads=1
//! ```
//!
//! Resident size is the honest measure here but a noisy one: freeing memory returns
//! it to the allocator, which need not return it to the OS. So the assertions are
//! about *growth across repeated cycles* — which no amount of allocator retention
//! explains — rather than about any single absolute figure.

#![cfg(test)]

use std::process::Command;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::{App, Mode};

/// Live thread count for this process. Each book spawns workers (section loader,
/// image builder, page rasterizer); if closing leaves them running, this climbs
/// per cycle even when resident size looks flat.
pub(crate) fn threads() -> usize {
    let pid = std::process::id();
    Command::new("ps")
        .args(["-M", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().count().max(1) - 1)
        .unwrap_or(0)
}

/// This process's resident size in MB, via `ps` — no extra dependency, and it is the
/// same number the user reads off their process monitor.
pub(crate) fn rss_mb() -> f64 {
    let pid = std::process::id();
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    let kb: f64 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0.0);
    kb / 1024.0
}

/// A picker built without querying a terminal, so the audit can drive the real image
/// pipeline headlessly. The cell size is a typical one; nothing here depends on it
/// being the reader's actual terminal.
fn headless_picker() -> ratatui_image::picker::Picker {
    // `from_fontsize` is deprecated upstream, but it is the only constructor that
    // doesn't need a tty — which is the whole point here.
    #[expect(deprecated)]
    let mut p = ratatui_image::picker::Picker::from_fontsize(ratatui_image::FontSize::new(10, 20));
    p.set_protocol_type(ratatui_image::picker::ProtocolType::Kitty);
    p
}

/// Drive one open → read → close cycle, returning the resident size after each phase.
///
/// `sections` bounds how far it reads; a maths chapter builds hundreds of equation
/// rasters per section, which is exactly the load that made retention visible.
fn cycle(path: &str, sections: usize) -> (f64, f64) {
    // `true`: PDFs refuse to open without a graphics-capable terminal, and the page
    // deck is exactly the allocation path this audit needs to weigh.
    let mut app = App::open_book(path, true).expect("open book");
    // Mirror `main`: without a builder no figure or equation is ever rasterised, and
    // those are the allocations this audit exists to weigh.
    let picker = headless_picker();
    app.image_builder = Some(crate::media::ImageBuilder::new(
        picker.clone(),
        crate::media::raster_cache_dir(),
    ));
    let geom = crate::app::reader::ImageGeom {
        avail: 80,
        max_rows: 40,
        max_px: 0,
        width_pct: 85,
        math_scale: 100,
        fit_mode: crate::media::ImageFit::Fit,
        policy: crate::media::RenderPolicy {
            tint: crate::media::Ink {
                ink: [0, 0, 0],
                paper: [255, 255, 255],
            },
            mode: crate::media::ImageMode::default(),
        },
    };

    // Stand in for the render loop: wrap at a realistic measure, pump the image
    // pipeline, and walk forward — what materialises lines, rasters and decodes.
    for _ in 0..sections {
        for _ in 0..12 {
            // Disjoint fields, so both borrows coexist.
            if let (Some(r), Some(b)) = (app.reader.as_mut(), app.image_builder.as_ref()) {
                r.ensure_wrapped(80);
                r.sync_images(b, &picker, geom);
            }
            app.poll_pages();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if let Some(r) = app.reader.as_mut() {
            r.scroll_down(40);
        }
    }
    let reading = rss_mb();

    // The close path the reader actually takes — through the real key handler, so the
    // audit can't drift from what pressing `q` does.
    app.on_key(KeyEvent::new_with_kind(
        KeyCode::Char('q'),
        KeyModifiers::NONE,
        KeyEventKind::Press,
    ));
    assert_eq!(app.mode, Mode::Library, "q returns to the library");
    drop(app);
    (reading, rss_mb())
}

/// The books to exercise, from `DELRYN_MEMAUDIT` — one or more paths separated by `:`.
///
/// Supplied rather than hardcoded for two reasons: a path into someone's library says what
/// they read, which has no business in a public repository; and a list naming one person's
/// files makes the audit silently vacuous everywhere else, which is worse than not having
/// it. A missing path is reported, never skipped quietly.
///
/// Pick books that stress the allocation paths — a maths chapter builds hundreds of
/// equation rasters per section, a PDF builds full-page ones:
///
/// ```text
/// DELRYN_MEMAUDIT="$HOME/books/maths.epub:$HOME/books/paper.pdf" \
///   cargo test -p delryn --lib memaudit -- --nocapture --test-threads=1
/// ```
fn books() -> Vec<String> {
    let Some(spec) = std::env::var_os("DELRYN_MEMAUDIT") else {
        return Vec::new();
    };
    spec.to_string_lossy()
        .split(':')
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "1")
        .filter(|p| {
            let ok = std::path::Path::new(p).exists();
            if !ok {
                eprintln!("memaudit: no such book, skipping: {p}");
            }
            ok
        })
        .map(str::to_string)
        .collect()
}

fn enabled() -> bool {
    !books().is_empty()
}

/// Repeated open → read → close of the **same** book must not grow the floor.
///
/// This is the assertion that actually catches a lifetime bug: allocator retention
/// and one-time global caches both settle after the first cycle, so anything still
/// climbing on cycle five is genuinely owned by something that outlives the book.
#[test]
fn reopening_one_book_does_not_grow_memory() {
    if !enabled() {
        return;
    }
    let _guard = crate::test_env_guard();
    let Some(book) = books().into_iter().next() else {
        return;
    };

    let base = rss_mb();
    eprintln!(
        "baseline               {base:8.1} MB  {} threads",
        threads()
    );
    let mut closed = Vec::new();
    let mut thread_counts = Vec::new();
    for i in 1..=5 {
        let (reading, after) = cycle(&book, 4);
        let t = threads();
        eprintln!("cycle {i}: reading {reading:8.1} MB   closed {after:8.1} MB  {t} threads");
        closed.push(after);
        thread_counts.push(t);
    }
    // Workers are book-scoped: closing must not leave a fleet behind.
    let thread_growth = thread_counts.last().unwrap() - thread_counts[0];
    assert!(
        thread_growth <= 2,
        "thread count grew by {thread_growth} across cycles ({thread_counts:?}) \
         — a per-book worker is outliving its book"
    );

    // Compare late cycles against the first *closed* figure, so one-time costs (fonts,
    // syntax sets, the store) are already paid and out of the comparison.
    let first = closed[0];
    let last = *closed.last().unwrap();
    let growth = last - first;
    eprintln!("floor drift over 4 further cycles: {growth:+.1} MB");
    assert!(
        growth < 40.0,
        "memory floor grew {growth:.1} MB across repeated open/close cycles \
         (first close {first:.1} MB, last {last:.1} MB) — something outlives the book"
    );
}

/// Closing must return most of what reading cost. Deliberately a loose bound: the
/// exact figure moves with the book and the allocator, and a tight one would fail for
/// reasons that aren't bugs.
#[test]
fn closing_a_book_releases_most_of_its_memory() {
    if !enabled() {
        return;
    }
    let _guard = crate::test_env_guard();
    let Some(book) = books().into_iter().next() else {
        return;
    };

    // Warm the one-time globals so they aren't counted as the book's cost.
    let _ = cycle(&book, 2);

    let before = rss_mb();
    let (reading, closed) = cycle(&book, 6);
    let grew = reading - before;
    let kept = closed - before;
    eprintln!("before {before:.1} MB  reading {reading:.1} MB  closed {closed:.1} MB");
    eprintln!("book cost {grew:+.1} MB, retained after close {kept:+.1} MB");
    assert!(
        kept < grew * 0.5 + 20.0,
        "closing kept {kept:.1} MB of the {grew:.1} MB the book cost"
    );
}

/// Not an assertion — a report. Runs a cycle, closes the book, then asks the
/// allocator what is still live and who allocated it.
///
/// Run with stack logging so the owners are named rather than merely sized:
///
/// ```text
/// DELRYN_MEMAUDIT="$HOME/books/maths.epub" MallocStackLogging=1 \
///   cargo test -p delryn --lib memaudit::report -- --nocapture --test-threads=1
/// ```
#[test]
fn report_what_survives_closing() {
    if !enabled() {
        return;
    }
    let _guard = crate::test_env_guard();
    let Some(book) = books().into_iter().next() else {
        return;
    };
    let (reading, closed) = cycle(&book, 4);
    eprintln!("reading {reading:.1} MB → closed {closed:.1} MB");

    let pid = std::process::id().to_string();

    // Where the resident bytes actually are. `heap` only sees malloc'd nodes; a
    // reader also holds thread stacks, mapped files and the allocator's own free
    // pools, and conflating those is how "leak" gets misdiagnosed.
    let regions = Command::new("vmmap")
        .args(["-summary", &pid])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    eprintln!("--- resident by region type ---");
    for line in regions.lines().filter(|l| {
        l.contains("MALLOC_LARGE")
            || l.contains("MALLOC_SMALL")
            || l.contains("MALLOC_TINY")
            || l.contains("Stack")
            || l.contains("mapped file")
            || l.contains("__DATA")
            || l.starts_with("TOTAL")
            || l.contains("Writable regions")
    }) {
        eprintln!("{}", &line[..line.len().min(150)]);
    }

    let sizes = Command::new("heap")
        .args([&pid])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    for line in sizes.lines().filter(|l| l.contains("nodes malloced")) {
        eprintln!("\n{}\n", &line[..line.len().min(300)]);
    }

    if std::env::var_os("MallocStackLogging").is_some() {
        let hist = Command::new("malloc_history")
            .args([&pid, "-allBySize"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        eprintln!("--- largest live allocations, by owner ---");
        for line in hist.lines().filter(|l| l.contains("calls for")).take(12) {
            // Keep the head (size) and the delryn frames; the rest is std plumbing.
            let head: String = line.chars().take(60).collect();
            let mut frames: Vec<&str> = line
                .split(" | ")
                .filter(|f| f.contains("delryn") || f.contains("ratex"))
                .collect();
            // Innermost frames last in the report — the allocating call is the
            // interesting one, and the outer ones are thread plumbing.
            frames.reverse();
            frames.truncate(3);
            eprintln!("{head}\n      {}", frames.join("\n      "));
        }
    }
}

/// Several different books in succession must not stack.
#[test]
fn different_books_do_not_stack() {
    if !enabled() {
        return;
    }
    let _guard = crate::test_env_guard();
    let paths = books();
    if paths.len() < 2 {
        eprintln!("memaudit: need two books, skipping");
        return;
    }
    let mut floors = Vec::new();
    for round in 1..=2 {
        for p in &paths {
            let (reading, closed) = cycle(p, 3);
            let name = p.rsplit('/').next().unwrap_or(p);
            eprintln!("round {round}: {reading:8.1} / {closed:8.1} MB  {name}");
            floors.push(closed);
        }
    }
    let drift = floors.last().unwrap() - floors[paths.len() - 1];
    eprintln!("floor drift across a second round of every book: {drift:+.1} MB");
    assert!(
        drift < 40.0,
        "opening the same set of books again grew the floor by {drift:.1} MB"
    );
}
