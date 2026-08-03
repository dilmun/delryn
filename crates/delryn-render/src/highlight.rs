//! Syntax highlighting for code blocks via syntect. Loaded once and reused.
//! Colour selection ties into the theme system later; for now a fixed dark
//! syntax theme. See `DESIGN.md` §1 (programming-book wedge).

use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

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

/// The syntax a code block resolves to **on its own evidence**: the fenced
/// `lang`, else the first line (shebang/modeline), else a content guess. `None`
/// when nothing is distinctive enough.
///
/// Cheap — it inspects text and never highlights — so a caller can tally what
/// languages a section actually uses before rendering any of it.
fn detect_syntax(lines: &[String], lang: Option<&str>) -> Option<&'static SyntaxReference> {
    let ps = syntaxes();
    let syntax = lang
        .and_then(|l| ps.find_syntax_by_token(l))
        .or_else(|| lines.first().and_then(|l| ps.find_syntax_by_first_line(l)))
        .or_else(|| guess_language(lines).and_then(|g| ps.find_syntax_by_token(g)))?;
    (syntax.name != "Plain Text").then_some(syntax)
}

/// The display name of the language a block identifies itself as, or `None`.
/// The tallying half of [`highlight_code`]'s fallback: pass the winner back in as
/// `fallback` and the blocks that couldn't identify themselves follow the ones
/// that could.
pub fn detect_language(lines: &[String], lang: Option<&str>) -> Option<&'static str> {
    detect_syntax(lines, lang).map(|s| s.name.as_str())
}

/// Look a fallback up by display name ("C++") or token ("cpp"), so a caller can
/// pass back either what [`detect_language`] returned or a bare token.
fn syntax_for(hint: &str) -> Option<&'static SyntaxReference> {
    let ps = syntaxes();
    ps.find_syntax_by_name(hint)
        .or_else(|| ps.find_syntax_by_token(hint))
        .filter(|s| s.name != "Plain Text")
}

/// Highlight code into per-line styled runs (one inner `Vec<Run>` per input
/// line) using the named syntect theme, plus the resolved language's display
/// name (`None` when it falls back to plain text) for the block's tag.
///
/// Language comes from the block's own evidence first ([`detect_syntax`]). When
/// that finds nothing, `fallback` decides — the language the rest of the book is
/// written in. Technical books routinely leave a short listing unmarked (a
/// declaration, a signature, a fragment with none of the usual give-aways), and
/// the answer is never "this one block is a different language"; it is the
/// language of every other listing around it. Without a fallback such a block
/// renders as flat grey text beside its highlighted neighbours.
pub fn highlight_code(
    lines: &[String],
    lang: Option<&str>,
    fallback: Option<&str>,
    theme_name: &str,
) -> (Vec<Vec<Run>>, Option<String>) {
    let ps = syntaxes();
    let syntax = detect_syntax(lines, lang)
        .or_else(|| fallback.and_then(syntax_for))
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
                        math: None,
                        break_hyphen: false,
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
                math: None,
                break_hyphen: false,
            }),
        }
        out.push(runs);
    }
    (out, lang_name)
}

/// Language names a book might put in its own title, longest match first, paired
/// with the syntect token they mean.
///
/// Order is load-bearing: "javascript" has to be tested before "java", "c++"
/// before "c". Entries are matched as whole words (see [`language_from_title`]),
/// which is what keeps "Go" out of "Algorithms" and "C" out of "Clean".
const TITLE_LANGUAGES: &[(&str, &str)] = &[
    ("javascript", "js"),
    ("typescript", "ts"),
    ("objective-c", "objc"),
    ("c++", "cpp"),
    ("cpp", "cpp"),
    ("c#", "cs"),
    ("csharp", "cs"),
    ("python", "py"),
    ("rust", "rs"),
    ("kotlin", "kt"),
    ("haskell", "hs"),
    ("clojure", "clj"),
    ("scala", "scala"),
    ("swift", "swift"),
    ("ruby", "rb"),
    ("perl", "pl"),
    ("lua", "lua"),
    ("erlang", "erl"),
    ("elixir", "ex"),
    ("golang", "go"),
    ("java", "java"),
    ("php", "php"),
    ("sql", "sql"),
    ("bash", "sh"),
    ("shell", "sh"),
    ("html", "html"),
    ("css", "css"),
    ("go", "go"),
    ("r", "r"),
    ("c", "c"),
];

