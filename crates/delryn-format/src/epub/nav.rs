//! EPUB navigation: table of contents, reading-start landmark, and the flat
//! outline the reader navigates.
//!
//! Source priority (per EPUB 3.3): the **EPUB 3 Navigation Document**
//! (`<nav epub:type="toc">`) when present, else the legacy **NCX** the `epub`
//! crate parses into `doc.toc`, else the **spine** order. We also read the
//! `landmarks` nav to start reading at `bodymatter` (skip front matter).

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use ego_tree::NodeRef;
use epub::doc::{EpubDoc, NavPoint};
use scraper::{Html, Node};

use super::{OutlineItem, TocEntry};
use crate::container::{body_or_root, descendant_text, filename_eq, has_token};

/// Resolved navigation for a document.
pub(super) struct Navigation {
    pub toc: Vec<TocEntry>,
    pub outline: Vec<OutlineItem>,
    /// Spine index to open at — the `bodymatter` landmark, else 0.
    pub start_section: usize,
}

/// Build navigation, preferring the EPUB 3 nav document over the NCX.
pub(super) fn build(doc: &mut EpubDoc<BufReader<File>>) -> Navigation {
    let nav = parse_nav_document(doc);

    let toc = match &nav {
        Some(n) if !n.toc.is_empty() => n.toc.clone(),
        _ => ncx_toc(doc),
    };

    let mut outline = Vec::new();
    build_outline(&toc, 0, 0, &mut outline);
    if outline.is_empty() {
        // No usable TOC: one entry per spine section.
        outline = (0..doc.get_num_chapters())
            .map(|s| OutlineItem {
                label: format!("Section {}", s + 1),
                depth: 0,
                section: s,
                locator: None,
            })
            .collect();
    }

    let start_section = nav.and_then(|n| n.start_section).unwrap_or(0);
    Navigation {
        toc,
        outline,
        start_section,
    }
}

// ── EPUB 3 Navigation Document ───────────────────────────────────────────────

struct ParsedNav {
    toc: Vec<TocEntry>,
    start_section: Option<usize>,
}

