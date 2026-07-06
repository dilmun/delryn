//! Styled-span word-wrap: flatten spans to glyph segments, greedily fill lines
//! (breaking long words at soft hyphens), then emit them with optional
//! justification — plus the narrow body-text spacing tidy-up.

use std::collections::VecDeque;

use delryn_model::{Anchor, Inline, Span};

use super::width::{char_width, display_width};
use super::{DisplayLine, LineKind, Run};

/// One styled glyph carried through wrapping: char + its inline style + any
/// navigation anchor (footnote ref, link, …).
type Glyph = (char, Inline, Option<Anchor>);

/// Soft hyphen (U+00AD): an invisible break opportunity inside a word. Dropped
/// from the rendered text, but a real `-` is shown when a word breaks there.
const SOFT_HYPHEN: char = '\u{00AD}';

/// A piece of a word placed on a line: its glyphs plus whether a hyphen follows
/// (true when a long word was broken at a soft hyphen).
struct Piece {
    cells: Vec<Glyph>,
    hyphen: bool,
}

/// Display width (terminal cells) of a run of glyphs — the fill/justify metric.
/// A wide CJK glyph is two cells, a combining mark none, so this is *not* the
/// glyph count.
fn cells_width(glyphs: &[Glyph]) -> usize {
    glyphs.iter().map(|(c, _, _)| char_width(*c)).sum()
}

/// Word-wrap styled spans into display lines, with a prefix on the first line
/// and a (usually padding) prefix on continuations. Three phases: flatten the
/// spans to break-segmented words, greedily fill lines, then emit them.
pub(super) fn wrap_spans(
    spans: &[Span],
    width: usize,
    first_prefix: &str,
    cont_prefix: &str,
    kind: LineKind,
    justify: bool,
    out: &mut Vec<DisplayLine>,
) {
    let words = flatten_to_words(spans);
    if words.is_empty() {
        return;
    }
    let lines = fill_lines(words, width, first_prefix, cont_prefix);
    emit_lines(lines, width, first_prefix, cont_prefix, kind, justify, out);
}

/// The prefix that leads line `line` (first line vs. continuation).
fn prefix_for<'a>(first: &'a str, cont: &'a str, line: usize) -> &'a str {
    if line == 0 { first } else { cont }
}

/// Columns available for content on line `line`, after its prefix.
fn avail(width: usize, first: &str, cont: &str, line: usize) -> usize {
    width
        .saturating_sub(display_width(prefix_for(first, cont, line)))
        .max(1)
}

/// Phase 1 — flatten the spans to glyphs preserving adjacency, then split into
/// words on *actual* whitespace. A word is a run of non-whitespace glyphs that
/// may span several source spans — so adjacent spans with no whitespace between
/// them join with no inserted space (matters for math emitted one span per
/// glyph: `𝔼`,`(`,`X`,`)` must stay `𝔼(X)`, not `𝔼 ( X )`). Within a word, soft
/// hyphens split it into break segments (the SHY itself is dropped).
fn flatten_to_words(spans: &[Span]) -> Vec<Vec<Vec<Glyph>>> {
    let chars = spans
        .iter()
        .flat_map(|s| s.text.chars().map(move |c| (c, s.style, s.anchor.clone())));
    let mut words: Vec<Vec<Vec<Glyph>>> = Vec::new(); // word → segments → glyphs
    let mut segs: Vec<Vec<Glyph>> = vec![Vec::new()];
    let mut in_word = false;
    for (c, st, an) in chars {
        if c.is_whitespace() {
            if in_word {
                segs.retain(|s| !s.is_empty());
                words.push(std::mem::take(&mut segs));
                segs = vec![Vec::new()];
                in_word = false;
            }
        } else if c == SOFT_HYPHEN {
            // A break opportunity: start a new segment (ignore a leading SHY).
            if in_word && !segs.last().is_none_or(Vec::is_empty) {
                segs.push(Vec::new());
            }
        } else {
            segs.last_mut().unwrap().push((c, st, an));
            in_word = true;
        }
    }
    if in_word {
        segs.retain(|s| !s.is_empty());
        words.push(segs);
    }
    words
}

