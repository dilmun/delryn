//! Heuristic book-metadata extraction from EPUB *content* (title page,
//! headings, ISBN/year/publisher/author-bio scans) — for converted files whose
//! OPF metadata is junk. Best-effort; the user reviews before saving.

use std::path::Path;

use epub::doc::EpubDoc;

use super::EXTRACT_WIDTH;

/// Most leaf blocks a section can have and still be treated as a title page
/// rather than a chapter (so chapter body text is never mistaken for a title).
const TITLE_PAGE_MAX_BLOCKS: usize = 14;

/// All metadata delryn can recover from the book's *own content* — for converted
/// files whose OPF metadata is junk and that aren't findable online. Every field
/// is best-effort and meant to be reviewed before saving.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ExtractedMeta {
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub author: Option<String>,
    pub year: Option<i32>,
    pub publisher: Option<String>,
    pub isbn: Option<String>,
}

/// Scan the book's front matter for as much metadata as possible: the title page
/// gives title/subtitle/author; the copyright/imprint page gives year, publisher
/// and ISBN. Reads only the first few sections.
pub fn extract_book_metadata(path: impl AsRef<Path>) -> ExtractedMeta {
    let mut out = ExtractedMeta::default();
    let Ok(mut doc) = EpubDoc::new(path.as_ref()) else {
        return out;
    };
    let n = doc.get_num_chapters().min(8);
    for i in 0..n {
        if !doc.set_current_chapter(i) {
            continue;
        }
        let Some((xhtml, _)) = doc.get_current_str() else {
            continue;
        };
        if out.title.is_none() {
            let page = parse_title_page(&xhtml);
            if let Some((t, s, a)) = page {
                out.title = Some(t);
                out.subtitle = s;
                out.author = a;
            }
        }
        if out.year.is_none()
            || out.publisher.is_none()
            || out.isbn.is_none()
            || out.author.is_none()
        {
            let text = html2text::from_read(xhtml.as_bytes(), EXTRACT_WIDTH).unwrap_or_default();
            out.isbn = out.isbn.or_else(|| find_isbn(&text));
            out.year = out.year.or_else(|| find_year(&text));
            out.publisher = out.publisher.or_else(|| find_publisher(&text));
            // Many books name the author only in an "About the Author" page.
            out.author = out.author.or_else(|| find_author_bio(&text));
        }
        if out.title.is_some() && out.author.is_some() && out.isbn.is_some() && out.year.is_some() {
            break;
        }
    }
    out
}

/// Leaf block-element lines of a section in document order — a block with no
/// block descendant, so a wrapping `<div>` doesn't swallow every line into one.
/// A `<br/>` *inside* a block also starts a new line, so a title and subtitle
/// packed into one heading (`Title<br/><i>Subtitle</i>`) come out separately.
fn leaf_block_lines(xhtml: &str) -> Vec<String> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(xhtml);
    let Ok(sel) = Selector::parse("h1,h2,h3,h4,h5,h6,p,div,li") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for el in doc.select(&sel) {
        if el.select(&sel).next().is_some() {
            continue; // not a leaf block
        }
        for raw in block_text_lines(el) {
            let line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
            if !line.is_empty() {
                out.push(line);
            }
        }
    }
    out
}

/// An element's text, with each `<br>` starting a new line.
fn block_text_lines(el: scraper::ElementRef) -> Vec<String> {
    crate::container::descendant_text(*el, true, None)
        .split('\n')
        .map(str::to_string)
        .collect()
}

