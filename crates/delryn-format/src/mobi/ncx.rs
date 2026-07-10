//! MOBI navigation: turn the parsed NCX index into delryn's [`TocEntry`] tree and
//! flattened [`OutlineItem`] list, and provide the heading-based fallback used
//! when a book has no NCX.
//!
//! The NCX index (see [`super::index`]) gives, per entry: a label (CNCX string via
//! tag 3), a heading level (tag 4), and a target — a `filepos` byte offset (tag 1,
//! MOBI6) or a `pos_fid` fragment reference (tag 6, KF8). The caller supplies a
//! `resolve` closure mapping each entry's target to a `(section, locator)` pair;
//! this module owns the tree shape and the fallback.

use std::sync::LazyLock;

use regex::Regex;

use super::index::Index;
use super::{OutlineItem, TocEntry, decode_bytes};

/// Cap the outline nesting we materialise (a real TOC nests a few levels; this
/// bounds a pathological/adversarial index).
const MAX_DEPTH: usize = 32;

/// One NCX entry, decoded from the index tag map.
pub(super) struct NcxEntry {
    pub label: String,
    /// Heading level (tag 4); missing ⇒ 0.
    pub level: usize,
    /// MOBI6 filepos target (tag 1).
    pub pos: Option<u32>,
    /// KF8 `pos_fid` target (tag 6): `(fragment row, offset)`.
    pub pos_fid: Option<(u32, u32)>,
    /// First/last child entry index (tags 22/23) — the book's own TOC hierarchy.
    pub child1: Option<usize>,
    pub childn: Option<usize>,
}

/// NCX tag numbers (KindleUnpack `mobi_ncx.py::parseNCX`).
mod tag {
    pub const POS: u8 = 1;
    pub const NOFFS: u8 = 3; // CNCX offset of the label
    pub const HLVL: u8 = 4; // heading level
    pub const POS_FID: u8 = 6; // KF8 fragment (fid, off)
    pub const CHILD1: u8 = 22; // first child entry index
    pub const CHILDN: u8 = 23; // last child entry index
}

/// Decode the NCX index into entries, resolving each label from the CNCX strings.
pub(super) fn parse(index: &Index, encoding: u32) -> Vec<NcxEntry> {
    index
        .entries
        .iter()
        .map(|e| {
            let label = e
                .tag(tag::NOFFS)
                .and_then(|off| index.label(off))
                .map(|bytes| decode_bytes(bytes, encoding))
                .map(|s| collapse_ws(&s))
                .filter(|s| !s.is_empty())
                .unwrap_or_default();
            NcxEntry {
                label,
                level: e.tag(tag::HLVL).unwrap_or(0) as usize,
                pos: e.tag(tag::POS),
                pos_fid: match (e.tag_at(tag::POS_FID, 0), e.tag_at(tag::POS_FID, 1)) {
                    (Some(fid), Some(off)) => Some((fid, off)),
                    _ => None,
                },
                child1: e.tag(tag::CHILD1).map(|v| v as usize),
                childn: e.tag(tag::CHILDN).map(|v| v as usize),
            }
        })
        .collect()
}

/// A resolved entry ready to place in the tree/outline.
struct Placed {
    label: String,
    depth: usize,
    section: usize,
    locator: Option<String>,
}

/// An NCX entry resolved to its place in the book: the section it targets, the
/// final display label, and a locator the reader matches within that section.
pub(super) struct Resolved {
    pub section: usize,
    pub label: String,
    pub locator: Option<String>,
}

/// A pre-placement TOC item: which section it targets, its nesting level (NCX
/// heading level, or `<hN>` level for a heading scan), display label, and locator.
struct Item {
    section: usize,
    level: usize,
    label: String,
    locator: Option<String>,
}

