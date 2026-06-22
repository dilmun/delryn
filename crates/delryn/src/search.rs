//! In-book search matching: plain substring, regex, and fuzzy (subsequence)
//! modes. Kept separate from the reader/view so the matching logic is testable
//! and reused for both finding matches and highlighting them. See `DESIGN.md`.

use regex::{Regex, RegexBuilder};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Plain,
    Regex,
    Fuzzy,
}

impl SearchMode {
    pub fn label(self) -> &'static str {
        match self {
            SearchMode::Plain => "plain",
            SearchMode::Regex => "regex",
            SearchMode::Fuzzy => "fuzzy",
        }
    }

    pub fn next(self) -> SearchMode {
        match self {
            SearchMode::Plain => SearchMode::Regex,
            SearchMode::Regex => SearchMode::Fuzzy,
            SearchMode::Fuzzy => SearchMode::Plain,
        }
    }
}

/// A compiled query: tests lines for a match and reports highlight ranges.
/// All matching is case-insensitive.
pub struct Matcher {
    mode: SearchMode,
    /// Lowercased needle (for plain/fuzzy).
    needle: String,
    /// Compiled regex (Regex mode only); `None` if the pattern was invalid.
    regex: Option<Regex>,
}

impl Matcher {
    pub fn new(mode: SearchMode, query: &str) -> Matcher {
        let regex = if mode == SearchMode::Regex {
            RegexBuilder::new(query).case_insensitive(true).build().ok()
        } else {
            None
        };
        Matcher {
            mode,
            needle: query.to_lowercase(),
            regex,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }

    /// In regex mode, whether the pattern compiled.
    pub fn is_valid(&self) -> bool {
        self.mode != SearchMode::Regex || self.regex.is_some()
    }

    pub fn matches(&self, line: &str) -> bool {
        if self.needle.is_empty() {
            return false;
        }
        match self.mode {
            SearchMode::Plain => line.to_lowercase().contains(&self.needle),
            SearchMode::Regex => self.regex.as_ref().is_some_and(|r| r.is_match(line)),
            SearchMode::Fuzzy => is_subsequence(&self.needle, &line.to_lowercase()),
        }
    }

    /// Character ranges `[start, end)` to highlight within `line`.
    pub fn highlight_ranges(&self, line: &str) -> Vec<(usize, usize)> {
        if self.needle.is_empty() {
            return Vec::new();
        }
        match self.mode {
            SearchMode::Plain => plain_ranges(&line.to_lowercase(), &self.needle),
            SearchMode::Regex => self.regex.as_ref().map_or_else(Vec::new, |r| {
                // Map byte offsets to char indices.
                let idx = byte_to_char_index(line);
                r.find_iter(line)
                    .filter(|m| m.start() < m.end())
                    .map(|m| (idx(m.start()), idx(m.end())))
                    .collect()
            }),
            SearchMode::Fuzzy => subsequence_positions(&self.needle, &line.to_lowercase()),
        }
    }
}

/// Are all of `needle`'s chars present, in order, somewhere in `hay`?
fn is_subsequence(needle: &str, hay: &str) -> bool {
    let mut chars = hay.chars();
    needle.chars().all(|nc| chars.any(|hc| hc == nc))
}

/// The matched char positions of a greedy subsequence match, as 1-wide ranges.
fn subsequence_positions(needle: &str, hay: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut needle_chars = needle.chars().peekable();
    for (i, hc) in hay.chars().enumerate() {
        match needle_chars.peek() {
            Some(&nc) if nc == hc => {
                out.push((i, i + 1));
                needle_chars.next();
            }
            Some(_) => {}
            None => break,
        }
    }
    // Only highlight if the whole needle matched.
    if needle_chars.peek().is_some() {
        Vec::new()
    } else {
        out
    }
}

/// All occurrences of `needle` in `hay` (both lowercased), as char ranges.
fn plain_ranges(hay: &str, needle: &str) -> Vec<(usize, usize)> {
    let hay: Vec<char> = hay.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()] == needle[..] {
            out.push((i, i + needle.len()));
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// A closure mapping a byte offset in `line` to its char index.
fn byte_to_char_index(line: &str) -> impl Fn(usize) -> usize + '_ {
    move |byte| line[..byte.min(line.len())].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_is_case_insensitive() {
        let m = Matcher::new(SearchMode::Plain, "Cloud");
        assert!(m.matches("the cloud platform"));
        assert_eq!(m.highlight_ranges("a cloud here"), vec![(2, 7)]);
    }

    #[test]
    fn regex_matches_and_highlights() {
        let m = Matcher::new(SearchMode::Regex, "c\\w+d");
        assert!(m.is_valid());
        assert!(m.matches("a cloud b"));
        assert_eq!(m.highlight_ranges("a cloud b"), vec![(2, 7)]);

        let bad = Matcher::new(SearchMode::Regex, "c(");
        assert!(!bad.is_valid());
        assert!(!bad.matches("anything"));
    }

    #[test]
    fn fuzzy_subsequence() {
        let m = Matcher::new(SearchMode::Fuzzy, "cld");
        assert!(m.matches("cloud")); // c..l..d in order
        assert!(!m.matches("clamp")); // no d after l
        assert_eq!(m.highlight_ranges("cloud"), vec![(0, 1), (1, 2), (4, 5)]);
    }
}
