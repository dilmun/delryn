//! Pure text heuristics for titles, authors, ISBNs, and filename templating.
//! No I/O — used by seeding, rename, and metadata extraction alike.

/// The filename-friendly main title: the title with a trailing subtitle removed.
///
/// EPUBs (even legitimate publisher ones) often bake the subtitle into `dc:title`
/// as "Main Title: The Subtitle" *and* store it separately in a subtitle field.
/// For filenames we want just the main title. Two signals, most reliable first:
///
/// 1. If the metadata `subtitle` is known and the title ends with it after a
///    conventional divider (`": "`, `" — "`, `" – "`, `" - "`, `"; "`), drop
///    exactly that trailing run (compared case-insensitively).
/// 2. Otherwise, if the title carries a `": "` divider, keep the part before it.
///
/// The title is returned unchanged when neither applies (e.g. a clean title whose
/// subtitle only ever lived in the separate field).
pub fn filename_title(title: &str, subtitle: &str) -> String {
    let title = title.trim();
    let subtitle = subtitle.trim();
    // Longest dividers first so " — " is preferred over a bare "-".
    const SEPS: [&str; 5] = [": ", " — ", " – ", " - ", "; "];

    if !subtitle.is_empty() {
        for sep in SEPS {
            if let Some(head) = strip_trailing(title, sep, subtitle) {
                return head;
            }
        }
    }
    // Fall back to the text before the first ": " divider (requires the space so
    // "Re:Zero" and "C++:foo" aren't split mid-token).
    if let Some((main, _)) = title.split_once(": ") {
        let main = main.trim();
        if !main.is_empty() {
            return main.to_string();
        }
    }
    title.to_string()
}

/// Does a title look like an opaque ID rather than a real title — a bare number
/// ("503392068") or a UUID-ish hex/dash string? Such "titles" are useless for
/// display and search, so the filename is a better source.
pub fn looks_like_id(title: &str) -> bool {
    let t = title.trim();
    if t.is_empty() {
        return true;
    }
    // All digits (ignoring spaces).
    if t.chars().all(|c| c.is_ascii_digit() || c.is_whitespace()) {
        return true;
    }
    // UUID-ish: only hex digits and dashes, with a dash and a long hex run.
    let hex = t.chars().filter(|c| c.is_ascii_hexdigit()).count();
    t.chars().all(|c| c.is_ascii_hexdigit() || c == '-') && t.contains('-') && hex >= 8
}

