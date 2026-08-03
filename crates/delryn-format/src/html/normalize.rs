//! Input normalisation applied before HTML parsing.

use std::sync::LazyLock;

use regex::Regex;

/// EPUB content is XHTML, where `<span id="x"/>` is self-closing. The HTML5
/// parser instead reads it as an *unclosed* `<span>` that swallows every
/// following sibling — collapsing whole sections (headings, paragraphs, code)
/// into one inline blob. Rewrite self-closing tags of non-void elements to
/// explicit empty pairs so the document structure survives. Void elements
/// (`<br/>`, `<img/>`, …) are valid self-closing in HTML and left as-is.
pub(super) fn expand_self_closing(xhtml: &str) -> std::borrow::Cow<'_, str> {
    // <name attrs… /> — attrs may hold quoted `>`/`/`, so consume quotes whole.
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"<([A-Za-z][\w:-]*)((?:"[^"]*"|'[^']*'|[^>"'])*?)\s*/>"#).unwrap()
    });
    let re = &*RE;
    re.replace_all(xhtml, |c: &regex::Captures| {
        let name = &c[1];
        if is_void_element(name) {
            c[0].to_string()
        } else {
            format!("<{name}{}></{name}>", &c[2])
        }
    })
}

fn is_void_element(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}
