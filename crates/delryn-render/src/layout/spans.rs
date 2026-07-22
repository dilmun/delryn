//! Styled-span word-wrap: flatten spans to glyph segments, greedily fill lines
//! (breaking long words at soft hyphens), then emit them with optional
//! justification — plus the narrow body-text spacing tidy-up.

use std::collections::VecDeque;

use delryn_model::{Anchor, Inline, Span, SpanMath};

use super::width::{char_width, display_width};
use super::{DisplayLine, LineKind, Run};

/// One unit carried through wrapping: a styled glyph (char + inline style + any
/// navigation anchor), or — when [`Glyph::math`] is set — an *atomic* inline-math
/// image occupying a fixed number of cells (its `ch` is a placeholder space the
/// reader paints the equation raster over; it is unbreakable and never coalesced
/// with neighbouring text).
#[derive(Clone)]
struct Glyph {
    ch: char,
    style: Inline,
    anchor: Option<Anchor>,
    /// `(inline-math id, reserved cell width, reserved cell height)` when this is an
    /// atomic inline-math cell; `None` for an ordinary glyph. A height > 1 (a fraction,
    /// always odd) makes its line reserve `(height-1)/2` blank spacer rows above and as
    /// many below, so the raster is centred on the text row (see [`emit_lines`]).
    math: Option<(usize, u16, u16)>,
}

impl Glyph {
    /// A plain text glyph.
    fn text(ch: char, style: Inline, anchor: Option<Anchor>) -> Glyph {
        Glyph {
            ch,
            style,
            anchor,
            math: None,
        }
    }

    /// Display width in terminal cells: the reserved width for an inline-math atom,
    /// else the glyph's own cell width (wide CJK = 2, combining = 0).
    fn width(&self) -> usize {
        match self.math {
            Some((_, cols, _)) => usize::from(cols),
            None => char_width(self.ch),
        }
    }
}

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
/// A wide CJK glyph is two cells, a combining mark none, an inline-math atom its
/// reserved width, so this is *not* the glyph count.
fn cells_width(glyphs: &[Glyph]) -> usize {
    glyphs.iter().map(Glyph::width).sum()
}

/// The prefix leading a wrapped block's first line vs. its continuation lines
/// (indent, quote bar, list marker + its hanging padding).
pub(super) struct Prefix<'a> {
    pub first: &'a str,
    pub cont: &'a str,
}

/// The reader's per-id inline-math reservations: reserved cell width and height per
/// section-local id (parallel slices). Empty ⇒ no graphical inline math on these spans
/// (the Unicode fallback), e.g. a caption or a following continuous section.
#[derive(Clone, Copy)]
pub(super) struct InlineMathDims<'a> {
    pub cols: &'a [u16],
    pub rows: &'a [u16],
}

