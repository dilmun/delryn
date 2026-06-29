//! Duplicate detection across formats. Each book contributes *several* match
//! keys — a canonical ISBN-13 (when present) **and** a normalized title+author —
//! and books are grouped by connected components, so any shared key links a
//! group. Matching on more than one key (rather than one rigid key with ISBN
//! precedence) is what lets a copy with no ISBN still join its ISBN-bearing twin,
//! and lets two editions whose ISBNs differ still meet on title+author.
//!
//! Everything here is pure, in-memory string work over already-loaded rows — no
//! I/O — so it stays cheap to run on demand for the whole library.

use std::collections::{HashMap, HashSet};

use delryn_model::naming::{canonical_isbn13, main_title};
use delryn_store::BookRow;

/// Groups of book indices that are duplicates of one another (each group has ≥2
/// members, members in ascending index order), ordered by first appearance.
pub fn duplicate_groups(books: &[BookRow]) -> Vec<Vec<usize>> {
    duplicate_groups_with_links(books, &[])
}

/// Like [`duplicate_groups`], but additionally treats each `(path_a, path_b)` in
/// `links` as a duplicate edge. These come from the thorough content scan (see
/// [`content_link_candidates`]) — discovered out-of-band and persisted — so books
/// with no shared metadata (e.g. a PDF and an EPUB) still group when their text
/// matches.
pub fn duplicate_groups_with_links(
    books: &[BookRow],
    links: &[(String, String)],
) -> Vec<Vec<usize>> {
    let mut uf = UnionFind::new(books.len());
    // First book index seen for each match key; subsequent books sharing the key
    // are unioned into the same component.
    let mut first: HashMap<String, usize> = HashMap::new();
    for (i, b) in books.iter().enumerate() {
        for key in match_keys(b) {
            match first.entry(key) {
                std::collections::hash_map::Entry::Occupied(e) => uf.union(*e.get(), i),
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(i);
                }
            }
        }
    }
    // Content-scan (or other out-of-band) links, resolved by path. Paths no longer
    // in the library are silently skipped.
    if !links.is_empty() {
        let idx: HashMap<&str, usize> = books
            .iter()
            .enumerate()
            .map(|(i, b)| (b.path.as_str(), i))
            .collect();
        for (a, b) in links {
            if let (Some(&ia), Some(&ib)) = (idx.get(a.as_str()), idx.get(b.as_str())) {
                uf.union(ia, ib);
            }
        }
    }
    let mut by_root: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..books.len() {
        by_root.entry(uf.find(i)).or_default().push(i); // pushed in ascending i
    }
    let mut groups: Vec<Vec<usize>> = by_root.into_values().filter(|g| g.len() > 1).collect();
    groups.sort_by_key(|g| g[0]); // stable document order (g[0] is the min index)
    groups
}

/// The set of paths that belong to some duplicate group.
pub fn duplicate_paths(books: &[BookRow]) -> HashSet<String> {
    duplicate_paths_excluding(books, &[], &HashSet::new())
}

/// Like [`duplicate_paths`], but folds in content-scan `links` and leaves out groups
/// the user has dismissed ("keep both") — whose signatures (see [`group_signature`])
/// are in `dismissed`.
pub fn duplicate_paths_excluding(
    books: &[BookRow],
    links: &[(String, String)],
    dismissed: &HashSet<String>,
) -> HashSet<String> {
    duplicate_groups_with_links(books, links)
        .into_iter()
        .filter(|g| !dismissed.contains(&group_signature(g, books)))
        .flatten()
        .map(|i| books[i].path.clone())
        .collect()
}

/// A book needs at least this many distinctive chapter labels to fingerprint on.
const TOC_MIN_LABELS: usize = 4;
/// …and a matching pair must share at least this many of them.
const TOC_MIN_SHARED: usize = 4;
/// Shared labels as a fraction of the shorter list (overlap coefficient, so a
/// finer-grained TOC on one side — sub-sections the other lacks — still matches).
const TOC_OVERLAP_MIN: f32 = 0.6;

/// A content identity read from a book's own pages (not its metadata): its
/// table-of-contents chapter labels.
pub struct ContentId {
    pub path: String,
    pub toc_labels: Vec<String>,
}