/// Parse a title-page-like section into (title, subtitle, author). A title page
/// keeps each in a *separate* line — headings, `<br/>`-split text, or plain
/// `<div>`/`<p>` lines in converted files. The first real line is the title and
/// the next a (distinct, non-boilerplate) subtitle; the title block ends at a
/// separator (`—`, `by`) or boilerplate (copyright/ISBN/…), and the first real
/// line after a separator is the author.
fn parse_title_page(xhtml: &str) -> Option<(String, Option<String>, Option<String>)> {
    let lines = leaf_block_lines(xhtml);
    if lines.is_empty() {
        return None;
    }
    // A title page is a short section, or one whose title block is quickly
    // followed by copyright-style boilerplate (a chapter has neither).
    let boiler_near_top = lines.iter().take(12).any(|l| is_boilerplate(l));
    if lines.len() > TITLE_PAGE_MAX_BLOCKS && !boiler_near_top {
        return None;
    }
    // Title block = leading lines before the first separator or boilerplate.
    let end = lines
        .iter()
        .position(|l| is_separator_line(l) || is_boilerplate(l))
        .unwrap_or(lines.len())
        .min(TITLE_PAGE_MAX_BLOCKS);
    let head = &lines[..end];
    let ti = head.iter().position(|l| is_title_candidate(l))?;
    let title = head[ti].clone();
    // Subtitle: the next real line that isn't just the title repeated.
    let subtitle = head
        .iter()
        .skip(ti + 1)
        .find(|l| is_title_candidate(l) && !title_overlaps(l, &title))
        .cloned();
    let author = author_after_separator(&lines);
    Some((title, subtitle, author))
}

/// The first real, non-boilerplate line after a `—`/`by` separator — the author.
fn author_after_separator(lines: &[String]) -> Option<String> {
    let sep = lines.iter().position(|l| is_separator_line(l))?;
    lines[sep + 1..]
        .iter()
        .find(|l| is_title_candidate(l) && !is_boilerplate(l))
        .cloned()
}

/// True when two lines are the same title (or one contains the other) — used to
/// reject a "subtitle" that merely repeats the title.
fn title_overlaps(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    a == b || a.contains(&b) || b.contains(&a)
}

/// Copyright / imprint boilerplate that must never be taken as a title, subtitle,
/// or author.
fn is_boilerplate(line: &str) -> bool {
    let l = line.trim().to_lowercase();
    l.starts_with("copyright")
        || l.starts_with('©')
        || l.contains("all rights reserved")
        || l.contains("no part of this")
        || l.starts_with("isbn")
        || l.contains("first published")
        || l.contains("printed in")
        || l.starts_with("published by")
        || l.contains("library of congress")
}