/// Build the `(toc, outline)` from NCX entries. `resolve` maps an entry's target
/// to a [`Resolved`]; entries that fail to resolve, or resolve to a label with no
/// alphanumeric content (blank labels, lone ornaments), are dropped. Returns
/// `None` if nothing usable resolves (caller falls back to headings).
pub(super) fn build<F>(
    entries: &[NcxEntry],
    resolve: F,
) -> Option<(Vec<TocEntry>, Vec<OutlineItem>)>
where
    F: Fn(&NcxEntry) -> Option<Resolved>,
{
    // Resolve each entry, aligned with `entries` so the child pointers stay valid.
    // Entries with a blank/ornament-only label are dropped (kept as `None`).
    let resolved: Vec<Option<Resolved>> = entries
        .iter()
        .map(|e| resolve(e).filter(|r| r.label.chars().any(char::is_alphanumeric)))
        .collect();
    let placeable = resolved.iter().filter(|r| r.is_some()).count();
    if placeable == 0 {
        return None;
    }

    // When the NCX carries child pointers, honour its own tree shape; otherwise
    // (and if the tree ends up dropping entries) fall back to a level-stack over
    // document order.
    if entries.iter().any(|e| e.child1.is_some()) {
        let min_level = entries.iter().map(|e| e.level).min().unwrap_or(0);
        let mut ctx = TreeCtx {
            entries,
            resolved: &resolved,
            outline: Vec::new(),
            visited: vec![false; entries.len()],
        };
        let toc = ctx.recurs(min_level, 0, entries.len(), 0);
        // Trust the tree only if it placed (almost) every entry.
        if ctx.outline.len() * 10 >= placeable * 9 {
            return Some((toc, ctx.outline));
        }
    }

    let items = resolved
        .into_iter()
        .zip(entries)
        .filter_map(|(r, e)| {
            r.map(|r| Item {
                section: r.section,
                level: e.level,
                label: r.label,
                locator: r.locator,
            })
        })
        .collect();
    assemble(items)
}

/// Walks the NCX child-pointer tree (KindleUnpack `recursINDX`): at each level it
/// emits the entries whose heading level matches, then recurses into each entry's
/// `[child1, childn]` range for the next level.
struct TreeCtx<'a> {
    entries: &'a [NcxEntry],
    resolved: &'a [Option<Resolved>],
    outline: Vec<OutlineItem>,
    visited: Vec<bool>,
}

impl TreeCtx<'_> {
    fn recurs(&mut self, level: usize, start: usize, end: usize, depth: usize) -> Vec<TocEntry> {
        if depth >= MAX_DEPTH {
            return Vec::new();
        }
        let mut out = Vec::new();
        for i in start..end.min(self.entries.len()) {
            if self.visited[i] || self.entries[i].level != level {
                continue;
            }
            let Some(r) = &self.resolved[i] else { continue };
            let (label, section, locator) = (r.label.clone(), r.section, r.locator.clone());
            self.visited[i] = true;
            self.outline.push(OutlineItem {
                label: label.clone(),
                depth,
                section,
                locator,
            });
            let children = match (self.entries[i].child1, self.entries[i].childn) {
                (Some(c1), Some(cn)) if c1 > i && c1 <= cn => {
                    self.recurs(level + 1, c1, cn + 1, depth + 1)
                }
                _ => Vec::new(),
            };
            out.push(TocEntry {
                label,
                section: Some(section),
                children,
            });
        }
        out
    }
}

/// Build the `(toc, outline)` by scanning the sections for `<h1>`–`<h6>` headings.
/// This is the most reliable source for reflowable books — the headings are the
/// exact chapter titles the reader sees, in order, at heading granularity — and it
/// is robust to a mangled or missing NCX. `None` if no heading is found.
pub(super) fn heading_scan(sections: &[String]) -> Option<(Vec<TocEntry>, Vec<OutlineItem>)> {
    static HEADING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<h([1-6])[^>]*>(.*?)</h[1-6]>").unwrap());
    // An `<hgroup>` bundles a heading with its subtitle/title (e.g. Standard
    // Ebooks' `<h2>VII</h2><p>Rouen in February</p>`); the whole group is one TOC
    // title, so a numbered part reads "VII Rouen in February", not a bare "VII".
    static HGROUP: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<hgroup[^>]*>(.*?)</hgroup>").unwrap());
    let mut items = Vec::new();
    for (section, html) in sections.iter().enumerate() {
        // (byte position, level, label) so hgroup and standalone headings interleave
        // in document order.
        let mut found: Vec<(usize, usize, String)> = Vec::new();
        let mut hgroups: Vec<(usize, usize)> = Vec::new();
        for cap in HGROUP.captures_iter(html) {
            let whole = cap.get(0).unwrap();
            hgroups.push((whole.start(), whole.end()));
            let inner = cap.get(1).unwrap().as_str();
            let level = HEADING
                .captures(inner)
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse::<usize>().ok())
                .unwrap_or(2);
            let label = clean_heading(inner); // numeral + title combined
            if !label.is_empty() {
                found.push((whole.start(), level, label));
            }
        }
        for cap in HEADING.captures_iter(html) {
            let whole = cap.get(0).unwrap();
            // Skip a heading already covered by an enclosing hgroup.
            if hgroups
                .iter()
                .any(|&(s, e)| whole.start() >= s && whole.start() < e)
            {
                continue;
            }
            let level = cap
                .get(1)
                .and_then(|m| m.as_str().parse::<usize>().ok())
                .unwrap_or(1);
            let label = clean_heading(cap.get(2).unwrap().as_str());
            if !label.is_empty() {
                found.push((whole.start(), level, label));
            }
        }
        found.sort_by_key(|&(pos, _, _)| pos);
        for (_, level, label) in found {
            items.push(Item {
                section,
                level,
                locator: Some(label.clone()),
                label,
            });
        }
    }
    assemble(items)
}