/// Candidate duplicate pairs from the thorough content scan. Each [`ContentId`] is
/// the chapter-title list read from a book's own table of contents — distinctive,
/// already-clean text (no page numbers, images, or symbols), and the same work
/// across formats. Generic structural labels ("Preface", "Index", "Chapter N", …)
/// are dropped; each remaining title is hashed into a set, and two books link when
/// their sets overlap by at least [`TOC_OVERLAP_MIN`] (and share ≥ [`TOC_MIN_SHARED`]
/// titles). Matching the chapter list *as a whole* means books that merely share a
/// topic don't collide. Returned as canonically-ordered `(a, b)` pairs with `a < b`.
///
/// O(n²) over the fingerprinted set — milliseconds at personal-library scale. A
/// *candidate* generator: the reader confirms in the overlay (and `n` keeps false
/// ones apart).
pub fn content_link_candidates(items: &[ContentId]) -> Vec<(String, String)> {
    let sets: Vec<HashSet<u64>> = items
        .iter()
        .map(|it| label_hashes(&it.toc_labels))
        .collect();
    let mut out = Vec::new();
    for i in 0..items.len() {
        if sets[i].len() < TOC_MIN_LABELS {
            continue;
        }
        for j in (i + 1)..items.len() {
            if sets[j].len() < TOC_MIN_LABELS {
                continue;
            }
            let shared = sets[i].intersection(&sets[j]).count();
            let coeff = shared as f32 / sets[i].len().min(sets[j].len()) as f32;
            if shared >= TOC_MIN_SHARED && coeff >= TOC_OVERLAP_MIN {
                let (a, b) = (&items[i].path, &items[j].path);
                if a < b {
                    out.push((a.clone(), b.clone()));
                } else {
                    out.push((b.clone(), a.clone()));
                }
            }
        }
    }
    out
}

/// Hash the *distinctive* chapter labels of a TOC into a set: each label's leading
/// "Chapter N" / "Part N" structural prefix is dropped, the rest reduced to bare
/// lowercase letters/digits, and generic boilerplate ("Preface", "Summary", …)
/// discarded. A set, so labels repeated across chapters (Packt's "Summary",
/// "Questions") collapse and don't inflate a match.
fn label_hashes(labels: &[String]) -> HashSet<u64> {
    labels.iter().filter_map(|l| distinctive_label(l)).collect()
}

/// The distinctive part of a TOC label, FNV-hashed — or `None` if the label is
/// purely structural ("Chapter 3", "Index", "Part II") with no real title.
fn distinctive_label(label: &str) -> Option<u64> {
    let lower = label.to_lowercase();
    let mut words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    // Drop a leading "chapter"/"part"/"section"/"appendix" and its number/numeral,
    // keeping any real title that follows ("Chapter 1: Event Loops" → "eventloops").
    while matches!(
        words.first(),
        Some(&("chapter" | "ch" | "part" | "section" | "appendix" | "unit" | "lesson"))
    ) {
        words.remove(0);
        if words
            .first()
            .is_some_and(|w| w.bytes().all(|b| b.is_ascii_digit()) || is_roman(w))
        {
            words.remove(0);
        }
    }
    let joined: String = words.concat();
    (joined.len() >= 4 && !is_generic_label(&joined)).then(|| fnv1a(joined.as_bytes()))
}

/// A lowercase token that's a Roman numeral (i, ii, iv, xii, …).
fn is_roman(w: &str) -> bool {
    !w.is_empty() && w.len() <= 7 && w.bytes().all(|b| b"ivxlcdm".contains(&b))
}

/// Whether a reduced (bare-letters) label is generic structural boilerplate rather
/// than a distinctive chapter title.
fn is_generic_label(norm: &str) -> bool {
    const GENERIC: &[&str] = &[
        "preface",
        "introduction",
        "contents",
        "tableofcontents",
        "index",
        "summary",
        "questions",
        "exercises",
        "furtherreading",
        "technicalrequirements",
        "glossary",
        "bibliography",
        "references",
        "foreword",
        "acknowledgments",
        "acknowledgements",
        "dedication",
        "abouttheauthor",
        "abouttheauthors",
        "aboutthereviewer",
        "aboutthereviewers",
        "conclusion",
        "notes",
        "prologue",
        "epilogue",
        "copyright",
        "title",
        "titlepage",
        "cover",
        "frontmatter",
        "backmatter",
        "contributors",
        "afterword",
    ];
    GENERIC.contains(&norm)
}

