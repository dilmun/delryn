//! Knuth–Plass optimal line breaking.
//!
//! The greedy filler takes as many words as fit on each line and lets the leftover slack
//! fall where it may. That is locally optimal and globally poor: a line ending just before
//! a long word has to stretch to cover the hole it left, and since the next line starts
//! fresh the damage never gets shared. Repeated down a column, those stretched gaps line
//! up into the white "rivers" that make justified terminal text look broken.
//!
//! Knuth–Plass scores the paragraph as a whole. Every line gets a *badness* from how far
//! its gaps had to stretch, badness is squared into *demerits* so one very loose line costs
//! more than several slightly loose ones, and the breaking with the lowest total wins. The
//! practical effect is that the breaker will happily leave a word behind — making one line
//! slightly tighter — to save the following line from gaping.
//!
//! This is a shortest-path problem over break positions, solved with the obvious dynamic
//! program: `best[i]` is the cheapest way to reach a line starting at atom `i`.
//!
//! Two things a terminal changes versus TeX. Spacing is integral (a gap is a whole cell, so
//! the adjustment ratio is *extra cells per gap*, and there is no shrink — you cannot set a
//! space narrower than one column). And the column is narrow, so a paragraph rarely admits
//! a feasible breaking at all when it contains a token wider than the measure; the caller
//! is expected to fall back to greedy when [`break_paragraph`] returns `None`.

/// What separates two adjacent atoms, and therefore what a break between them costs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Join {
    /// An inter-word space: one cell inside a line, dropped when the line breaks here.
    Space,
    /// A break opportunity *inside* a word (an author's soft hyphen or one the hyphenator
    /// found): nothing inside a line, but a visible `-` costing one cell when broken here.
    Hyphen,
}

/// TeX's `\linepenalty`: a flat cost per line, so that of two equally loose breakings the
/// one using fewer lines wins.
const LINE_PENALTY: i64 = 10;

/// TeX's `\hyphenpenalty`: breaking inside a word is a little worse than breaking between
/// words, all else equal. Low enough that hyphenation is still freely used — its whole
/// purpose is to give the breaker somewhere to take slack from.
const HYPHEN_PENALTY: i64 = 50;

/// TeX's `\doublehyphendemerits`: two hyphenated lines in a row read badly, so the second
/// one is charged heavily. Not forbidden — a narrow column sometimes has no alternative.
const DOUBLE_HYPHEN_DEMERITS: i64 = 3000;

/// The badness of a line that cannot be set at all (TeX's "infinitely bad"). Used for a
/// line with no gaps to stretch but slack to absorb — a lone word marooned on its own row.
const INF_BADNESS: i64 = 10_000;

/// Charged to a line wider than the column. Only reachable for a single atom that simply
/// does not fit (a URL, a long identifier), where the alternative is not setting the
/// paragraph at all; the size keeps the breaker from ever preferring one.
const OVERFULL_DEMERITS: i64 = 1_000_000;

/// One line's badness from how far its gaps must stretch.
///
/// `r` is the adjustment ratio — extra cells per gap — and badness grows with its cube, so
/// tolerance falls away sharply: at one extra cell per gap a line is unremarkable (100), at
/// two it is poor (800), at three it is the worst thing that still counts as set (2700).
/// The cube is what makes the total, once squared into demerits, prefer several nearly-tight
/// lines over one tight line and one gaping one.
fn badness(slack: usize, gaps: usize, last_line: bool) -> i64 {
    // The paragraph's final line is filled out by the (infinitely stretchable) end glue, so
    // being short is free — that is what lets a paragraph end mid-column.
    if last_line || slack == 0 {
        return 0;
    }
    if gaps == 0 {
        return INF_BADNESS; // nothing to stretch: the slack has nowhere to go
    }
    let r = slack as f64 / gaps as f64;
    ((100.0 * r * r * r).round() as i64).min(INF_BADNESS)
}

/// What one line costs the paragraph.
///
/// Badness is *squared* — that is the whole mechanism. Two lines at badness 100 cost 2·110²
/// = 24 200, while one perfect line and one at badness 200 cost 100 + 210² = 44 200. So the
/// total always prefers spreading the difficulty around, which is exactly what the greedy
/// filler cannot do: it commits to each line before it has seen the next.
fn line_demerits(b: i64, hyphenated: bool, after_hyphen: bool) -> i64 {
    let mut d = (LINE_PENALTY + b).pow(2);
    if hyphenated {
        d += HYPHEN_PENALTY * HYPHEN_PENALTY;
        if after_hyphen {
            d += DOUBLE_HYPHEN_DEMERITS;
        }
    }
    d
}

