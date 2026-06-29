//! Duplicate detection by **content**. Books are grouped solely by table-of-contents
//! matches found by the thorough scan (`R`) — each match is a `(path, path)` link,
//! and connected components of those links are the duplicate groups. The TOC is the
//! one reliable cross-format signal (see [`content_link_candidates`]); messy
//! metadata (ISBN/title/author) is deliberately *not* used. A book with no usable
//! TOC simply isn't flagged. Pure, in-memory union-find over the links.

use std::collections::{HashMap, HashSet};

use delryn_store::BookRow;

/// Groups of book indices that are duplicates of one another. With no links this is
/// always empty — matches come only from the content scan, via
/// [`duplicate_groups_with_links`].
pub fn duplicate_groups(books: &[BookRow]) -> Vec<Vec<usize>> {
    duplicate_groups_with_links(books, &[])
}

/// Group books into duplicate sets from the content-scan `links`: each
/// `(path_a, path_b)` is an edge, and each connected component of ≥2 books is a
/// group (members in ascending index order; groups in document order). Paths no
/// longer in the library are silently skipped.
pub fn duplicate_groups_with_links(
    books: &[BookRow],
    links: &[(String, String)],
) -> Vec<Vec<usize>> {
    let mut uf = UnionFind::new(books.len());
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
    let raw: Vec<HashSet<u64>> = items
        .iter()
        .map(|it| label_hashes(&it.toc_labels))
        .collect();
    // Document frequency: how many books carry each label. Publisher front matter
    // ("Who this book is for", "Why subscribe?") and generic section names
    // ("Implementation", "Summary", "Functions") recur across *many* books and
    // would otherwise make any two same-publisher books share ≥4 "distinctive"
    // labels. Drop the frequent ones so matching keys only on labels that are
    // actually peculiar to a work (its real chapter titles).
    let mut df: HashMap<u64, usize> = HashMap::new();
    for set in &raw {
        for &h in set {
            *df.entry(h).or_default() += 1;
        }
    }
    let with_toc = raw.iter().filter(|s| !s.is_empty()).count();
    let max_df = (with_toc / 12).max(4);
    let sets: Vec<HashSet<u64>> = raw
        .iter()
        .map(|s| s.iter().copied().filter(|h| df[h] <= max_df).collect())
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
            book("/b.pdf", "Dune", "", ""),
        ];
        let links = vec![("/a.epub".to_string(), "/b.pdf".to_string())];
        let groups = duplicate_groups_with_links(&books, &links);
        let sig = group_signature(&groups[0], &books);
        let dismissed: HashSet<String> = [sig].into_iter().collect();
        assert!(duplicate_paths_excluding(&books, &links, &dismissed).is_empty());
        assert_eq!(
            duplicate_paths_excluding(&books, &links, &HashSet::new()).len(),
            2,
            "still flagged without the dismissal"
        );
    }

    #[test]
    fn links_group_books_otherwise_unmatched() {
        // Only a content-scan link ties these together — no metadata is used.
        let books = vec![
            book("/scan.pdf", "9912_ocr", "", ""),
            book("/clean.epub", "The Pragmatic Programmer", "Hunt", ""),
        ];
        assert!(duplicate_groups(&books).is_empty(), "nothing without links");
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
    fn content_links_ignore_shared_publisher_boilerplate() {
        // Front matter every same-publisher book repeats (Packt-style). It survives
        // the per-label stoplist, but document-frequency filtering drops it, so only
        // the two books that also share real chapter titles are linked.
        let boiler = [
            "Who this book is for",
            "What this book covers",
            "Conventions used",
            "Why subscribe",
            "Errata",
            "Piracy",
        ];
        let cid = |path: &str, chapters: &[&str]| {
            let mut v: Vec<String> = boiler.iter().map(|s| s.to_string()).collect();
            v.extend(chapters.iter().map(|s| s.to_string()));
            ContentId {
                path: path.into(),
                toc_labels: v,
            }
        };
        let items = vec![
            cid(
                "/dup1.epub",
                &[
                    "Reactive Stream Topologies",
                    "Lock Free Ring Buffers",
                    "Coroutine Schedulers",
                    "Zero Copy Serialization",
                ],
            ),
            cid(
                "/dup2.pdf",
                &[
                    "Reactive Stream Topologies",
                    "Lock Free Ring Buffers",
                    "Coroutine Schedulers",
                    "Zero Copy Serialization",
                ],
            ),
            cid(
                "/other1.epub",
                &[
                    "Bayesian Priors",
                    "Gibbs Sampling",
                    "Variational Inference",
                    "Hamiltonian Monte Carlo",
                ],
            ),
            cid(
                "/other2.epub",
                &[
                    "Sourdough Hydration",
                    "Lamination Folds",
                    "Crumb Structure",
                    "Oven Spring",
                ],
            ),
            cid(
                "/other3.epub",
                &[
                    "Roman Aqueducts",
                    "Gothic Arches",
                    "Baroque Facades",
                    "Brutalist Forms",
                ],
            ),
            cid(
                "/other4.epub",
                &[
                    "Tax Loss Harvesting",
                    "Dividend Capture",
                    "Options Greeks",
                    "Yield Curves",
                ],
            ),
        ];
        assert_eq!(
            content_link_candidates(&items),
            vec![("/dup1.epub".to_string(), "/dup2.pdf".to_string())],
            "shared boilerplate must not link different books"
        );
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