/// Phase 2 — greedy line fill, breaking over-long words at soft hyphens when it
/// helps. Each line is a list of placed [`Piece`]s.
fn fill_lines(
    words: Vec<Vec<Vec<Glyph>>>,
    width: usize,
    first: &str,
    cont: &str,
) -> Vec<Vec<Piece>> {
    let mut lines: Vec<Vec<Piece>> = Vec::new();
    let mut cur: Vec<Piece> = Vec::new();
    let mut cur_w = 0usize; // width of `cur` excluding the prefix
    let mut queue: VecDeque<Vec<Vec<Glyph>>> = words.into();
    while let Some(word) = queue.pop_front() {
        let av = avail(width, first, cont, lines.len());
        let gap = usize::from(!cur.is_empty());
        let wfull: usize = word.iter().map(|seg| cells_width(seg)).sum();
        if cur_w + gap + wfull <= av {
            cur.push(Piece {
                cells: word.into_iter().flatten().collect(),
                hyphen: false,
            });
            cur_w += gap + wfull;
            continue;
        }
        // Try to break the word at the latest soft-hyphen boundary that fits the
        // already-placed prefix plus a trailing '-'.
        if word.len() > 1 {
            let (mut acc, mut best_k) = (0usize, 0usize);
            for (k, seg) in word.iter().take(word.len() - 1).enumerate() {
                acc += cells_width(seg);
                // `+ 1` for the trailing hyphen (folded into `<` per clippy).
                if cur_w + gap + acc < av {
                    best_k = k + 1;
                } else {
                    break;
                }
            }
            if best_k > 0 {
                cur.push(Piece {
                    cells: word[..best_k].iter().flatten().cloned().collect(),
                    hyphen: true,
                });
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
                queue.push_front(word[best_k..].to_vec());
                continue;
            }
        }
        if !cur.is_empty() {
            // No break point here: flush the line and retry the word on a fresh one.
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
            queue.push_front(word);
            continue;
        }
        // Empty line and the word is wider than the column with no break point:
        // place it whole (overflow), as before — one over-long token per line.
        cur.push(Piece {
            cells: word.into_iter().flatten().collect(),
            hyphen: false,
        });
        lines.push(std::mem::take(&mut cur));
        cur_w = 0;
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Phase 3 — emit. Full justification (when enabled, body only) distributes the
/// leftover columns across inter-word gaps — never on the last line of the
/// paragraph or a single-piece line.
fn emit_lines(
    lines: Vec<Vec<Piece>>,
    width: usize,
    first: &str,
    cont: &str,
    kind: LineKind,
    justify: bool,
    out: &mut Vec<DisplayLine>,
) {
    let last = lines.len().saturating_sub(1);
    let justify_body = justify && matches!(kind, LineKind::Body);
    for (li, line) in lines.iter().enumerate() {
        let prefix = prefix_for(first, cont, li);
        let pieces_w: usize = line
            .iter()
            .map(|p| cells_width(&p.cells) + usize::from(p.hyphen))
            .sum();
        let gaps = line.len().saturating_sub(1);
        let justify_line = justify_body && li != last && gaps >= 1;
        let slack = if justify_line {
            avail(width, first, cont, li).saturating_sub(pieces_w + gaps)
        } else {
            0
        };

        let mut runs: Vec<Run> = Vec::new();
        if !prefix.is_empty() {
            runs.push(Run {
                text: prefix.to_string(),
                style: Inline::default(),
                fg: None,
                anchor: None,
            });
        }
        for (pi, piece) in line.iter().enumerate() {
            if pi > 0 {
                let extra = if justify_line {
                    slack / gaps + usize::from(pi - 1 < slack % gaps)
                } else {
                    0
                };
                runs.push(Run {
                    text: " ".repeat(1 + extra),
                    style: Inline::default(),
                    fg: None,
                    anchor: None,
                });
            }
            push_word_runs(&piece.cells, &mut runs);
            if piece.hyphen {
                let st = piece.cells.last().map(|(_, s, _)| *s).unwrap_or_default();
                runs.push(Run {
                    text: "-".to_string(),
                    style: st,
                    fg: None,
                    anchor: None,
                });
            }
        }
        out.push(DisplayLine { runs, kind });
    }
}

/// Append one word's chars as runs, coalescing consecutive chars of equal style
/// *and* anchor (a word may mix styles — a bold glyph next to a normal one with
/// no whitespace between source spans — or carry a navigation anchor on part of
/// it, e.g. a footnote-ref superscript).
fn push_word_runs(word: &[Glyph], runs: &mut Vec<Run>) {
    let mut buf = String::new();
    let mut cur: Option<(Inline, Option<Anchor>)> = None;
    for (c, st, an) in word {
        let key = (*st, an.clone());
        if cur.as_ref() == Some(&key) {
            buf.push(*c);
        } else {
            if let Some((s, a)) = cur.take() {
                runs.push(Run {
                    text: std::mem::take(&mut buf),
                    style: s,
                    fg: None,
                    anchor: a,
                });
            }
            buf.push(*c);
            cur = Some(key);
        }
    }
    if let Some((s, a)) = cur {
        runs.push(Run {
            text: buf,
            style: s,
            fg: None,
            anchor: a,
        });
    }
}

/// Collapse the stray space some converters leave between a short *styled*
/// variable and a hyphenated suffix: `<i>t</i> -distribution` → `t-distribution`
/// (also `p-value`, `F-test`). Returns rewritten spans only when something
/// changed. Deliberately narrow — the trigger is a short italic/bold/math/code
/// token immediately before a space + hyphen + letter, so it never touches
/// numbers (`16. 3`), `p < 0.05`, dashes, or ordinary prose.
pub(super) fn tidy_spacing(spans: &[Span]) -> Option<Vec<Span>> {
    if !(1..spans.len()).any(|i| stripped_suffix(&spans[i - 1], &spans[i]).is_some()) {
        return None;
    }
    let mut out = spans.to_vec();
    for i in 1..out.len() {
        if let Some(text) = stripped_suffix(&out[i - 1], &out[i]) {
            out[i].text = text;
        }
    }
    Some(out)
}

/// If `prev` is a short styled variable and `next` begins with " -<letter>", the
/// `next` text with that leading space removed; else `None`.
fn stripped_suffix(prev: &Span, next: &Span) -> Option<String> {
    let st = prev.style;
    if !(st.italic || st.bold || st.math || st.code) {
        return None;
    }
    let tok = prev.text.trim();
    if tok.is_empty() || tok.chars().count() > 3 || tok.chars().any(char::is_whitespace) {
        return None;
    }
    let rest = next.text.trim_start_matches(' ');
    if rest.len() == next.text.len() {
        return None; // no leading space to drop
    }
    let mut cs = rest.chars();
    if !matches!(cs.next(), Some('-' | '\u{2010}' | '\u{2011}')) {
        return None;
    }
    cs.next().filter(|c| c.is_alphanumeric())?;
    Some(rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::{DisplayLine, LineKind, WrapOpts, wrap_blocks};
    use delryn_model::{Anchor, Block, Inline, Span};

    fn texts(lines: &[DisplayLine]) -> Vec<String> {
        lines.iter().map(DisplayLine::text).collect()
    }

    fn para(s: &str) -> Block {
        Block::Para {
            spans: vec![Span::plain(s)],
            indent: 0,
            quote: false,
            marker: None,
        }
    }

    fn ital(s: &str) -> Span {
        Span {
            text: s.into(),
            style: Inline {
                italic: true,
                ..Default::default()
            },
            anchor: None,
        }
    }

    fn para_spans(spans: Vec<Span>) -> Block {
        Block::Para {
            spans,
            indent: 0,
            quote: false,
            marker: None,
        }
    }

    #[test]
    fn cjk_wide_glyphs_are_packed_by_display_width_not_char_count() {
        // Each 2-cell CJK word is 2 chars = 4 cells. At width 10 the greedy fill
        // packs two words per line (4+1+4 = 9 cells); a char-count metric would
        // wrongly fit a third (7 chars ≤ 10) and overrun to 14 cells. Assert every
        // line stays within the column measured in *display cells*.
        let block = para("日本 語の テキ スト です ねこ れは かえ");
        let opts = WrapOpts {
            width: 10,
            ..Default::default()
        };
        let lines = texts(&wrap_blocks(&[block], &opts, &[]));
        assert!(lines.len() >= 2, "wraps to several lines: {lines:?}");
        for line in &lines {
            assert!(
                super::display_width(line) <= 10,
                "line overruns the 10-cell column: {line:?} = {} cells",
                super::display_width(line)
            );
        }
    }

    #[test]
    fn anchors_survive_wrapping_onto_their_run() {
        // A footnote-ref span must reach the view as a Run carrying its Anchor,
        // so the reader's link cursor can locate and follow it.
        let spans = vec![
            Span::plain("see "),
            Span {
                text: "7".to_string(),
                style: Inline::default(),
                anchor: Some(Anchor::Footnote("fn7".to_string())),
            },
            Span::plain(" for details"),
        ];
        let block = Block::Para {
            spans,
            indent: 0,
            quote: false,
            marker: None,
        };
        let lines = wrap_blocks(&[block], &WrapOpts::default(), &[]);
        let anchored: Vec<(&str, &Anchor)> = lines
            .iter()
            .flat_map(|l| &l.runs)
            .filter_map(|r| r.anchor.as_ref().map(|a| (r.text.as_str(), a)))
            .collect();
        assert_eq!(anchored.len(), 1, "exactly the ref run is anchored");
        assert_eq!(anchored[0].0, "7");
        assert_eq!(anchored[0].1, &Anchor::Footnote("fn7".to_string()));
    }

    #[test]
    fn adjacent_spans_join_without_inserted_spaces() {
        // Per-glyph spans (as InDesign MathTools emits for math) with explicit
        // space spans only where the publisher intended them. The wrapper must
        // join touching glyphs with no space, and honour the real spaces.
        let glyphs = ["𝔼", "(", "X", ")", " ", "=", " ", "∑", "x"];
        let spans: Vec<Span> = glyphs.iter().map(|g| Span::plain(*g)).collect();
        let block = Block::Para {
            spans,
            indent: 0,
            quote: false,
            marker: None,
        };
        let joined = texts(&wrap_blocks(&[block], &WrapOpts::default(), &[])).join("\n");
        assert!(joined.contains("𝔼(X) = ∑x"), "{joined:?}");
        assert!(
            !joined.contains("𝔼 ( X )"),
            "no per-glyph spaces: {joined:?}"
        );
    }

    #[test]
    fn whitespace_runs_collapse_to_single_space() {
        let block = Block::Para {
            spans: vec![Span::plain("a   b\tc")],
            indent: 0,
            quote: false,
            marker: None,
        };
        let joined = texts(&wrap_blocks(&[block], &WrapOpts::default(), &[])).join("\n");
        assert!(joined.contains("a b c"), "{joined:?}");
    }

    #[test]
    fn tidy_collapses_styled_variable_hyphen() {
        // `<i>t</i> -distribution` (a converter artifact) → `t-distribution`.
        let block = para_spans(vec![
            Span::plain("the "),
            ital("t"),
            Span::plain(" -distribution is wide"),
        ]);
        let on = WrapOpts {
            width: 80,
            tidy_spacing: true,
            ..Default::default()
        };
        let j = texts(&wrap_blocks(std::slice::from_ref(&block), &on, &[])).join(" ");
        assert!(j.contains("t-distribution"), "tidied: {j:?}");
        assert!(!j.contains("t -distribution"));

        // With the toggle off we render faithfully (space kept).
        let off = WrapOpts {
            width: 80,
            tidy_spacing: false,
            ..Default::default()
        };
        let j = texts(&wrap_blocks(&[block], &off, &[])).join(" ");
        assert!(j.contains("t -distribution"), "faithful: {j:?}");
    }

    #[test]
    fn tidy_leaves_numbers_operators_and_prose() {
        let opts = WrapOpts {
            width: 80,
            ..Default::default()
        }; // tidy on by default
        let render = |b| texts(&wrap_blocks(&[b], &opts, &[])).join(" ");
        // No hyphen → number untouched (`16. 3` style is content, never rewritten).
        assert!(render(para_spans(vec![ital("t"), Span::plain(" = 16.3")])).contains("t = 16.3"));
        // A comparison operator, not a hyphen → untouched.
        assert!(render(para_spans(vec![ital("p"), Span::plain(" < 0.05")])).contains("p < 0.05"));
        // Unstyled preceding token → untouched even with a hyphen.
        assert!(
            render(para_spans(vec![
                Span::plain("well"),
                Span::plain(" -being")
            ]))
            .contains("well -being")
        );
    }

    #[test]
    fn justify_fills_inner_lines_not_the_last() {
        let block = para(
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron",
        );
        let opts = WrapOpts {
            width: 24,
            justify: true,
            para_spacing: 0,
            ..Default::default()
        };
        let lines = wrap_blocks(&[block], &opts, &[]);
        let body: Vec<&DisplayLine> = lines
            .iter()
            .filter(|l| matches!(l.kind, LineKind::Body) && !l.runs.is_empty())
            .collect();
        assert!(body.len() >= 2, "wraps to several lines");
        for l in &body[..body.len() - 1] {
            let w: usize = l.runs.iter().map(|r| r.text.chars().count()).sum();
            assert_eq!(w, 24, "inner line justified to full width: {:?}", l.text());
        }
        let last: usize = body
            .last()
            .unwrap()
            .runs
            .iter()
            .map(|r| r.text.chars().count())
            .sum();
        assert!(last <= 24, "last line not padded past width");
    }

    #[test]
    fn soft_hyphen_breaks_long_word_with_visible_hyphen() {
        let shy = '\u{00AD}';
        let word = format!("super{shy}cali{shy}fragi{shy}listic");
        let block = para(&format!("a {word} end"));
        let opts = WrapOpts {
            width: 12,
            ..Default::default()
        };
        let lines = texts(&wrap_blocks(&[block], &opts, &[]));
        let joined = lines.join("|");
        assert!(
            !joined.contains(shy),
            "soft hyphens never render: {joined:?}"
        );
        assert!(
            joined.contains('-'),
            "long word breaks with a hyphen: {joined:?}"
        );
        for l in &lines {
            assert!(l.chars().count() <= 12, "line stays within width: {l:?}");
        }
    }
}
