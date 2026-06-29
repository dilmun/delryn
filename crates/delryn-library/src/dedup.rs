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
    // Cover-scan (or other out-of-band) links, resolved by path. Paths no longer
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

/// Char-shingle width for the content SimHash.
const SIMHASH_SHINGLE: usize = 12;
/// Cap on canonical (alphanumeric) characters fed to the SimHash — plenty for a
/// stable fingerprint while bounding per-book work.
const SIMHASH_MAX_CHARS: usize = 20_000;
/// Below this many canonical characters there's too little text to fingerprint
/// reliably (a stub chapter, or an image-only/scanned source with no text layer).
const SIMHASH_MIN_CHARS: usize = 600;
/// Content SimHashes within this many bits are treated as the same work. Tuned for
/// "close, not identical": the same book across formats lands a few bits apart
/// (conversion noise — hyphenation, headers, ligatures), unrelated text is ~32.
pub const CONTENT_HAMMING_MAX: u32 = 12;

/// A 64-bit **SimHash** of a text sample, for near-duplicate detection. The text is
/// reduced to bare lowercase alphanumerics — markup, spaces, punctuation, and case
/// all dropped — so two formats of the same book canonicalize to nearly the same
/// string; that string is shingled into overlapping `SIMHASH_SHINGLE`-char windows
/// whose hashes vote on each output bit. Two samples of the same content land a
/// small Hamming distance apart (see [`content_link_candidates`]); unrelated text
/// is ~32 bits apart. `None` when there's too little text to be reliable.
pub fn text_simhash(text: &str) -> Option<u64> {
    let canon: Vec<char> = text
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .take(SIMHASH_MAX_CHARS)
        .collect();
    if canon.len() < SIMHASH_MIN_CHARS {
        return None;
    }
    let mut votes = [0i32; 64];
    for window in canon.windows(SIMHASH_SHINGLE) {
        let h = shingle_hash(window);
        for (b, v) in votes.iter_mut().enumerate() {
            *v += if (h >> b) & 1 == 1 { 1 } else { -1 };
        }
    }
    let mut sim = 0u64;
    for (b, v) in votes.iter().enumerate() {
        if *v > 0 {
            sim |= 1 << b;
        }
    }
    Some(sim)
}

/// FNV-1a hash of a char shingle (folds each codepoint's bytes).
fn shingle_hash(chars: &[char]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &c in chars {
        let mut cp = c as u32;
        for _ in 0..4 {
            h ^= u64::from(cp & 0xff);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
            cp >>= 8;
        }
    }
    h
}

/// Candidate duplicate pairs from the thorough content scan: every pair whose text
/// SimHashes are within `max_distance` bits (see [`text_simhash`]). Each item is
/// `(path, simhash)`. Returned as canonically-ordered `(a, b)` pairs with `a < b`.
///
/// O(n²) over the fingerprinted set — a few million 64-bit pop-counts, milliseconds
/// at personal-library scale. Content is the whole signal here (no title/cover
/// gate), so it catches same-work files whatever their metadata says; the reader
/// confirms in the overlay (and `n` keeps false ones apart).
pub fn content_link_candidates(
    items: &[(String, u64)],
    max_distance: u32,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for i in 0..items.len() {
        for j in (i + 1)..items.len() {
            if (items[i].1 ^ items[j].1).count_ones() <= max_distance {
                let (a, b) = (&items[i].0, &items[j].0);
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

/// A stable identity for a duplicate group, independent of member order: the
/// sorted member paths joined by newlines. Used to remember groups the user has
/// dismissed. If the group's membership changes (a new copy appears), the
/// signature changes and the group resurfaces for review.
pub fn group_signature(group: &[usize], books: &[BookRow]) -> String {
    let mut paths: Vec<&str> = group.iter().map(|&i| books[i].path.as_str()).collect();
    paths.sort_unstable();
    paths.join("\n")
}

/// The match keys for a book: a canonical ISBN-13 key (when the ISBN is valid)
/// and a normalized title+author key (when there's a title). Both are emitted, so
/// a book links to twins that share *either*.
fn match_keys(b: &BookRow) -> Vec<String> {
    let mut keys = Vec::with_capacity(2);
    if let Some(isbn) = canonical_isbn13(&b.isbn) {
        keys.push(format!("isbn:{isbn}"));
    }
    let title = norm_title(&b.title);
    if !title.is_empty() {
        keys.push(format!("ta:{title}|{}", norm_author(&b.author)));
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

/// Normalized first-author surname (diacritics folded, lowercased alphanumerics),
/// so "Frank Herbert" and "Herbert, Frank" match — and "Müller" matches "Muller".
/// Falls back to the whole first author.
fn norm_author(author: &str) -> String {
    // First author from a list separated by `&`, `;`, or " and ".
    let first = author
        .split(['&', ';'])
        .next()
        .unwrap_or(author)
        .split(" and ")
        .next()
        .unwrap_or(author)
        .trim();
    let surname = if let Some((last, _)) = first.split_once(',') {
        last.trim() // "Herbert, Frank" → "Herbert"
    } else {
        first.split_whitespace().last().unwrap_or(first) // "Frank Herbert" → "Herbert"
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

    // A body-text paragraph repeated enough to exceed the fingerprint minimum.
    fn body(seed: &str) -> String {
        seed.repeat(40)
    }

    #[test]
    fn simhash_ignores_formatting_whitespace_and_case() {
        // Same words, but one copy is reformatted the way a different conversion
        // would render it (uppercased, repunctuated, rewrapped). The canonical
        // alphanumeric form is identical, so the fingerprints match closely.
        let a = body("the system reads each record then writes the merged result to disk. ");
        let b = a
            .to_uppercase()
            .replace(". ", ".\n\n    ")
            .replace("the", "the—");
        let (ha, hb) = (text_simhash(&a).unwrap(), text_simhash(&b).unwrap());
        assert!(
            (ha ^ hb).count_ones() <= CONTENT_HAMMING_MAX,
            "same content should be close, was {} bits",
            (ha ^ hb).count_ones()
        );
    }

    #[test]
    fn simhash_separates_different_content() {
        let a = text_simhash(&body(
            "the system reads each record then writes the result. ",
        ))
        .unwrap();
        let b = text_simhash(&body(
            "machine learning models need careful tuning of parameters. ",
        ))
        .unwrap();
        assert!(
            (a ^ b).count_ones() > CONTENT_HAMMING_MAX,
            "different books should be far, was {} bits",
            (a ^ b).count_ones()
        );
    }

    #[test]
    fn simhash_none_for_too_little_text() {
        assert!(text_simhash("only a few words here").is_none());
    }

    #[test]
    fn content_candidates_pair_near_simhashes_only() {
        let base = 0b1010_1100_1111_0000u64;
        let items = vec![
            ("/z.pdf".to_string(), base),
            ("/a.epub".to_string(), base ^ 0b11), // distance 2
            ("/x.epub".to_string(), !base),       // far
        ];
        let pairs = content_link_candidates(&items, 4);
        assert_eq!(pairs, vec![("/a.epub".to_string(), "/z.pdf".to_string())]);
    }
}
