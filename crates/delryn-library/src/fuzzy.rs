//! A tiny subsequence fuzzy matcher for the command palette / pickers. Pure and
//! testable: scores higher for contiguous runs, word-boundary hits, and matches
//! near the start, so the best candidates float up.

/// Score `haystack` against the (case-insensitive) `needle`. `None` when not all
/// needle characters appear in order; higher is better. An empty needle matches
/// everything with a neutral score.
pub fn score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = haystack.chars().flat_map(char::to_lowercase).collect();
    let pat: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();

    let mut hi = 0;
    let mut total = 0i32;
    let mut prev_match: Option<usize> = None;
    for &pc in &pat {
        // Advance through the haystack to the next occurrence of `pc`.
        let start = hi;
        while hi < hay.len() && hay[hi] != pc {
            hi += 1;
        }
        if hi >= hay.len() {
            return None; // a needle char isn't present in order
        }
        let mut s = 1; // base point for the match
        if hi > 0 && prev_match == Some(hi - 1) {
            s += 3; // contiguous with the previous match
        }
        if hi == 0 || matches!(hay.get(hi - 1), Some(' ' | '-' | '_' | '/' | ':')) {
            s += 2; // word-boundary start
        }
        s -= (hi - start) as i32; // penalise the gap we skipped
        total += s;
        prev_match = Some(hi);
        hi += 1;
    }
    // A small bonus for shorter haystacks (a tighter match overall).
    Some(total - (hay.len() as i32 / 16))
}

/// Filter + rank `items` by `needle`, best first. Stable for equal scores
/// (preserves input order). Returns the matching items (cloned).
pub fn rank<'a, T, F>(needle: &str, items: &'a [T], key: F) -> Vec<&'a T>
where
    F: Fn(&T) -> &str,
{
    let mut scored: Vec<(i32, usize, &T)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| score(needle, key(it)).map(|s| (s, i, it)))
        .collect();
    // Highest score first; ties keep original order.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, it)| it).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_subsequence_case_insensitively() {
        assert!(score("tt", "Title").is_some());
        assert!(score("ttl", "Title").is_some());
        assert!(score("xyz", "Title").is_none());
        assert!(score("", "anything").is_some());
    }

    #[test]
    fn contiguous_and_boundary_score_higher() {
        // "sort" contiguous beats scattered s..o..r..t.
        let contiguous = score("sort", "sort by title").unwrap();
        let scattered = score("sort", "switch other route tabs").unwrap();
        assert!(contiguous > scattered, "{contiguous} vs {scattered}");
        // A word-boundary match beats a mid-word one.
        let boundary = score("th", "by theme").unwrap();
        let midword = score("th", "breathe").unwrap();
        assert!(boundary > midword, "{boundary} vs {midword}");
    }

    #[test]
    fn rank_orders_best_first_and_filters() {
        let items = ["Sort by Author", "Toggle Sidebar", "Sort by Title"];
        let out = rank("sort", &items, |s| s);
        assert_eq!(out.len(), 2, "only the two 'Sort' commands");
        assert!(out.iter().all(|s| s.contains("Sort")));
    }

    #[test]
    fn empty_needle_keeps_all_in_order() {
        let items = ["a", "b", "c"];
        let out = rank("", &items, |s| s);
        assert_eq!(out, vec![&"a", &"b", &"c"]);
    }
}