/// The width, in cells, a line of `atoms[i..j]` occupies when set at its natural spacing,
/// plus how many stretchable gaps it contains.
struct Line {
    natural: usize,
    gaps: usize,
    /// The line breaks inside a word, so it shows a trailing hyphen.
    hyphenated: bool,
}

/// Break `widths` into lines, minimising total demerits.
///
/// `joins[k]` describes what sits between atom `k` and atom `k + 1`, so `joins` is one
/// shorter than `widths`. `first_avail` is the usable width of the first line and
/// `cont_avail` that of every later one (they differ by the block's prefix — an indent, a
/// quote bar, a list marker's hanging padding). `max_stretch` is the most a gap may widen,
/// in extra cells, and doubles as the tolerance: a line needing more is not a candidate.
///
/// Returns the index each line *ends* at, exclusive — `[3, 7]` over 7 atoms means
/// `atoms[0..3]` then `atoms[3..7]`. `None` when the paragraph admits no feasible breaking
/// at all, which is the caller's signal to fall back to greedy filling rather than to give
/// up: that happens whenever a stretch of the text simply cannot be set within the column.
pub(super) fn break_paragraph(
    widths: &[usize],
    joins: &[Join],
    first_avail: usize,
    cont_avail: usize,
    max_stretch: usize,
) -> Option<Vec<usize>> {
    let n = widths.len();
    if n == 0 {
        return Some(Vec::new());
    }
    debug_assert_eq!(joins.len(), n - 1, "one join between each pair of atoms");
    let tolerance = badness(max_stretch, 1, false);

    // best[i] = cheapest total demerits for a paragraph whose next line starts at atom i;
    // from[i] = the atom the line ending at i started from, for reconstruction.
    let mut best = vec![i64::MAX; n + 1];
    let mut from = vec![0usize; n + 1];
    best[0] = 0;

    for i in 0..n {
        if best[i] == i64::MAX {
            continue; // unreachable start
        }
        // Only the very first line carries the block's first-line prefix; a line starting
        // anywhere else is a continuation. So the available width is fixed by where the
        // line starts, which is why the search state needs nothing but that position.
        let avail = if i == 0 { first_avail } else { cont_avail };
        // Whether the *previous* line ended on a hyphen is likewise decided by the join we
        // broke at to get here — again a function of the start position alone.
        let after_hyphen = i > 0 && joins[i - 1] == Join::Hyphen;

        let mut content = 0usize;
        let mut gaps = 0usize;
        for j in (i + 1)..=n {
            content += widths[j - 1];
            // The join that pulled atom j-1 into this line: a space costs a cell, a
            // mid-word break costs nothing (the segments simply run together).
            if j - 1 > i && joins[j - 2] == Join::Space {
                gaps += 1;
            }
            let hyphenated = j < n && joins[j - 1] == Join::Hyphen;
            let line = Line {
                natural: content + gaps + usize::from(hyphenated),
                gaps,
                hyphenated,
            };

            if line.natural > avail {
                // Lines only grow from here, so this is the end of the search from `i`.
                // A single atom too wide for the column still has to go somewhere: allow
                // it to overflow alone rather than fail the whole paragraph.
                if j == i + 1 {
                    relax(&mut best, &mut from, i, j, OVERFULL_DEMERITS);
                }
                break;
            }

            let slack = avail - line.natural;
            let b = badness(slack, line.gaps, j == n);
            if b > tolerance {
                continue; // too loose — a longer line has less slack, so keep going
            }
            let d = line_demerits(b, line.hyphenated, after_hyphen);
            relax(&mut best, &mut from, i, j, d);
        }
    }

    if best[n] == i64::MAX {
        return None; // no feasible breaking — the caller falls back to greedy
    }
    let mut ends = Vec::new();
    let mut at = n;
    while at > 0 {
        ends.push(at);
        at = from[at];
    }
    ends.reverse();
    Some(ends)
}

