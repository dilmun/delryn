//! Reflow: turn a section's blocks into wrapped display lines for a given
//! content width. We wrap here (rather than letting the widget do it) so that
//! scroll position, total line count, and progress % are all exact and stable
//! across resizes. See `DESIGN.md` §2, §4.

use crate::document::Block;

/// Wrap a section's blocks to `width` columns, returning display lines.
pub fn wrap_blocks(blocks: &[Block], width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for block in blocks {
        match block {
            Block::Blank => out.push(String::new()),
            Block::Heading(t) | Block::Paragraph(t) => wrap_paragraph(t, width, &mut out),
        }
    }
    out
}

fn wrap_paragraph(text: &str, width: usize, out: &mut Vec<String>) {
    let mut line = String::new();
    let mut len = 0usize;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();

        // A word longer than the whole measure: flush, then hard-break it.
        if wlen > width {
            if len > 0 {
                out.push(std::mem::take(&mut line));
                len = 0;
            }
            let chars: Vec<char> = word.chars().collect();
            for chunk in chars.chunks(width) {
                out.push(chunk.iter().collect());
            }
            continue;
        }

        let needed = if len == 0 { wlen } else { len + 1 + wlen };
        if needed > width {
            out.push(std::mem::take(&mut line));
            line.push_str(word);
            len = wlen;
        } else {
            if len > 0 {
                line.push(' ');
                len += 1;
            }
            line.push_str(word);
            len += wlen;
        }
    }
    out.push(line);
}