/// Filter, depth-normalise, and assemble items into a `(toc tree, flat outline)`.
/// The depth sequence is normalised to start at 0 and never jump up by more than
/// one level, so the tree builder can trust it without validating child pointers.
fn assemble(items: Vec<Item>) -> Option<(Vec<TocEntry>, Vec<OutlineItem>)> {
    let mut placed: Vec<Placed> = Vec::new();
    let mut prev_depth = 0usize;
    let raw_min = items.iter().map(|i| i.level).min().unwrap_or(0);
    for it in items {
        if !it.label.chars().any(char::is_alphanumeric) {
            continue; // blank or ornament-only label — not a real TOC entry
        }
        let mut depth = it.level.saturating_sub(raw_min);
        if placed.is_empty() {
            depth = 0;
        } else if depth > prev_depth + 1 {
            depth = prev_depth + 1;
        }
        prev_depth = depth;
        placed.push(Placed {
            label: it.label,
            depth,
            section: it.section,
            locator: it.locator,
        });
    }
    if placed.is_empty() {
        return None;
    }

    let outline = placed
        .iter()
        .map(|p| OutlineItem {
            label: p.label.clone(),
            depth: p.depth,
            section: p.section,
            locator: p.locator.clone(),
        })
        .collect();

    let mut pos = 0;
    let toc = build_level(&placed, &mut pos, 0);
    Some((toc, outline))
}

/// Assemble the `TocEntry` subtree for `level` by consuming `items` in preorder.
fn build_level(items: &[Placed], pos: &mut usize, level: usize) -> Vec<TocEntry> {
    let mut out: Vec<TocEntry> = Vec::new();
    while *pos < items.len() {
        let depth = items[*pos].depth;
        if depth < level {
            break;
        }
        if depth > level {
            // Deeper than expected: nest under the previous sibling (or, defensively,
            // stop if there is none — normalisation makes that unreachable).
            match out.last_mut() {
                Some(last) if level < MAX_DEPTH => {
                    last.children.extend(build_level(items, pos, level + 1));
                }
                _ => break,
            }
            continue;
        }
        let it = &items[*pos];
        *pos += 1;
        let children = if level < MAX_DEPTH {
            build_level(items, pos, level + 1)
        } else {
            Vec::new()
        };
        out.push(TocEntry {
            label: it.label.clone(),
            section: Some(it.section),
            children,
        });
    }
    out
}

/// Heading-based fallback: one flat entry per section, labelled by its first
/// `<hN>` heading, falling back to "Section N". Used when a book has no usable
/// NCX. (Retains the pre-NCX behaviour.)
pub(super) fn heading_fallback(sections: &[String]) -> (Vec<TocEntry>, Vec<OutlineItem>) {
    static HEADING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<h[1-6][^>]*>(.*?)</h[1-6]>").unwrap());
    let mut toc = Vec::with_capacity(sections.len());
    let mut outline = Vec::with_capacity(sections.len());
    for (i, s) in sections.iter().enumerate() {
        let label = HEADING
            .captures(s)
            .and_then(|c| c.get(1))
            .map(|m| clean_heading(m.as_str()))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("Section {}", i + 1));
        toc.push(TocEntry {
            label: label.clone(),
            section: Some(i),
            children: Vec::new(),
        });
        outline.push(OutlineItem {
            label,
            depth: 0,
            section: i,
            locator: None,
        });
    }
    (toc, outline)
}