/// Pull a title (+ optional subtitle) out of one XHTML section: the title page if
/// there is one, else a non-generic `<title>` element.
pub(crate) fn title_from_html(xhtml: &str) -> Option<(String, Option<String>)> {
    use scraper::{Html, Selector};
    if let Some((title, subtitle, _)) = parse_title_page(xhtml) {
        return Some((title, subtitle));
    }
    let doc = Html::parse_document(xhtml);
    let head_title = doc
        .select(&Selector::parse("title").ok()?)
        .map(|e| {
            e.text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .find(|s| is_title_candidate(s))?;
    Some((head_title, None))
}

/// First labelled ISBN in `text`, normalized to a bare ISBN-10/13. Tolerates the
/// usual clutter between the label and the number (`ISBN-13 (pbk): 978-…`).
fn find_isbn(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?i)ISBN.{0,30}?([0-9][0-9\- ]{8,18}[0-9Xx])").ok()?;
    re.captures_iter(text)
        .find_map(|c| delryn_model::naming::normalize_isbn(&c[1]))
}

/// The author named in an "About the Author" section, e.g. "Shekhar Khandelwal
/// is a distinguished…" → "Shekhar Khandelwal". Takes the words after the heading
/// up to a biographical verb (is/was/works/holds/…), 1–4 words.
fn find_author_bio(text: &str) -> Option<String> {
    let re = regex::Regex::new(
        r"(?i)about the authors?[\s:]+([^\n,]{2,40}?)\s+(?:is|was|are|has|have|holds|works|serves|received|earned|currently)\b",
    )
    .ok()?;
    let name = re.captures(text)?[1].trim().to_string();
    let words = name.split_whitespace().count();
    (1..=4).contains(&words).then_some(name)
}

/// A publication year near a copyright / "published" marker.
fn find_year(text: &str) -> Option<i32> {
    let re = regex::Regex::new(
        r"(?i)(?:copyright|©|\(c\)|first published|published)\D{0,24}((?:19|20)\d{2})",
    )
    .ok()?;
    re.captures(text).and_then(|c| c[1].parse().ok())
}

/// The publisher named on the imprint page. A well-known publisher mentioned in
/// the front matter wins (most reliable); else an explicit "Published by X" line
/// — but only when it reads like a name (Title-Case, no sentence verbs), so the
/// boilerplate "Neither the publisher … can accept responsibility" is ignored.
fn find_publisher(text: &str) -> Option<String> {
    const PUBS: [&str; 12] = [
        "Apress",
        "O'Reilly Media",
        "O'Reilly",
        "Springer",
        "Packt Publishing",
        "Packt",
        "Manning",
        "No Starch Press",
        "BPB",
        "Wiley",
        "Pearson",
        "Addison-Wesley",
    ];
    let low = text.to_lowercase();
    if let Some(p) = PUBS.iter().find(|p| low.contains(&p.to_lowercase())) {
        return Some(p.to_string());
    }
    // "Published by X", or "Copyright © 2024 by X" — the name after "by".
    for pat in [
        r"(?i)published by\s+([^\n\r.,;]{2,50})",
        r"(?i)(?:copyright|©)[^\n]*?\bby\s+([^\n\r.,;]{2,50})",
    ] {
        let Ok(re) = regex::Regex::new(pat) else {
            continue;
        };
        let Some(cap) = re.captures(text) else {
            continue;
        };
        let p = cap[1].trim().to_string();
        // Reject sentence fragments (a publisher name has no sentence words).
        let looks_like_name = !p.is_empty()
            && p.split_whitespace().count() <= 6
            && !p.split_whitespace().any(|w| {
                matches!(
                    w.to_lowercase().as_str(),
                    "can" | "the" | "and" | "any" | "for" | "nor" | "of"
                )
            });
        if looks_like_name {
            return Some(p);
        }
    }
    None
}

/// A line that divides the title block from the author (`—`, `-`, `·`, `by`).
fn is_separator_line(l: &str) -> bool {
    let t = l.trim();
    t.eq_ignore_ascii_case("by") || (!t.is_empty() && t.chars().all(|c| !c.is_alphanumeric()))
}

/// Is `t` a plausible book title rather than an opaque ID or generic front-matter
/// label (Cover / Contents / Copyright …)?
fn is_title_candidate(t: &str) -> bool {
    let t = t.trim();
    // Needs a few real letters — rejects bare numbers / IDs.
    if t.chars().filter(|c| c.is_alphabetic()).count() < 3 {
        return false;
    }
    const GENERIC: [&str; 14] = [
        "cover",
        "title page",
        "title",
        "contents",
        "table of contents",
        "copyright",
        "copyright page",
        "dedication",
        "acknowledgments",
        "acknowledgements",
        "index",
        "preface",
        "foreword",
        "about the author",
    ];
    let low = t.to_lowercase();
    !GENERIC.contains(&low.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_metadata_scanners() {
        // ISBN: tolerate label clutter, normalize to bare digits.
        assert_eq!(
            find_isbn("ISBN-13 (pbk): 978-1-4842-4096-0"),
            Some("9781484240960".into())
        );
        assert_eq!(
            find_isbn("eISBN: 9789355515391"),
            Some("9789355515391".into())
        );
        assert_eq!(find_isbn("no number here"), None);
        // Year: only near a copyright / publication marker.
        assert_eq!(find_year("Copyright © 2019 by Sumit Raj"), Some(2019));
        assert_eq!(find_year("First published 2024 by BPB"), Some(2024));
        assert_eq!(find_year("see you in 2030 maybe"), None);
        // Publisher: known names win; sentence boilerplate is rejected.
        assert_eq!(
            find_publisher("© 2019 Apress Media LLC"),
            Some("Apress".into())
        );
        assert_eq!(
            find_publisher("Published by Acme Press, London"),
            Some("Acme Press".into())
        );
        assert_eq!(
            find_publisher("Copyright © 2024 by HiTeX Press"),
            Some("HiTeX Press".into())
        );
        assert_eq!(
            find_publisher("Neither the publisher nor the author can accept responsibility"),
            None
        );
        // Author from an "About the Author" bio.
        assert_eq!(
            find_author_bio(
                "About the Author\n\nShekhar Khandelwal is a distinguished AI Scientist."
            ),
            Some("Shekhar Khandelwal".into())
        );
        assert_eq!(
            find_author_bio("About the author: Jane Roe works at Acme."),
            Some("Jane Roe".into())
        );
        assert_eq!(find_author_bio("no author section here"), None);
    }

    #[test]
    fn content_title_from_headings() {
        // h1 title + h2 subtitle.
        let html = "<html><body><h1>Building Chatbots with Python</h1>\
            <h2>Using NLP and Machine Learning</h2></body></html>";
        assert_eq!(
            title_from_html(html),
            Some((
                "Building Chatbots with Python".into(),
                Some("Using NLP and Machine Learning".into())
            ))
        );

        // Calibre-style title page: plain <div> lines, title and subtitle split
        // into separate blocks, an em-dash separator before the author. Only the
        // title + subtitle are captured — never the author.
        let calibre = "<html><body><div class=\"c1\">\
            <div class=\"c2\">Building Chatbots with Python</div>\
            <div class=\"c2\">Using Natural Language Processing and Machine Learning</div>\
            <div class=\"c2\">—</div>\
            <div class=\"c2\">Sumit Raj</div></div></body></html>";
        assert_eq!(
            title_from_html(calibre),
            Some((
                "Building Chatbots with Python".into(),
                Some("Using Natural Language Processing and Machine Learning".into())
            ))
        );

        // LaTeX-style: title + subtitle in one heading split by <br/>, with the
        // subtitle italicised and copyright following. Title/subtitle split; the
        // copyright is never taken as the subtitle.
        let latex = "<html><body><div class=\"maketitle\">\
            <h2>CUDA Programming with C++<br/><i>From Basics to Expert Proficiency</i></h2>\
            <div class=\"author\"></div></div>\
            <div class=\"center\"><p>Copyright © 2024 by HiTeX Press<br/>All rights reserved. \
            No part of this publication may be reproduced.</p></div></body></html>";
        assert_eq!(
            title_from_html(latex),
            Some((
                "CUDA Programming with C++".into(),
                Some("From Basics to Expert Proficiency".into())
            ))
        );

        // A leading generic block (Cover) is skipped.
        let html = "<html><body><h1>Cover</h1><h1>Deep Learning for Data Architects</h1>\
            <p>Unleash the power of Python</p></body></html>";
        assert_eq!(
            title_from_html(html),
            Some((
                "Deep Learning for Data Architects".into(),
                Some("Unleash the power of Python".into())
            ))
        );

        // No usable block → fall back to a non-generic <title>.
        let html =
            "<html><head><title>A Real Book Title</title></head><body><p>x</p></body></html>";
        assert_eq!(
            title_from_html(html),
            Some(("A Real Book Title".into(), None))
        );

        // An ID-ish heading is not a title.
        assert_eq!(
            title_from_html("<html><body><h1>503392068</h1></body></html>"),
            None
        );
        // A long section is a chapter, not a title page → ignored.
        let chapter = format!("<html><body>{}</body></html>", "<p>line</p>".repeat(20));
        assert_eq!(title_from_html(&chapter), None);
    }
}
