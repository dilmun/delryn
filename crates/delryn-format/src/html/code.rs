//! Code-block detection and extraction: recognise `<pre>`/styled containers/
//! `<br>`-delimited code, reassemble lines, strip line numbers, detect language.

use super::*;

/// A non-`<pre>` block that is really a code listing, by a code-container class
/// from the toolchain registry (Springer/Apress `ProgramCode`, LaTeX
/// `lstlisting`, DocBook `programlisting`, …).
pub(super) fn is_code_container(e: &scraper::node::Element) -> bool {
    matches!(e.name(), "div" | "section" | "p")
        && profile()
            .code_container_classes
            .iter()
            .any(|t| class_has_token(e, t))
}

/// Whether an element should render as a code block: a `<pre>`, a styled code
/// container (by class), or a block holding a multi-line `<code>`.
pub(super) fn is_code_block(e: &scraper::node::Element, node: NodeRef<Node>) -> bool {
    e.name() == "pre" || is_code_container(e) || has_multiline_code(node)
}

/// A block whose direct `<code>` child spans several lines (its lines split by
/// `<br/>`) — a code listing written without `<pre>`, e.g.
/// `<p class="Code"><code>line1<br/>line2</code></p>`. The `<br/>` requirement
/// keeps short inline `<code>` snippets out.
pub(super) fn has_multiline_code(node: NodeRef<Node>) -> bool {
    node.children().any(|c| {
        matches!(c.value(), Node::Element(e) if e.name() == "code")
            && c.descendants()
                .any(|d| matches!(d.value(), Node::Element(e) if e.name() == "br"))
    })
}

/// Code lines from a styled code container. When the source wraps each line in a
/// per-line element (`code_line_classes`, e.g. Springer/Apress `FixedLine`), one
/// line per such element; otherwise fall back to splitting the concatenated text
/// on newlines.
pub(super) fn code_lines(node: NodeRef<Node>) -> Vec<String> {
    let is_line = |e: &scraper::node::Element| {
        profile()
            .code_line_classes
            .iter()
            .any(|t| class_has_token(e, t))
    };
    let fixed: Vec<String> = node
        .descendants()
        .filter(|n| matches!(n.value(), Node::Element(e) if is_line(e)))
        .map(code_text)
        .collect();
    let raw = if fixed.is_empty() {
        code_text(node)
    } else {
        fixed.join("\n")
    };
    // Normalise the spacing publishers use in code: non-breaking spaces for
    // indentation back to real spaces, and drop zero-width spaces.
    let normalized = raw.replace('\u{a0}', " ").replace('\u{200b}', "");
    // Collapse runs of blank lines (publishers often emit several `<br/>` between
    // code lines) down to a single blank.
    let mut out = Vec::new();
    let mut prev_blank = false;
    for l in normalized.split('\n') {
        let l = l.trim_end().to_string();
        let blank = l.is_empty();
        if blank && prev_blank {
            continue;
        }
        prev_blank = blank;
        out.push(l);
    }
    out
}

/// Concatenate descendant text, turning `<br/>` into a newline — so code laid out
/// with `<br/>` line breaks (instead of `<pre>`) keeps its line structure.
pub(super) fn code_text(node: NodeRef<Node>) -> String {
    crate::container::descendant_text(node, true, None)
}

/// Some books bake line numbers into the code text ("1 import std;"). When most
/// lines start with their own 1-based index, strip those so our gutter is the
/// single source of line numbers.
pub(super) fn strip_line_numbers(lines: Vec<String>) -> Vec<String> {
    let mut nonempty = 0usize;
    let mut numbered = 0usize;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.is_empty() {
            continue;
        }
        nonempty += 1;
        if let Some(rest) = t.strip_prefix(&(i + 1).to_string())
            && (rest.is_empty() || rest.starts_with([' ', '\t']))
        {
            numbered += 1;
        }
    }
    if nonempty < 2 || numbered * 4 < nonempty * 3 {
        return lines;
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let t = l.trim_start();
            match t.strip_prefix(&(i + 1).to_string()) {
                Some(rest) => rest.strip_prefix([' ', '\t']).unwrap_or(rest).to_string(),
                None => l,
            }
        })
        .collect()
}

pub(super) fn trim_blank_edges(lines: impl Iterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = lines.collect();
    while v.first().is_some_and(|l| l.trim().is_empty()) {
        v.remove(0);
    }
    while v.last().is_some_and(|l| l.trim().is_empty()) {
        v.pop();
    }
    v
}

/// Detect a code language from a `class="language-xxx"`/`lang-xxx` on the
/// `<pre>` or its child `<code>`.
pub(super) fn detect_lang(node: NodeRef<Node>) -> Option<String> {
    fn from_class(class: &str) -> Option<String> {
        class.split_whitespace().find_map(|c| {
            c.strip_prefix("language-")
                .or_else(|| c.strip_prefix("lang-"))
                .map(|s| s.to_string())
        })
    }
    for n in node.descendants() {
        if let Node::Element(e) = n.value()
            && let Some(class) = e.attr("class")
            && let Some(lang) = from_class(class)
        {
            return Some(lang);
        }
    }
    None
}