/// The text of the first `<h1>`–`<h6>` heading at or shortly after byte offset
/// `pos` in `html` — the ground-truth chapter title at a nav target, used in
/// preference to a book's (often mangled) CNCX label. `None` if no heading is
/// found nearby.
pub(super) fn heading_at(html: &str, pos: usize) -> Option<String> {
    static HEADING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<h[1-6][^>]*>(.*?)</h[1-6]>").unwrap());
    // Look only in a bounded window from the target so we get *this* entry's
    // heading, not one much later in the section.
    let mut start = pos.min(html.len());
    while start < html.len() && !html.is_char_boundary(start) {
        start += 1;
    }
    let end = (start + 4000).min(html.len());
    let window = html.get(start..end)?;
    let text = clean_heading(HEADING.captures(window)?.get(1)?.as_str());
    (!text.is_empty()).then_some(text)
}

/// Strip tags, decode HTML entities, collapse whitespace, and cap the length of a
/// heading's inner HTML into a display label.
fn clean_heading(inner: &str) -> String {
    static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]+>").unwrap());
    // Drop soft hyphens (U+00AD) — invisible hyphenation hints some producers
    // (e.g. Standard Ebooks) embed mid-word — before collapsing whitespace.
    let text = decode_entities(&TAGS.replace_all(inner, " ")).replace('\u{00AD}', "");
    trim_to(&collapse_ws(&text), 100)
}

/// Decode the HTML entities that appear in headings (regex scanning bypasses the
/// scraper decode the block pipeline gets). Handles numeric (`&#39;`, `&#x2019;`)
/// and the common named entities; unknown entities pass through unchanged.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        match after.find(';').filter(|&p| p <= 12) {
            Some(semi) => match decode_entity(&after[..semi]) {
                Some(ch) => {
                    out.push(ch);
                    rest = &after[semi + 1..];
                }
                None => {
                    out.push('&');
                    rest = after;
                }
            },
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decode one entity body (the text between `&` and `;`).
fn decode_entity(ent: &str) -> Option<char> {
    if let Some(num) = ent.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse::<u32>().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match ent {
        "nbsp" => ' ',
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201C}',
        "rdquo" => '\u{201D}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "deg" => '°',
        "middot" => '·',
        "bull" => '•',
        _ => return None,
    })
}

/// Collapse runs of whitespace into single spaces and trim.
pub(super) fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to `max` chars, adding an ellipsis when cut.
pub(super) fn trim_to(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_named_and_numeric_entities() {
        assert_eq!(decode_entities("a&amp;b"), "a&b");
        assert_eq!(decode_entities("x&#39;y"), "x'y");
        assert_eq!(decode_entities("x&#x2019;y"), "x\u{2019}y");
        assert_eq!(decode_entities("a&nbsp;b"), "a b");
        assert_eq!(decode_entities("plain text"), "plain text");
        // Unknown entities pass through untouched.
        assert_eq!(decode_entities("a&bogus;b"), "a&bogus;b");
    }

    #[test]
    fn heading_scan_combines_hgroup_title_and_drops_soft_hyphens() {
        // A numbered part is an <hgroup> (numeral + title); its sub-sections are
        // bare <h3> numerals. The part must read "VII Rouen in February" (combined,
        // soft hyphen removed), and its inner <h2> must not be double-counted.
        let sections = vec![
            "<hgroup><h2>VII</h2><p class=\"t\">Rouen in Feb\u{ad}ruary</p></hgroup>\
             <h3>I</h3><p>body</p><h3>II</h3>"
                .to_string(),
        ];
        let (_, outline) = heading_scan(&sections).unwrap();
        assert_eq!(outline.len(), 3);
        assert_eq!(outline[0].label, "VII Rouen in February");
        assert_eq!(outline[0].depth, 0);
        assert_eq!((outline[1].label.as_str(), outline[1].depth), ("I", 1));
        assert_eq!((outline[2].label.as_str(), outline[2].depth), ("II", 1));
    }

    #[test]
    fn heading_scan_nests_by_level_and_decodes() {
        let sections = vec![
            "<h1>Chapter 1</h1><p>x</p><h2>Sub &amp; more</h2>".to_string(),
            "<h1>Chapter 2</h1>".to_string(),
        ];
        let (toc, outline) = heading_scan(&sections).unwrap();
        assert_eq!(toc.len(), 2);
        assert_eq!(toc[0].children.len(), 1);
        assert_eq!(toc[0].children[0].label, "Sub & more"); // entity decoded, nested
        assert_eq!(outline.len(), 3);
        assert_eq!((outline[1].depth, outline[1].section), (1, 0));
        assert_eq!((outline[2].depth, outline[2].section), (0, 1));
    }
}