/// Word-wrap styled spans into display lines, with a prefix on the first line
/// and a (usually padding) prefix on continuations. Three phases: flatten the
/// spans to break-segmented words, greedily fill lines, then emit them.
pub(super) fn wrap_spans(
    spans: &[Span],
    width: usize,
    prefix: Prefix,
    kind: LineKind,
    justify: bool,
    math: InlineMathDims,
    out: &mut Vec<DisplayLine>,
) {
    let (first_prefix, cont_prefix) = (prefix.first, prefix.cont);
    let words = flatten_to_words(spans, math);
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
///
/// A span the reader rasterised to an inline equation ([`SpanMath::Raster`]) with
/// a reserved width in `math_cols` becomes a single unbreakable atom glyph instead
/// of its Unicode text; if no width is provided (e.g. a following continuous
/// section, for which the reader draws no inline math), it falls back to its text.
fn flatten_to_words(spans: &[Span], math: InlineMathDims) -> Vec<Vec<Vec<Glyph>>> {
    let mut words: Vec<Vec<Vec<Glyph>>> = Vec::new(); // word → segments → glyphs
    let mut segs: Vec<Vec<Glyph>> = vec![Vec::new()];
    let mut in_word = false;
    for s in spans {
        // Graphical inline math: one atom glyph of its reserved cell width, glued to
        // adjacent text (so `$x$.` doesn't wrap before the period), never broken.
        if let Some(SpanMath::Raster { id, .. }) = &s.math
            && let Some(&cols) = math.cols.get(*id)
            && cols > 0
        {
            segs.last_mut().unwrap().push(Glyph {
                ch: ' ',
                style: s.style,
                anchor: s.anchor.clone(),
                math: Some((*id, cols, math.rows.get(*id).copied().unwrap_or(1).max(1))),
            });
            in_word = true;
            continue;
        }
        for c in s.text.chars() {
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
                segs.last_mut()
                    .unwrap()
                    .push(Glyph::text(c, s.style, s.anchor.clone()));
                in_word = true;
            }
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
                math: None,
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
                    math: None,
                });
            }
            push_word_runs(&piece.cells, &mut runs);
            if piece.hyphen {
                let st = piece.cells.last().map(|g| g.style).unwrap_or_default();
                runs.push(Run {
                    text: "-".to_string(),
                    style: st,
                    fg: None,
                    anchor: None,
                    math: None,
                });
            }
        }
        // A line carrying a multi-row inline equation (a fraction) reserves blank spacer
        // rows **above and below** it: the raster is centred on the text row, straddling the
        // line so its bar sits level with the prose instead of hanging under it. `(rows-1)/2`
        // rows each side (the reader paints the atom `rows` cells tall, starting that many
        // rows above the text line).
        let atom_rows = line
            .iter()
            .flat_map(|p| p.cells.iter())
            .filter_map(|g| g.math.map(|(_, _, r)| r))
            .max()
            .unwrap_or(1);
        let half = usize::from(atom_rows.saturating_sub(1) / 2);
        for _ in 0..half {
            out.push(DisplayLine {
                runs: Vec::new(),
                kind,
            });
        }
        out.push(DisplayLine { runs, kind });
        for _ in 0..half {
            out.push(DisplayLine {
                runs: Vec::new(),
                kind,
            });
        }
    }
}

/// Append one word's glyphs as runs, coalescing consecutive chars of equal style
/// *and* anchor (a word may mix styles — a bold glyph next to a normal one with
/// no whitespace between source spans — or carry a navigation anchor on part of
/// it, e.g. a footnote-ref superscript). An inline-math atom always becomes its
/// own run (its `text` is `cols` blank spaces the reader paints the equation
/// over) and never coalesces with surrounding text.
fn push_word_runs(word: &[Glyph], runs: &mut Vec<Run>) {
    let mut buf = String::new();
    let mut cur: Option<(Inline, Option<Anchor>)> = None;
    for g in word {
        if let Some((id, cols, _rows)) = g.math {
            flush_run(runs, &mut buf, &mut cur);
            runs.push(Run {
                text: " ".repeat(usize::from(cols)),
                style: g.style,
                anchor: g.anchor.clone(),
                math: Some(id),
                ..Default::default()
            });
            continue;
        }
        let key = (g.style, g.anchor.clone());
        if cur.as_ref() == Some(&key) {
            buf.push(g.ch);
        } else {
            flush_run(runs, &mut buf, &mut cur);
            buf.push(g.ch);
            cur = Some(key);
        }
    }
    flush_run(runs, &mut buf, &mut cur);
}