/// The language a book announces in its title — "C++ Memory Management" is a C++
/// book, and its unmarked listings are C++ too.
///
/// The last resort behind the block's own evidence and the rest of the section's,
/// so it only decides for a book whose listings are *all* unmarked. Matched on
/// word boundaries: a bare "c" or "go" as a word is a language, the same letters
/// inside "Clean" or "Algorithms" are not.
pub fn language_from_title(title: &str) -> Option<&'static str> {
    let lower = title.to_ascii_lowercase();
    // Split on everything that isn't part of a language name — `+` and `#` stay
    // so "c++" and "c#" survive as single words.
    let words: Vec<&str> = lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '#'))
        .filter(|w| !w.is_empty())
        .collect();
    TITLE_LANGUAGES
        .iter()
        .find(|(name, _)| words.contains(name))
        .map(|(_, token)| *token)
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
        let (runs, name) = highlight_code(&lines, Some("python"), None, "InspiredGitHub");
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
        let (_, name) = highlight_code(&py, None, None, "InspiredGitHub");
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
        let (_, name) = highlight_code(&flow, None, None, "InspiredGitHub");
        assert_eq!(name.as_deref(), Some("Python"));

        // Prose in a code box → no strong signal → stays plain.
        let prose: Vec<String> = ["This is ordinary text.", "Not code at all."]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (_, name) = highlight_code(&prose, None, None, "InspiredGitHub");
        assert_eq!(name, None);
    }

    /// The reported case: a listing of C declarations carrying none of the
    /// markers the content guess looks for (no `#include`, no `printf`, no
    /// `std::`) rendered as flat grey text beside highlighted neighbours.
    #[test]
    fn a_block_that_cannot_identify_itself_follows_the_fallback() {
        let lines: Vec<String> = [
            "// opens the file called \"name\", returns a pointer",
            "FILE *open_file(const char *name);",
            "int read_from(FILE *file, char *buf, int capacity);",
            "// closes file. Precondition: file is non-null and valid",
            "void close_file(FILE *file);",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        assert_eq!(
            detect_language(&lines, None),
            None,
            "nothing distinctive enough to identify it on its own"
        );
        let (_, name) = highlight_code(&lines, None, None, "InspiredGitHub");
        assert_eq!(name, None, "and with no fallback it stays plain");

        // Given the language the rest of the book is in, it highlights as that.
        let (runs, name) = highlight_code(&lines, None, Some("C++"), "InspiredGitHub");
        assert_eq!(name.as_deref(), Some("C++"));
        let colours: std::collections::HashSet<_> =
            runs.iter().flatten().filter_map(|r| r.fg).collect();
        assert!(
            colours.len() > 1,
            "actually highlighted, not one flat colour: {colours:?}"
        );
    }

    /// A fallback never overrides a block that knows what it is — a shell
    /// transcript in a Python book stays shell.
    #[test]
    fn the_fallback_never_overrides_a_blocks_own_evidence() {
        let sh: Vec<String> = ["#!/bin/bash", "echo hello"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (_, name) = highlight_code(&sh, None, Some("py"), "InspiredGitHub");
        assert_ne!(name.as_deref(), Some("Python"), "own evidence wins");

        let marked: Vec<String> = vec!["SELECT 1;".to_string()];
        let (_, name) = highlight_code(&marked, Some("sql"), Some("py"), "InspiredGitHub");
        assert_eq!(name.as_deref(), Some("SQL"), "an explicit lang wins");
    }

    /// Titles name their language often enough to be worth reading, but only on
    /// word boundaries — "Clean Code" is not a C book.
    #[test]
    fn a_books_title_names_its_language_without_false_positives() {
        for (title, want) in [
            ("C++ Memory Management", Some("cpp")),
            ("The C Programming Language", Some("c")),
            ("C# in Depth", Some("cs")),
            ("JavaScript: The Good Parts", Some("js")),
            ("Effective Java", Some("java")),
            ("Programming Rust", Some("rs")),
            ("Learning Python, 5th Edition", Some("py")),
            ("Go in Action", Some("go")),
            ("The Art of R Programming", Some("r")),
            // No language named — and the letters of one inside another word
            // must not count.
            ("Clean Code", None),
            ("Introduction to Algorithms", None),
            ("Designing Data-Intensive Applications", None),
        ] {
            assert_eq!(language_from_title(title), want, "title: {title}");
        }
    }
}