/// Reduce a raw title (from metadata *or* a filename) to a clean main title for
/// searching: cut at the first subtitle/edition divider, turn underscores/dots
/// into spaces, and collapse whitespace. Dividers are colons, semicolons,
/// brackets, slashes/pipes, and *spaced* dashes (` - `) — so hyphenated words
/// ("Well-Grounded") and programming chars ("C++") are preserved.
pub fn main_title(raw: &str) -> String {
    let t = raw.trim();
    let mut cut = t.len();
    // The first single-char divider (cut anywhere it appears).
    for (i, c) in t.char_indices() {
        if matches!(c, ':' | ';' | '/' | '\\' | '|' | '(' | '[' | '{') {
            cut = i;
            break;
        }
    }
    // …or an earlier spaced dash separating title from subtitle/author.
    for pat in [" - ", " – ", " — "] {
        if let Some(i) = t.find(pat) {
            cut = cut.min(i);
        }
    }
    let head: String = t[..cut]
        .chars()
        .map(|c| if c == '_' || c == '.' { ' ' } else { c })
        .collect();
    head.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first author from a possibly multi-author string — a single author is far
/// cleaner for a metadata search than the full byline. Splits on the usual
/// separators (`,`, `;`, ` and `, ` & `) and keeps the first *non-empty* name, so
/// a malformed byline like ", Kissinger" yields "Kissinger", not leading junk.
/// Placeholder bylines (`Unknown`, `Anonymous`, …) become empty.
pub fn first_author(authors: &str) -> String {
    if is_placeholder_author(authors) {
        return String::new();
    }
    let mut a = authors.trim();
    for sep in [",", ";", " and ", " & "] {
        if let Some(piece) = a.split(sep).map(str::trim).find(|p| !p.is_empty()) {
            a = piece;
        }
    }
    let first = a.trim_matches(|c: char| matches!(c, ',' | ';' | '&') || c.is_whitespace());
    if is_placeholder_author(first) {
        String::new()
    } else {
        first.to_string()
    }
}

/// A non-author placeholder the indexer or a converter leaves behind.
fn is_placeholder_author(s: &str) -> bool {
    matches!(
        s.trim().to_lowercase().as_str(),
        "" | "unknown" | "unknown author" | "anonymous" | "n/a" | "na" | "none"
    )
}

/// If `title` ends with `<sep><subtitle>` (the subtitle matched case-insensitively),
/// return its leading main-title part, trimmed; else `None`.
fn strip_trailing(title: &str, sep: &str, subtitle: &str) -> Option<String> {
    let chars: Vec<char> = title.chars().collect();
    let suffix_len = sep.chars().count() + subtitle.chars().count();
    if chars.len() <= suffix_len {
        return None;
    }
    let head_len = chars.len() - suffix_len;
    let tail: String = chars[head_len..].iter().collect();
    if tail.eq_ignore_ascii_case(&format!("{sep}{subtitle}")) {
        let head: String = chars[..head_len].iter().collect();
        let head = head.trim();
        if !head.is_empty() {
            return Some(head.to_string());
        }
    }
    None
}

/// Fill a rename template from metadata `values` (Title, Author, Year, Series,
/// Series index, Publisher — index order) and the file `ext`. Placeholders:
/// `%T %A %Y %S %I %P %E`; `%%` → `%`. The result is sanitized as a filename.
pub fn fill_template(template: &str, values: &[String], ext: &str) -> String {
    let get = |i: usize| values.get(i).map(String::as_str).unwrap_or("").trim();
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('T') => out.push_str(get(0)),
            Some('A') => out.push_str(get(1)),
            Some('Y') => out.push_str(get(2)),
            Some('S') => out.push_str(get(3)),
            Some('I') => out.push_str(get(4)),
            Some('P') => out.push_str(get(5)),
            Some('E') => out.push_str(ext),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    sanitize_filename(&out)
}

/// Make a string safe as a single filename: drop path separators and characters
/// that misbehave across filesystems, collapse whitespace, trim.
pub fn sanitize_filename(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        let mapped = match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => Some(' '),
            c if c.is_control() => Some(' '),
            c => Some(c),
        };
        if let Some(m) = mapped {
            if m == ' ' {
                if !last_space {
                    out.push(' ');
                }
                last_space = true;
            } else {
                out.push(m);
                last_space = false;
            }
        }
    }
    out.trim().to_string()
}

