//! The folder picker: what the home-directory search found, waiting to be
//! confirmed. Each row is a proposed library source with the number of books
//! under it — the count is what the choice is actually made on, so it's given
//! its own column rather than buried in the path.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{App, Overlay};
use crate::theme::Role;

pub fn render(f: &mut Frame, app: &mut App) {
    let Overlay::FolderFinder(p) = &app.overlay else {
        return;
    };
    let theme = app.config.theme;
    let area = super::overlay_rect(f.area(), app.overlay_large);

    f.render_widget(Clear, area);

    let block = super::overlay_frame(theme, app.config.bold_borders)
        .title(Span::styled(
            " Found book folders ",
            theme.style(Role::Title),
        ))
        .style(theme.style(Role::Body).bg(theme.paper()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // what was found
        Constraint::Length(1), // what to do about it
        Constraint::Min(0),    // the proposed folders
        Constraint::Length(1), // the actions
    ])
    .split(inner);

    let total: usize = p.found.iter().map(|(fd, _)| fd.books).sum();
    f.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "{} folder{} · {total} book{}",
                p.found.len(),
                plural(p.found.len()),
                plural(total),
            ),
            theme.style(Role::Muted).add_modifier(Modifier::ITALIC),
        )),
        rows[0],
    );
    // This popup is the one place a first-run user lands without knowing the
    // shortcuts, so it says outright what the list is for — the bottom status
    // row's legend is too far from the popup to be found here.
    f.render_widget(
        Paragraph::new(Line::styled(
            "Pick the folders to add to your library:",
            theme.style(Role::Body),
        )),
        rows[1],
    );

    // The book count is right-aligned in a fixed column so the numbers line up
    // and the path — the long, variable part — gets whatever is left.
    let list = rows[2];
    let count_w = p
        .found
        .iter()
        .map(|(fd, _)| fd.books.to_string().len())
        .max()
        .unwrap_or(1)
        + 6; // " · NNN books"
    let path_w = (list.width as usize).saturating_sub(6 + count_w);

    let lines: Vec<Line> = p
        .found
        .iter()
        .enumerate()
        .map(|(i, (found, ticked))| {
            let selected = i == p.sel;
            let marker = "  ";
            let check = if *ticked { "[✓] " } else { "[ ] " };
            let path = super::truncate(&home_relative(&found.path), path_w);
            let count = format!(" · {} book{}", found.books, plural(found.books));
            let label = format!("{marker}{check}{path}{count}");
            if selected {
                crate::view::rounded_line(label, list.width, theme)
            } else {
                let style = if *ticked {
                    theme.style(Role::Body)
                } else {
                    theme.style(Role::Muted)
                };
                Line::from(Span::styled(label, style))
            }
        })
        .collect();

    // Keep the cursor in view when there are more folders than rows.
    let h = list.height as usize;
    let total_rows = lines.len();
    let offset = p
        .sel
        .saturating_sub(h / 2)
        .min(total_rows.saturating_sub(h));
    let view: Vec<Line> = lines.into_iter().skip(offset).take(h).collect();
    f.render_widget(Paragraph::new(view), list);

    // Each rendered line is one folder, so its index is the row index for clicks.
    app.mouse.overlay_rows = (offset..(offset + h).min(total_rows))
        .map(|j| {
            (
                j,
                Rect {
                    x: list.x,
                    y: list.y + (j - offset) as u16,
                    width: list.width,
                    height: 1,
                },
            )
        })
        .collect();

    f.render_widget(Paragraph::new(actions(p.picked(), theme)), rows[3]);
}

/// The action row: what each key does, with the one that commits spelled out in
/// full ("⏎ add 2 folders") and accented, so the way forward is unmissable.
fn actions(picked: usize, theme: crate::theme::Theme) -> Line<'static> {
    let dim = theme.style(Role::Muted);
    let mut spans = vec![Span::styled("Space tick  ·  a all / none  ·  ", dim)];
    if picked == 0 {
        spans.push(Span::styled("tick a folder to add it", dim));
        spans.push(Span::styled("  ·  Esc close", dim));
    } else {
        spans.push(Span::styled(
            format!("⏎ add {picked} folder{}", plural(picked)),
            theme.style(Role::Accent).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled("  ·  Esc cancel", dim));
    }
    Line::from(spans)
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Show `/Users/someone/Books` as `~/Books`: these are all home-directory paths,
/// so the prefix is the same on every row and only costs width.
fn home_relative(path: &str) -> String {
    match delryn_library::discover::home()
        .as_deref()
        .and_then(|h| h.to_str())
        .and_then(|h| path.strip_prefix(h))
    {
        Some(rest) => format!("~{rest}"),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    //! The reported bug: the picker listed folders with no visible way to
    //! accept them. The overlay's shortcuts live on the bottom status row like
    //! every other popup's, but this is the one popup a first-run user reaches
    //! without knowing any shortcuts — so it has to say so itself.

    use crate::app::{App, FolderFinder, Overlay};
    use delryn_library::discover::Found;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Draw a picker over `found` and return everything on screen as text.
    fn screen(found: Vec<(Found, bool)>) -> String {
        let _env = crate::test_env_guard();
        let dir = std::env::temp_dir().join(format!("delryn_finderview_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: serialized by `_env`; keeps `App::library()` off the real config dir.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };

        let mut app = App::library();
        app.overlay = Overlay::FolderFinder(FolderFinder { found, sel: 0 });
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::view::render(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        let _ = std::fs::remove_dir_all(&dir);
        text
    }

    fn found(path: &str, books: usize, ticked: bool) -> (Found, bool) {
        (
            Found {
                path: path.to_string(),
                books,
            },
            ticked,
        )
    }

    #[test]
    fn the_popup_says_what_the_list_is_for_and_which_key_commits() {
        let text = screen(vec![found("/books", 452, true), found("/papers", 6, true)]);

        assert!(
            text.contains("Pick the folders to add to your library"),
            "the list explains itself\n{text}"
        );
        assert!(
            text.contains("⏎ add 2 folders"),
            "the commit key is named, with what it will do\n{text}"
        );
        assert!(text.contains("Esc cancel"), "and the way out\n{text}");
        assert!(
            text.contains("452 book"),
            "each folder shows its count\n{text}"
        );
    }

    /// With nothing ticked there is nothing to add, so don't offer a key that
    /// would do nothing — say what to do instead.
    #[test]
    fn with_nothing_ticked_the_popup_asks_for_a_tick_rather_than_offering_enter() {
        let text = screen(vec![found("/books", 452, false)]);

        assert!(!text.contains("⏎ add"), "no dead commit key\n{text}");
        assert!(
            text.contains("tick a folder to add it"),
            "says what's missing\n{text}"
        );
    }
}