/// Jaccard-free FNV-1a hash of bytes (a stable per-label hash).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A stable identity for a duplicate group, independent of member order: the
/// sorted member paths joined by newlines. Used to remember groups the user has
/// dismissed. If the group's membership changes (a new copy appears), the
/// signature changes and the group resurfaces for review.
pub fn group_signature(group: &[usize], books: &[BookRow]) -> String {
    let mut paths: Vec<&str> = group.iter().map(|&i| books[i].path.as_str()).collect();
    paths.sort_unstable();
    paths.join("\n")
}

/// The exact-metadata match keys for a book — the cheap first tier, run on every
/// refresh. A canonical ISBN-13 key (when the ISBN is valid), and a normalized
/// title + author-surname key **per author** (when there's a title), so a copy
/// matches any twin that shares the ISBN, or the title and *any one* author. A
/// titled book with no usable author still emits a title-only key, so two
/// author-less copies of the same title match.
fn match_keys(b: &BookRow) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(isbn) = canonical_isbn13(&b.isbn) {
        keys.push(format!("isbn:{isbn}"));
    }
    let title = norm_title(&b.title);
    if !title.is_empty() {
        let surnames = author_surnames(&b.author);
        if surnames.is_empty() {
            keys.push(format!("ta:{title}|"));
        } else {
            for surname in surnames {
                keys.push(format!("ta:{title}|{surname}"));
            }
        }
    }
    keys
}

/// Normalized title for matching: subtitle stripped (via `main_title`), lowercased
/// and diacritics folded, punctuation removed, a leading article dropped, trailing
/// edition noise ("2nd ed", "revised edition") removed, whitespace collapsed.
/// Volume/part markers are deliberately *kept* so different entries in a series
/// don't collapse together.
fn norm_title(title: &str) -> String {
    let base = main_title(title).to_lowercase();
    let cleaned: String = base
        .chars()
        .map(fold_diacritic)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let mut words: Vec<&str> = cleaned.split_whitespace().collect();
    if matches!(words.first(), Some(&("the" | "a" | "an"))) && words.len() > 1 {
        words.remove(0);
    }
    strip_edition_noise(&mut words);
    words.join(" ")
}