/// Flush the pending coalesced text (`buf` in style/anchor `cur`) as one run.
fn flush_run(runs: &mut Vec<Run>, buf: &mut String, cur: &mut Option<(Inline, Option<Anchor>)>) {
    if let Some((style, anchor)) = cur.take() {
        runs.push(Run {
            text: std::mem::take(buf),
            style,
            anchor,
            ..Default::default()
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
    use super::super::{DisplayLine, LineKind, Run, WrapOpts, wrap_blocks};
    use delryn_model::{Anchor, Block, Inline, Span, SpanMath};

    fn texts(lines: &[DisplayLine]) -> Vec<String> {
        lines.iter().map(DisplayLine::text).collect()
    }

    /// An inline-math run (Unicode `text`, a rasterised `id`) — the parser/reader
    /// output the wrapper turns into an atom when a width is reserved for its `id`.
    fn raster(text: &str, id: usize) -> Span {
        Span {
            text: text.into(),
            style: Inline {
                math: true,
                ..Inline::default()
            },
            anchor: None,
            math: Some(SpanMath::Raster {
                id,
                png: vec![],
                ink: None,
            }),
        }
    }

    fn atoms(lines: &[DisplayLine]) -> Vec<&Run> {
        lines
            .iter()
            .flat_map(|l| &l.runs)
            .filter(|r| r.math.is_some())
            .collect()
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
            math: None,
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
                math: None,
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
    fn inline_math_atom_reserves_cells_when_width_is_provided() {
        // A rasterised inline-math run with a reserved width becomes ONE atom run of
        // that many blank cells, carrying the inline-math id; the surrounding words
        // are untouched, and the atom glues to them as a single wrap unit.
        let block = para_spans(vec![
            Span::plain("let "),
            raster("x²", 0),
            Span::plain(" be"),
        ]);
        let opts = WrapOpts {
            width: 80,
            inline_math_cols: &[3], // id 0 reserves 3 cells
            ..Default::default()
        };
        let lines = wrap_blocks(&[block], &opts, &[]);
        let atoms = atoms(&lines);
        assert_eq!(atoms.len(), 1, "exactly one atom run");
        assert_eq!(atoms[0].math, Some(0), "carries the inline-math id");
        assert_eq!(
            atoms[0].text, "   ",
            "reserves 3 blank cells for the raster"
        );
        // The atom sits between the words (a gap space each side of its 3 cells),
        // its blank cells standing in for the equation the reader paints over them.
        assert_eq!(texts(&lines).join(""), "let     be");
    }

    #[test]
    fn inline_math_falls_back_to_unicode_without_a_reserved_width() {
        // With no reserved width (empty `inline_math_cols` — a following continuous
        // section, which the reader doesn't draw inline math for), the rasterised run
        // degrades to its Unicode text and produces no atom.
        let block = para_spans(vec![
            Span::plain("let "),
            raster("x²", 0),
            Span::plain(" be"),
        ]);
        let lines = wrap_blocks(&[block], &WrapOpts::default(), &[]);
        assert!(atoms(&lines).is_empty(), "no atom without a reserved width");
        assert_eq!(
            texts(&lines).join(""),
            "let x² be",
            "shows the Unicode fallback"
        );
    }

    #[test]
    fn multi_row_inline_math_reserves_spacers_around_the_line() {
        // A rasterised inline-math atom reserving an odd row count (a fraction: 3 = one
        // spacer above, the text row, one below) makes its wrapped line reserve blank
        // spacer lines **above and below** it, so the raster straddles the line (centred on
        // it) instead of hanging under it. A one-row atom inserts no spacer.
        let mk = || {
            para_spans(vec![
                Span::plain("let "),
                raster("½", 0),
                Span::plain(" be"),
            ])
        };
        let three_row = WrapOpts {
            width: 80,
            inline_math_cols: &[3],
            inline_math_rows: &[3],
            para_spacing: 0,
            ..Default::default()
        };
        let lines = wrap_blocks(&[mk()], &three_row, &[]);
        let atom_line = lines
            .iter()
            .position(|l| l.runs.iter().any(|r| r.math.is_some()))
            .expect("a line carrying the atom");
        assert!(
            atom_line >= 1
                && lines[atom_line - 1].text().trim().is_empty()
                && lines
                    .get(atom_line + 1)
                    .is_some_and(|l| l.text().trim().is_empty()),
            "blank spacer lines bracket the multi-row atom: {:?}",
            texts(&lines)
        );

        // One row (no `inline_math_rows`) → no spacer, a single line.
        let one_row = WrapOpts {
            width: 80,
            inline_math_cols: &[3],
            para_spacing: 0,
            ..Default::default()
        };
        assert_eq!(
            wrap_blocks(&[mk()], &one_row, &[]).len(),
            1,
            "one-row atom inserts no spacer"
        );
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
