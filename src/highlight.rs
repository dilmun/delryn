//! Syntax highlighting for code blocks via syntect. Loaded once and reused.
//! Colour selection ties into the theme system later; for now a fixed dark
//! syntax theme. See `DESIGN.md` §1 (programming-book wedge).

use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::document::Inline;
use crate::layout::Run;

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEMES: OnceLock<ThemeSet> = OnceLock::new();

fn syntaxes() -> &'static SyntaxSet {
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme(name: &str) -> &'static Theme {
    let ts = THEMES.get_or_init(ThemeSet::load_defaults);
    ts.themes
        .get(name)
        .or_else(|| ts.themes.get("base16-ocean.dark"))
        .or_else(|| ts.themes.values().next())
        .expect("at least one default theme")
}

/// Highlight code into per-line styled runs (one inner `Vec<Run>` per input
/// line) using the named syntect theme. Language is taken from the fenced
/// `lang`, else guessed from the first line, else treated as plain text.
pub fn highlight_code(lines: &[String], lang: Option<&str>, theme_name: &str) -> Vec<Vec<Run>> {
    let ps = syntaxes();
    let syntax = lang
        .and_then(|l| ps.find_syntax_by_token(l))
        .or_else(|| lines.first().and_then(|l| ps.find_syntax_by_first_line(l)))
        .unwrap_or_else(|| ps.find_syntax_plain_text());

    let mut hl = HighlightLines::new(syntax, theme(theme_name));
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let mut runs = Vec::new();
        match hl.highlight_line(line, ps) {
            Ok(ranges) => {
                for (style, text) in ranges {
                    let text = text.trim_end_matches(['\n', '\r']);
                    if text.is_empty() {
                        continue;
                    }
                    let c = style.foreground;
                    runs.push(Run {
                        text: text.to_string(),
                        style: Inline {
                            bold: style.font_style.contains(FontStyle::BOLD),
                            italic: style.font_style.contains(FontStyle::ITALIC),
                            code: true,
                            link: false,
                        },
                        fg: Some((c.r, c.g, c.b)),
                    });
                }
            }
            Err(_) => runs.push(Run {
                text: line.clone(),
                style: Inline {
                    code: true,
                    ..Inline::default()
                },
                fg: None,
            }),
        }
        out.push(runs);
    }
    out
}