/// Read and parse the EPUB 3 nav document (if the package declares one).
fn parse_nav_document(doc: &mut EpubDoc<BufReader<File>>) -> Option<ParsedNav> {
    let nav_id = doc.get_nav_id()?;
    // The nav's own directory — hrefs inside it are relative to it.
    let nav_dir = doc
        .resources
        .get(&nav_id)
        .and_then(|r| r.path.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let (xhtml, _) = doc.get_resource_str(&nav_id)?;

    let html = Html::parse_document(&xhtml);
    let body = body_or_root(&html);

    let mut toc = Vec::new();
    let mut start_section = None;
    for nav in body
        .descendants()
        .filter(|n| matches!(n.value(), Node::Element(e) if e.name() == "nav"))
    {
        let Node::Element(e) = nav.value() else {
            continue;
        };
        let etype = e.attr("epub:type").unwrap_or("");
        if has_token(etype, "toc") && toc.is_empty() {
            if let Some(ol) = child_element(nav, "ol") {
                toc = parse_ol(ol, &nav_dir, doc);
            }
        } else if has_token(etype, "landmarks") {
            start_section = bodymatter_section(nav, &nav_dir, doc);
        }
    }
    Some(ParsedNav { toc, start_section })
}

/// Parse a nav `<ol>` into TOC entries (each `<li>`: an `<a>`/`<span>` label,
/// then an optional nested `<ol>`).
fn parse_ol(ol: NodeRef<Node>, nav_dir: &Path, doc: &EpubDoc<BufReader<File>>) -> Vec<TocEntry> {
    let mut entries = Vec::new();
    for li in ol
        .children()
        .filter(|n| matches!(n.value(), Node::Element(e) if e.name() == "li"))
    {
        // Label + target from the first <a> (or unlinked <span>).
        let anchor = child_element(li, "a");
        let label_node = anchor.or_else(|| child_element(li, "span"));
        let Some(label_node) = label_node else {
            continue;
        };
        let label = descendant_text(label_node, false, None).trim().to_string();
        if label.is_empty() {
            continue;
        }
        let section = anchor
            .and_then(|a| a.value().as_element()?.attr("href"))
            .and_then(|href| resolve_href(href, nav_dir, doc));
        let children = child_element(li, "ol")
            .map(|ol| parse_ol(ol, nav_dir, doc))
            .unwrap_or_default();
        entries.push(TocEntry {
            label,
            section,
            children,
        });
    }
    entries
}

/// The spine index of the `bodymatter` landmark, if the landmarks nav has one.
fn bodymatter_section(
    nav: NodeRef<Node>,
    nav_dir: &Path,
    doc: &EpubDoc<BufReader<File>>,
) -> Option<usize> {
    nav.descendants()
        .filter_map(|n| n.value().as_element().map(|e| (n, e)))
        .filter(|(_, e)| e.name() == "a")
        .find(|(_, e)| has_token(e.attr("epub:type").unwrap_or(""), "bodymatter"))
        .and_then(|(_, e)| e.attr("href"))
        .and_then(|href| resolve_href(href, nav_dir, doc))
}

// ── NCX + spine fallbacks ────────────────────────────────────────────────────

/// TOC from the NCX (the `epub` crate parses `toc.ncx` into `doc.toc`).
fn ncx_toc(doc: &EpubDoc<BufReader<File>>) -> Vec<TocEntry> {
    doc.toc.iter().map(|np| convert_navpoint(np, doc)).collect()
}

fn convert_navpoint(np: &NavPoint, doc: &EpubDoc<BufReader<File>>) -> TocEntry {
    TocEntry {
        label: np.label.clone(),
        section: resolve_path(&np.content, doc),
        children: np
            .children
            .iter()
            .map(|c| convert_navpoint(c, doc))
            .collect(),
    }
}

/// Flatten the TOC tree into a depth-tagged outline, preserving hierarchy. Each
/// entry locates by its label text within the (possibly shared) section; entries
/// with no resolved section inherit their parent's.
fn build_outline(entries: &[TocEntry], depth: usize, parent: usize, out: &mut Vec<OutlineItem>) {
    for e in entries {
        let section = e.section.unwrap_or(parent);
        out.push(OutlineItem {
            label: e.label.clone(),
            depth,
            section,
            locator: Some(e.label.clone()),
        });
        build_outline(&e.children, depth + 1, section, out);
    }
}

// ── Resolution helpers ───────────────────────────────────────────────────────

/// Resolve an `href` (relative to `nav_dir`) to a spine index, tolerating
/// `#fragment`s and base-path mismatches. Shared by nav parsing and the reader's
/// cross-reference / citation jumps.
pub(super) fn resolve_href(
    href: &str,
    nav_dir: &Path,
    doc: &EpubDoc<BufReader<File>>,
) -> Option<usize> {
    let raw = href.split('#').next().unwrap_or(href);
    resolve_path(&super::normalize_path(&nav_dir.join(raw)), doc)
}

/// Map a resource path to a spine index (direct lookup, else file-name match).
fn resolve_path(content: &Path, doc: &EpubDoc<BufReader<File>>) -> Option<usize> {
    let raw = content.to_string_lossy();
    let raw = raw.split('#').next().unwrap_or(&raw);
    let path = PathBuf::from(raw);
    if let Some(i) = doc.resource_uri_to_chapter(&path) {
        return Some(i);
    }
    let target = path.file_name()?;
    doc.spine.iter().position(|item| {
        doc.resources
            .get(&item.idref)
            .is_some_and(|res| filename_eq(&res.path, target))
    })
}

// ── Small DOM helpers (local to nav parsing) ─────────────────────────────────

fn child_element<'a>(node: NodeRef<'a, Node>, name: &str) -> Option<NodeRef<'a, Node>> {
    node.children()
        .find(|n| matches!(n.value(), Node::Element(e) if e.name() == name))
}