/// Record a line `i..j` costing `d` demerits if it is the cheapest route to `j` so far.
fn relax(best: &mut [i64], from: &mut [usize], i: usize, j: usize, d: i64) {
    let total = best[i].saturating_add(d);
    if total < best[j] {
        best[j] = total;
        from[j] = i;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOLERANCE: usize = 3;

    /// Atom widths for a list of word lengths, all separated by spaces.
    fn words(lens: &[usize]) -> (Vec<usize>, Vec<Join>) {
        (
            lens.to_vec(),
            vec![Join::Space; lens.len().saturating_sub(1)],
        )
    }

    /// The greedy fill this module exists to beat: take atoms until the next one would
    /// overrun, then break. Mirrors `spans::fill_lines` for a paragraph with no over-wide
    /// token, and is the baseline the optimality test measures against.
    fn greedy_ends(widths: &[usize], joins: &[Join], first: usize, cont: usize) -> Vec<usize> {
        let n = widths.len();
        let (mut ends, mut i) = (Vec::new(), 0usize);
        while i < n {
            let avail = if i == 0 { first } else { cont };
            let (mut content, mut gaps, mut take) = (0usize, 0usize, i);
            for j in (i + 1)..=n {
                let add_gap = j - 1 > i && joins[j - 2] == Join::Space;
                let hyph = j < n && joins[j - 1] == Join::Hyphen;
                let natural =
                    content + widths[j - 1] + gaps + usize::from(add_gap) + usize::from(hyph);
                if natural > avail && j > i + 1 {
                    break;
                }
                content += widths[j - 1];
                if add_gap {
                    gaps += 1;
                }
                take = j;
            }
            ends.push(take);
            i = take;
        }
        ends
    }

    /// Score a breaking the same way the algorithm does, so a candidate can be compared
    /// against the optimum. `None` when the breaking contains a line that cannot be set.
    fn score(
        widths: &[usize],
        joins: &[Join],
        ends: &[usize],
        first: usize,
        cont: usize,
    ) -> Option<i64> {
        let n = widths.len();
        let tolerance = badness(TOLERANCE, 1, false);
        let (mut total, mut i) = (0i64, 0usize);
        for &j in ends {
            let avail = if i == 0 { first } else { cont };
            let content: usize = widths[i..j].iter().sum();
            let gaps = (i..j.saturating_sub(1))
                .filter(|&k| joins[k] == Join::Space)
                .count();
            let hyphenated = j < n && joins[j - 1] == Join::Hyphen;
            let natural = content + gaps + usize::from(hyphenated);
            if natural > avail {
                return None; // overfull
            }
            let b = badness(avail - natural, gaps, j == n);
            if b > tolerance {
                return None; // unsettable
            }
            let after_hyphen = i > 0 && joins[i - 1] == Join::Hyphen;
            total += line_demerits(b, hyphenated, after_hyphen);
            i = j;
        }
        Some(total)
    }

    /// Every atom placed exactly once, in order — a breaking that drops or duplicates text
    /// would be a silent corruption, so this is checked on every result below.
    fn assert_covers(ends: &[usize], n: usize) {
        assert_eq!(ends.last().copied().unwrap_or(0), n, "reaches the end");
        assert!(ends.windows(2).all(|w| w[0] < w[1]), "strictly increasing");
    }

    #[test]
    fn a_paragraph_that_fits_on_one_line_is_not_broken() {
        let (w, j) = words(&[5, 4, 5]);
        assert_eq!(break_paragraph(&w, &j, 40, 40, TOLERANCE).unwrap(), vec![3]);
    }

    #[test]
    fn an_empty_paragraph_breaks_into_no_lines() {
        assert_eq!(
            break_paragraph(&[], &[], 10, 10, TOLERANCE).unwrap(),
            Vec::<usize>::new()
        );
    }

    /// The claim this module makes: over the same feasible set, the result is never worse
    /// than the greedy fill — and on paragraphs with an awkward long word, strictly better.
    #[test]
    fn it_is_never_worse_than_the_greedy_fill() {
        // Realistic prose measures. A narrow column has so few feasible breakings that
        // greedy is often already optimal; the interesting cases are at reading width.
        let paragraphs: [&[usize]; 5] = [
            &[5, 3, 7, 4, 9, 2, 6, 5, 8, 3, 4, 7, 5, 6, 4, 9, 3, 5, 7, 4],
            &[4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4],
            &[12, 3, 4, 11, 5, 3, 14, 4, 6, 3, 10, 5, 4, 8, 3, 7, 5, 4],
            &[2, 9, 3, 15, 4, 2, 8, 6, 3, 11, 4, 5, 9, 2, 7, 4, 6, 3, 8],
            &[6, 6, 6, 6, 18, 6, 6, 6, 6, 6, 17, 6, 6, 6, 6, 6, 6],
        ];
        for (n, lens) in paragraphs.iter().enumerate() {
            let (w, j) = words(lens);
            for width in [40usize, 56, 72] {
                let Some(kp) = break_paragraph(&w, &j, width, width, TOLERANCE) else {
                    continue; // no feasible breaking; the caller falls back to greedy
                };
                assert_covers(&kp, w.len());
                let kp_score = score(&w, &j, &kp, width, width)
                    .expect("the optimum is by construction settable");
                if let Some(greedy) =
                    score(&w, &j, &greedy_ends(&w, &j, width, width), width, width)
                {
                    assert!(
                        kp_score <= greedy,
                        "paragraph {n} at width {width}: optimal {kp_score} worse than greedy {greedy}"
                    );
                }
            }
        }
    }

    /// The concrete win, hand-checked. Greedy packs the first line and strands a lone word
    /// on the second, which cannot be justified at all; giving one word back sets both.
    #[test]
    fn it_gives_a_word_back_rather_than_strand_one() {
        // Width 21. Greedy: "aaaa bbbb cccc dddd" = 19 (fits), leaving "eeee" alone on an
        // inner line with no gaps to absorb its slack. Breaking a word earlier lets both
        // inner lines carry gaps.
        let (w, j) = words(&[4, 4, 4, 4, 4, 4]);
        let kp = break_paragraph(&w, &j, 21, 21, TOLERANCE).expect("sets");
        assert_covers(&kp, 6);
        let greedy = greedy_ends(&w, &j, 21, 21);
        assert_eq!(greedy, vec![4, 6], "greedy packs four then two");
        assert!(
            score(&w, &j, &kp, 21, 21).unwrap() <= score(&w, &j, &greedy, 21, 21).unwrap(),
            "optimal is at least as cheap: {kp:?} vs {greedy:?}"
        );
    }

    /// Segments of one word run together inside a line, but a break between them shows a
    /// hyphen that has to be paid for — miscounting it overruns the column by one.
    #[test]
    fn a_mid_word_break_pays_for_its_hyphen() {
        // Two 3-cell halves of one word. Joined inside a line they are 6 cells, so they
        // fit a 6-cell column whole.
        let w = [3usize, 3];
        assert_eq!(
            break_paragraph(&w, &[Join::Hyphen], 6, 6, TOLERANCE).unwrap(),
            vec![2],
            "no space between segments of one word"
        );
        // Split across lines the first half needs its hyphen too: 3 + 1 > 3.
        let ends = break_paragraph(&w, &[Join::Hyphen], 3, 3, TOLERANCE).unwrap();
        assert_covers(&ends, 2);
    }

    /// The last line is finished by the paragraph's end glue, so it may be as short as it
    /// likes — otherwise no paragraph could end mid-column.
    #[test]
    fn the_last_line_is_free_to_be_short() {
        let (w, j) = words(&[6, 6, 3]);
        let ends = break_paragraph(&w, &j, 13, 13, TOLERANCE).expect("sets");
        assert_eq!(ends, vec![2, 3], "ends on a short line rather than gaping");
    }

    /// A continuation prefix (an indent, a quote bar, a list marker's hanging padding)
    /// narrows every line after the first, and each line must be measured against its own.
    #[test]
    fn continuation_lines_use_their_own_width() {
        let w = [4usize, 4];
        assert_eq!(
            break_paragraph(&w, &[Join::Hyphen], 8, 4, TOLERANCE).unwrap(),
            vec![2],
            "both halves fit the wide first line"
        );
    }

    /// A paragraph containing something that cannot be set — a token wider than the column,
    /// or a word marooned with slack it has no gaps to absorb — reports failure rather than
    /// inventing a bad breaking. The caller falls back to the greedy fill, which always
    /// produces *something*.
    #[test]
    fn an_unsettable_paragraph_defers_to_the_caller() {
        let (w, j) = words(&[4, 30, 4]);
        assert!(
            break_paragraph(&w, &j, 10, 10, TOLERANCE).is_none(),
            "hands off rather than set an unsettable paragraph"
        );
    }
}