/// Every author's normalized surname, so a multi-author book matches a copy that
/// lists the same work with *any one* author in common (and in any order). The
/// byline is split on the usual multi-author separators (`&`, `;`, ` and `); each
/// name is reduced to its surname by [`surname_of`].
fn author_surnames(author: &str) -> Vec<String> {
    author
        .split(['&', ';'])
        .flat_map(|s| s.split(" and "))
        .map(surname_of)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Normalized surname of a single author (diacritics folded, lowercased
/// alphanumerics), so "Frank Herbert" and "Herbert, Frank" match — and "Müller"
/// matches "Muller".
fn surname_of(name: &str) -> String {
    let name = name.trim();
    let surname = if let Some((last, _)) = name.split_once(',') {
        last.trim() // "Herbert, Frank" → "Herbert"
    } else {
        name.split_whitespace().last().unwrap_or(name) // "Frank Herbert" → "Herbert"
    };
    surname
        .to_lowercase()
        .chars()
        .map(fold_diacritic)
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

/// Fold a (lowercased) Latin letter with a diacritic to its base ASCII letter, so
/// accented spellings bucket together. Matching-only — not for display — so a
/// pragmatic table of common Latin-1 / Latin-Extended-A letters is enough.
fn fold_diacritic(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'č' | 'ċ' => 'c',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => 'i',
        'ñ' | 'ń' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => 'o',
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => 'u',
        'ý' | 'ÿ' => 'y',
        'ß' | 'š' | 'ś' | 'ş' => 's',
        'ž' | 'ź' | 'ż' => 'z',
        'ł' => 'l',
        'đ' | 'ð' => 'd',
        'þ' => 't',
        'æ' => 'a',
        _ => c,
    }
}

/// Edition/printing qualifiers that don't distinguish content, stripped only from
/// the *end* of a title (where editions live). An ordinal directly preceding an
/// "edition"/"ed" is dropped with it ("2nd ed" → gone).
const EDITION_WORDS: &[&str] = &[
    "edition",
    "ed",
    "revised",
    "annotated",
    "illustrated",
    "unabridged",
    "reprint",
    "anniversary",
    "deluxe",
];

fn strip_edition_noise(words: &mut Vec<&str>) {
    while let Some(&last) = words.last() {
        if !EDITION_WORDS.contains(&last) {
            break;
        }
        let edition_marker = matches!(last, "edition" | "ed");
        words.pop();
        if edition_marker && words.last().is_some_and(|w| is_ordinal(w)) {
            words.pop();
        }
    }
}

/// An ordinal that qualifies an edition ("2nd", "second", or a bare number) — used
/// only to drop the count in front of a stripped "edition"/"ed".
fn is_ordinal(w: &str) -> bool {
    matches!(
        w,
        "1st"
            | "2nd"
            | "3rd"
            | "4th"
            | "5th"
            | "6th"
            | "7th"
            | "8th"
            | "9th"
            | "10th"
            | "first"
            | "second"
            | "third"
            | "fourth"
            | "fifth"
            | "sixth"
            | "seventh"
            | "eighth"
            | "ninth"
            | "tenth"
    ) || w.chars().all(|c| c.is_ascii_digit())
}

/// Disjoint-set forest (union by rank, path compression) for grouping books that
/// share any match key into connected components.
struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }

    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut cur = x;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        match self.rank[ra].cmp(&self.rank[rb]) {
            std::cmp::Ordering::Less => self.parent[ra] = rb,
            std::cmp::Ordering::Greater => self.parent[rb] = ra,
            std::cmp::Ordering::Equal => {
                self.parent[rb] = ra;
                self.rank[ra] += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(path: &str, title: &str, author: &str, isbn: &str) -> BookRow {
        BookRow {
            path: path.into(),
            title: title.into(),
            author: author.into(),
            year: None,
            size: 0,
            favorite: false,
            pct: 0,
            series: String::new(),
            series_index: None,
            publisher: String::new(),
            subtitle: String::new(),
            isbn: isbn.into(),
            language: String::new(),
            converted: false,
            rating: 0,
            status: String::new(),
            tags: String::new(),
        }
    }

    #[test]
    fn matches_across_format_by_normalized_title_author() {
        let books = vec![
            book("/a/Dune.epub", "Dune", "Frank Herbert", ""),
            book("/b/dune.epub", "DUNE: A Novel", "Herbert, Frank", ""),
            book("/c/Other.epub", "Something Else", "Someone", ""),
        ];
        let groups = duplicate_groups(&books);
        assert_eq!(groups.len(), 1, "one duplicate group");
        assert_eq!(groups[0], vec![0, 1]);
        let paths = duplicate_paths(&books);
        assert!(paths.contains("/a/Dune.epub") && paths.contains("/b/dune.epub"));
        assert!(!paths.contains("/c/Other.epub"));
    }

    #[test]
    fn matches_when_any_one_author_is_shared() {
        // Multi-author book vs. a copy crediting one of them (different order/format).
        // Sharing a single author is enough; a same-title book with no shared author
        // is not grouped.
        let books = vec![
            book(
                "/a.epub",
                "Deep Learning",
                "Ian Goodfellow & Yoshua Bengio & Aaron Courville",
                "",
            ),
            book("/b.pdf", "Deep Learning", "Bengio, Yoshua", ""),
            book("/c.epub", "Deep Learning", "Someone Else", ""),
        ];
        let groups = duplicate_groups(&books);
        assert_eq!(groups.len(), 1, "shared author groups a+b, c stays out");
        assert_eq!(groups[0], vec![0, 1]);
    }

    #[test]
    fn isbn_takes_precedence_over_title() {
        // Same ISBN, different displayed title → still duplicates.
        let books = vec![
            book("/a.epub", "The Art of War", "Sun Tzu", "978-0-14-045991-9"),
            book(
                "/b.epub",
                "Art of War (Annotated)",
                "Tzu, Sun",
                "9780140459919",
            ),
        ];
        assert_eq!(duplicate_groups(&books).len(), 1);
    }

    #[test]
    fn leading_article_and_subtitle_ignored() {
        let books = vec![
            book("/a.epub", "The Rust Programming Language", "Klabnik", ""),
            book(
                "/b.epub",
                "Rust Programming Language: 2nd Ed",
                "Klabnik",
                "",
            ),
        ];
        assert_eq!(duplicate_groups(&books).len(), 1);
    }

    #[test]
    fn distinct_books_are_not_grouped() {
        let books = vec![
            book("/a.epub", "Book One", "Author A", ""),
            book("/b.epub", "Book Two", "Author B", ""),
        ];
        assert!(duplicate_groups(&books).is_empty());
        assert!(duplicate_paths(&books).is_empty());
    }

    #[test]
    fn asymmetric_isbn_still_matches_on_title_author() {
        // The classic cross-format miss: the EPUB carries an ISBN, the converted
        // copy carries none. They must still group on title+author.
        let books = vec![
            book("/dune.epub", "Dune", "Frank Herbert", "9780441013593"),
            book("/dune.pdf", "Dune", "Frank Herbert", ""),
        ];
        assert_eq!(duplicate_groups(&books).len(), 1);
    }

    #[test]
    fn isbn10_and_isbn13_of_same_book_match() {
        let books = vec![
            book("/a.epub", "Whatever Title", "Someone", "0441013597"),
            book(
                "/b.epub",
                "Totally Different Display",
                "Else",
                "9780441013593",
            ),
        ];
        // 0441013597 (ISBN-10) canonicalizes to 9780441013593 (ISBN-13).
        assert_eq!(duplicate_groups(&books).len(), 1, "ISBN-10/13 collapse");
    }

    #[test]
    fn different_isbns_still_meet_on_title_author() {
        // Two editions with different ISBNs (so the ISBN keys differ) still group
        // because the title+author key is shared — the old ISBN-precedence model
        // missed this.
        let books = vec![
            book("/a.epub", "Clean Code", "Robert Martin", "9780132350884"),
            book("/b.epub", "Clean Code", "Martin, Robert", "9780136083238"),
        ];
        assert_eq!(duplicate_groups(&books).len(), 1);
    }

    #[test]
    fn diacritics_are_folded() {
        let books = vec![
            book("/a.epub", "Café Society", "Müller", ""),
            book("/b.epub", "Cafe Society", "Muller", ""),
        ];
        assert_eq!(duplicate_groups(&books).len(), 1);
    }

    #[test]
    fn trailing_edition_noise_ignored() {
        let books = vec![
            book("/a.epub", "The C Programming Language", "Kernighan", ""),
            book(
                "/b.epub",
                "C Programming Language, 2nd Edition",
                "Kernighan",
                "",
            ),
        ];
        assert_eq!(duplicate_groups(&books).len(), 1);
    }

    #[test]
    fn series_volumes_are_not_merged() {
        // Volume markers must survive normalization so distinct series entries
        // don't collapse into one false duplicate.
        let books = vec![
            book("/a.epub", "Mistborn Volume 1", "Sanderson", ""),
            book("/b.epub", "Mistborn Volume 2", "Sanderson", ""),
        ];
        assert!(duplicate_groups(&books).is_empty());
    }

    #[test]
    fn group_signature_is_order_independent() {
        let books = vec![
            book("/z.epub", "Dune", "Herbert", ""),
            book("/a.epub", "Dune", "Herbert", ""),
        ];
        let sig_a = group_signature(&[0, 1], &books);
        let sig_b = group_signature(&[1, 0], &books);
        assert_eq!(sig_a, sig_b);
        assert_eq!(sig_a, "/a.epub\n/z.epub");
    }

    #[test]
    fn dismissed_groups_are_excluded() {
        let books = vec![
            book("/a.epub", "Dune", "Herbert", ""),
            book("/b.epub", "Dune", "Herbert", ""),
        ];
        let groups = duplicate_groups(&books);
        let sig = group_signature(&groups[0], &books);
        let dismissed: HashSet<String> = [sig].into_iter().collect();
        assert!(duplicate_paths_excluding(&books, &[], &dismissed).is_empty());
        assert_eq!(
            duplicate_paths(&books).len(),
            2,
            "still flagged without the dismissal"
        );
    }

    #[test]
    fn out_of_band_links_group_books_with_no_shared_metadata() {
        // Two files with nothing in common metadata-wise (the PDF case) — only a
        // content-scan link ties them together.
        let books = vec![
            book("/scan.pdf", "9912_ocr", "", ""),
            book("/clean.epub", "The Pragmatic Programmer", "Hunt", ""),
        ];
        assert!(duplicate_groups(&books).is_empty(), "no metadata overlap");
        let links = vec![("/scan.pdf".to_string(), "/clean.epub".to_string())];
        let groups = duplicate_groups_with_links(&books, &links);
        assert_eq!(groups.len(), 1, "the content link groups them");
        assert_eq!(groups[0], vec![0, 1]);
    }

    #[test]
    fn stale_links_are_ignored() {
        let books = vec![book("/a.epub", "One", "A", "")];
        // A link referencing a path no longer in the library must not panic.
        let links = vec![("/a.epub".to_string(), "/gone.pdf".to_string())];
        assert!(duplicate_groups_with_links(&books, &links).is_empty());
    }

    fn toc(path: &str, labels: &[&str]) -> ContentId {
        ContentId {
            path: path.into(),
            toc_labels: labels.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn content_links_match_books_with_the_same_chapter_list() {
        // Same book: the PDF prefixes chapters with "Chapter N:" and adds a
        // sub-section the EPUB lacks; the distinctive titles still match (prefix
        // stripped, the extra sub-section only widens one side). The unrelated third
        // book shares no chapter titles.
        let epub = toc(
            "/a.epub",
            &[
                "Preface",
                "Exploring Async Patterns",
                "Building Event Loops",
                "Coroutines in Depth",
                "Lock-Free Queues",
                "Index",
            ],
        );
        let pdf = toc(
            "/b.pdf",
            &[
                "Chapter 1: Exploring Async Patterns",
                "Chapter 2: Building Event Loops",
                "Chapter 3: Coroutines in Depth",
                "Chapter 4: Lock-Free Queues",
                "Summary",
            ],
        );
        let other = toc(
            "/c.epub",
            &[
                "Financial Time Series",
                "Risk Models",
                "Portdelryn Optimization",
                "Backtesting Strategies",
                "Market Microstructure",
            ],
        );
        assert_eq!(
            content_link_candidates(&[epub, pdf, other]),
            vec![("/a.epub".to_string(), "/b.pdf".to_string())]
        );
    }

    #[test]
    fn content_links_separate_books_with_different_chapter_lists() {
        let a = toc(
            "/a",
            &[
                "Neural Networks",
                "Backpropagation",
                "Convolutional Layers",
                "Attention and Transformers",
            ],
        );
        let b = toc(
            "/b",
            &[
                "Hash Tables",
                "Balanced Trees",
                "Graph Algorithms",
                "Dynamic Programming",
            ],
        );
        assert!(content_link_candidates(&[a, b]).is_empty());
    }

    #[test]
    fn content_links_skip_generic_only_tocs() {
        // Nothing but structural labels → no distinctive fingerprint → no match,
        // even though the two lists are identical.
        let a = toc(
            "/a",
            &[
                "Preface",
                "Chapter 1",
                "Chapter 2",
                "Chapter 3",
                "Index",
                "Summary",
            ],
        );
        let b = toc(
            "/b",
            &[
                "Preface",
                "Chapter 1",
                "Chapter 2",
                "Chapter 3",
                "Index",
                "Summary",
            ],
        );
        assert!(content_link_candidates(&[a, b]).is_empty());
    }

    #[test]
    fn distinctive_label_strips_structure_and_drops_boilerplate() {
        assert_eq!(
            distinctive_label("Chapter 12: Lock-Free Queues"),
            distinctive_label("Lock-Free Queues")
        );
        assert_eq!(distinctive_label("Part IV"), None);
        assert_eq!(distinctive_label("Summary"), None);
        assert!(distinctive_label("Exploring Async Patterns").is_some());
    }
}
