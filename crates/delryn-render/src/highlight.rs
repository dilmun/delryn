//! Syntax highlighting for code blocks via syntect. Loaded once and reused.
//! Colour selection ties into the theme system later; for now a fixed dark
//! syntax theme. See `DESIGN.md` §1 (programming-book wedge).

use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::layout::Run;
use delryn_model::Inline;

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEMES: OnceLock<ThemeSet> = OnceLock::new();

fn syntaxes() -> &'static SyntaxSet {
    // The *no-newlines* variant: our source lines carry no trailing `\n`, and the
    // newline grammars need one to close line-scoped rules (e.g. `#` comments) —
    // without it a comment's style leaks into every following line of the block.
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_nonewlines)
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
/// line) using the named syntect theme, plus the resolved language's display
/// name (`None` when it falls back to plain text) for the block's tag. Language
/// is taken from the fenced `lang`, else the first line (shebang/modeline), else
/// a content guess (many book code blocks carry no language annotation), else
/// plain text.
pub fn highlight_code(
    lines: &[String],
    lang: Option<&str>,
    theme_name: &str,
) -> (Vec<Vec<Run>>, Option<String>) {
    let ps = syntaxes();
    let syntax = lang
        .and_then(|l| ps.find_syntax_by_token(l))
        .or_else(|| lines.first().and_then(|l| ps.find_syntax_by_first_line(l)))
        .or_else(|| guess_language(lines).and_then(|g| ps.find_syntax_by_token(g)))
        .unwrap_or_else(|| ps.find_syntax_plain_text());
    let lang_name = (syntax.name != "Plain Text").then(|| syntax.name.clone());

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
                            math: false,
                        },
                        fg: Some((c.r, c.g, c.b)),
                        anchor: None,
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
                anchor: None,
            }),
        }
        out.push(runs);
    }
    (out, lang_name)
}

/// Best-effort language guess from code *content*, for blocks with no language
/// annotation and no first-line hint (common in books that mark code by a
/// container class only). Returns a syntect token (file extension) only on a
/// strong, distinctive signal (≥2 markers) so it never mis-colours a prose box;
/// `None` leaves the block plain.
fn guess_language(lines: &[String]) -> Option<&'static str> {
    let text = lines.join("\n");
    let up = text.to_ascii_uppercase();
    let hits = |needles: &[&str]| needles.iter().filter(|s| text.contains(**s)).count();
    // Python snippets often carry no distinctive keyword (just for/if/else flow),
    // so also recognise its block structure: a line that begins with a block
    // keyword and ends with a colon. Each such line is a strong Python signal.
    let py_struct = lines
        .iter()
        .filter(|l| {
            let t = l.trim();
            t.ends_with(':')
                && [
                    "def ", "class ", "for ", "if ", "elif ", "else", "while ", "try", "with ",
                    "except",
                ]
                .iter()
                .any(|kw| t.starts_with(kw))
        })
        .count();
    let scores = [
        (
            "py",
            py_struct
                + hits(&[
                    "def ",
                    "import ",
                    "elif",
                    "print(",
                    "lambda ",
                    "self.",
                    "range(",
                    ".iterrows(",
                    ".groupby(",
                ]),
        ),
        (
            "rs",
            hits(&[
                "fn ", "let mut ", "impl ", " -> ", "::", "pub fn", "match ", "&str", "println!",
            ]),
        ),
        (
            "js",
            hits(&[
                "function ",
                "=>",
                "const ",
                "console.",
                "require(",
                "var ",
                "export ",
            ]),
        ),
        (
            "c",
            hits(&[
                "#include", "int main", "printf(", "scanf(", "malloc(", "std::", "cout <<",
            ]),
        ),
        (
            "java",
            hits(&[
                "public class",
                "System.out",
                "public static void",
                "import java",
            ]),
        ),
        (
            "sh",
            hits(&["#!/bin", "echo ", "$(", "\nthen", "\nfi", "\ndone"]),
        ),
        (
            "html",
            hits(&["</", "<div", "<html", "<span", "<p>", "<a "]),
        ),
    ];
    // SQL keywords are case-insensitive.
    let sql = [
        "SELECT ",
        "FROM ",
        "WHERE ",
        "INSERT INTO",
        "CREATE TABLE",
        "GROUP BY",
        " JOIN ",
    ]
    .iter()
    .filter(|s| up.contains(**s))
    .count();
    let best = scores
        .into_iter()
        .chain(std::iter::once(("sql", sql)))
        .max_by_key(|&(_, s)| s)?;
    (best.1 >= 2).then_some(best.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `#` comment must not leak its style into the following code lines — the
    /// bug from feeding no-newline lines to the newline grammars.
    #[test]
    fn comment_does_not_leak_style_into_following_lines() {
        let lines: Vec<String> = [
            "#!/usr/bin/env python",
            "# -*- coding: utf-8 -*-",
            "import os",
            "def f():",
            "    return 1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (runs, name) = highlight_code(&lines, Some("python"), "InspiredGitHub");
        assert_eq!(name.as_deref(), Some("Python"));
        // The comment lines (0,1) may be italic; the code lines (2..) must not all
        // inherit the comment's italic/colour.
        for (i, line) in runs.iter().enumerate().skip(2) {
            assert!(
                !line.iter().all(|r| r.style.italic),
                "line {i} is entirely italic — comment style leaked into the code"
            );
        }
        // The keyword `def` on line 3 gets its own (non-comment) colour.
        let def_fg = runs[3].first().and_then(|r| r.fg);
        let comment_fg = runs[0].first().and_then(|r| r.fg);
        assert_ne!(
            def_fg, comment_fg,
            "code keyword should not share the comment colour"
        );
    }

    #[test]
    fn guesses_language_from_content_when_unannotated() {
        // Python with no lang and no shebang → guessed and highlighted.
        let py: Vec<String> = ["import os", "def f(x):", "    return x + 1", "print(f(2))"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (_, name) = highlight_code(&py, None, "InspiredGitHub");
        assert_eq!(name.as_deref(), Some("Python"));

        // Control-flow-only Python (no def/import) — detected via block structure.
        let flow: Vec<String> = [
            "for j in range(len(data)-1):",
            "    if data[j] > 0:",
            "        status = 1",
            "    else:",
            "        status = 0",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (_, name) = highlight_code(&flow, None, "InspiredGitHub");
        assert_eq!(name.as_deref(), Some("Python"));

        // Prose in a code box → no strong signal → stays plain.
        let prose: Vec<String> = ["This is ordinary text.", "Not code at all."]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (_, name) = highlight_code(&prose, None, "InspiredGitHub");
        assert_eq!(name, None);
    }
}