/// Extract a clean ISBN-10/13 from a messy `dc:identifier` (which may be a UUID,
/// calibre id, ASIN, or an ISBN wrapped in `isbn:` / `urn:isbn:` with hyphens).
/// Returns `None` when it isn't a plausible ISBN.
pub fn normalize_isbn(raw: &str) -> Option<String> {
    if raw.to_lowercase().contains("asin") {
        return None; // Amazon id, not an ISBN
    }
    let digits: String = raw
        .chars()
        .filter(|c| c.is_ascii_digit() || c.eq_ignore_ascii_case(&'X'))
        .collect();
    match digits.len() {
        13 if digits.starts_with("978") || digits.starts_with("979") => Some(digits),
        10 => Some(digits.to_uppercase()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_template_fills_and_sanitizes() {
        let v: Vec<String> = ["Dune", "Frank Herbert", "1965", "Dune", "1", "Ace"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(fill_template("%T.%E", &v, "epub"), "Dune.epub");
        assert_eq!(
            fill_template("%A - %T (%Y).%E", &v, "epub"),
            "Frank Herbert - Dune (1965).epub"
        );
        assert_eq!(
            fill_template("%S %I - %T.%E", &v, "epub"),
            "Dune 1 - Dune.epub"
        );
        assert_eq!(fill_template("100%% %T.%E", &v, "epub"), "100% Dune.epub");
        let bad: Vec<String> = ["A/B:C", "", "", "", "", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(fill_template("%T.%E", &bad, "epub"), "A B C.epub");
    }

    #[test]
    fn filename_title_strips_subtitle() {
        assert_eq!(
            filename_title("Main Title: The Subtitle", "The Subtitle"),
            "Main Title"
        );
        assert_eq!(
            filename_title("Main Title - The Subtitle", "The Subtitle"),
            "Main Title"
        );
        assert_eq!(
            filename_title("Main Title — The Subtitle", "The Subtitle"),
            "Main Title"
        );
        assert_eq!(filename_title("Main: SUBTITLE", "subtitle"), "Main");
        assert_eq!(
            filename_title("Applied NLP: Implementing ML", ""),
            "Applied NLP"
        );
        assert_eq!(
            filename_title(
                "Applied NLP with Python: Implementing ML",
                "A Different Subtitle Wording"
            ),
            "Applied NLP with Python"
        );
        assert_eq!(
            filename_title(
                "Applied Natural Language Processing with Python",
                "Implementing ML and DL"
            ),
            "Applied Natural Language Processing with Python"
        );
        assert_eq!(filename_title("Plain Title", ""), "Plain Title");
        assert_eq!(
            filename_title("Re:Zero Starting Life", ""),
            "Re:Zero Starting Life"
        );
    }

    #[test]
    fn first_author_and_main_title() {
        assert_eq!(first_author("A. Author, B. Other; C"), "A. Author");
        assert_eq!(first_author("Jane Doe and John Roe"), "Jane Doe");
        assert_eq!(first_author("Solo Writer"), "Solo Writer");
        assert_eq!(first_author(", Kissinger"), "Kissinger");
        assert_eq!(first_author(" , Smith , Jones"), "Smith");
        assert_eq!(first_author("Unknown"), "");
        assert_eq!(first_author("Unknown Author"), "");
        assert_eq!(first_author("anonymous"), "");
        assert_eq!(first_author("Real Name, Other"), "Real Name");

        assert_eq!(
            main_title("Deep Learning With Python : A Crash Course"),
            "Deep Learning With Python"
        );
        assert_eq!(main_title("Some Book - Author - 2020"), "Some Book");
        assert_eq!(main_title("some_book_title.v2"), "some book title v2");
        assert_eq!(main_title("Title (1st ed)"), "Title");
        assert_eq!(main_title("C++ Primer"), "C++ Primer");
        assert_eq!(main_title("Well-Grounded Rubyist"), "Well-Grounded Rubyist");
        assert_eq!(main_title("503392068"), "503392068");
    }

    #[test]
    fn looks_like_id_flags_junk() {
        assert!(looks_like_id("503392068"));
        assert!(looks_like_id("3a2629db-9413-44cf-a547-0b7791b3d987"));
        assert!(looks_like_id("  "));
        assert!(!looks_like_id("Deep Learning"));
        assert!(!looks_like_id("Catch-22"));
    }

    #[test]
    fn normalize_isbn_cleans_identifiers() {
        assert_eq!(
            normalize_isbn("isbn:9789819753338").as_deref(),
            Some("9789819753338")
        );
        assert_eq!(
            normalize_isbn("urn:isbn:978-3-031-61037-0").as_deref(),
            Some("9783031610370")
        );
        assert_eq!(
            normalize_isbn("9781492094524").as_deref(),
            Some("9781492094524")
        );
        assert_eq!(normalize_isbn("0441013597").as_deref(), Some("0441013597"));
        assert_eq!(normalize_isbn("5cdd9eaf-3ede-43dc-9509-845585947b3d"), None);
        assert_eq!(normalize_isbn("calibre:255"), None);
        assert_eq!(normalize_isbn("urn:asin:B0CRR19Z5D"), None);
    }
}
