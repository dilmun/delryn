//! User tags: free-form labels on a library book. Stored normalised (lowercased,
//! trimmed, deduplicated) and comma-separated, so matching is case-insensitive
//! and the displayed order is stable.

use std::collections::HashSet;

/// Split a stored/typed tag string into individual tags (trimmed, non-empty),
/// preserving order. Commas separate tags.
pub fn split(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Normalise a typed tag string for storage: lowercase, trim, drop empties and
/// duplicates (first occurrence wins), joined with ", ".
pub fn normalize(input: &str) -> String {
    let mut seen = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for raw in input.split(',') {
        let t = raw.trim().to_lowercase();
        if !t.is_empty() && seen.insert(t.clone()) {
            out.push(t);
        }
    }
    out.join(", ")
}

/// Whether `tags` (a stored tag string) has any tag containing `needle`
/// (case-insensitive substring) — the matcher behind the `tag:` query field.
pub fn matches(tags: &str, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return false;
    }
    split(tags).iter().any(|t| t.contains(&needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lowercases_trims_and_dedupes() {
        assert_eq!(
            normalize("  Fiction ,  sci-fi, FICTION "),
            "fiction, sci-fi"
        );
        assert_eq!(normalize(""), "");
        assert_eq!(normalize(" , ,"), "");
    }

    #[test]
    fn split_yields_individual_tags() {
        assert_eq!(split("a, b ,c"), vec!["a", "b", "c"]);
        assert!(split("").is_empty());
    }

    #[test]
    fn matches_is_case_insensitive_substring_per_tag() {
        assert!(matches("fiction, sci-fi", "fic"));
        assert!(matches("fiction, sci-fi", "SCI"));
        assert!(!matches("fiction", "xyz"));
        assert!(!matches("fiction", ""));
    }
}
