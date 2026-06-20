//! delryn binary — terminal setup, the event loop, and CLI entry.
//! All logic lives in the `delryn` library crate.

use std::io;

use anyhow::Result;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
use ratatui::DefaultTerminal;

use delryn::app::App;
use delryn::view;

fn main() -> Result<()> {
    let mut app = match std::env::args().nth(1) {
        Some(path) => App::open_book(&path)?,
        None => App::library(),
    };

    let mut terminal = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, &mut app);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|f| view::render(f, app))?;
        match event::read()? {
            Event::Key(key) => app.on_key(key),
            Event::Mouse(m) => app.on_mouse(m),
            _ => {}
        }
    }
    Ok(())
}
