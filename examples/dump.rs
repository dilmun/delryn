//! Smoke test for the non-UI pipeline: open an EPUB and dump metadata, TOC,
//! and the first wrapped lines. Usage: `cargo run --example dump -- <file.epub>`

use anyhow::Result;
use delryn::document::epub::EpubDocument;
use delryn::document::{Document, TocEntry};
use delryn::layout::wrap_blocks;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("usage: dump <file.epub>");
    let mut doc = EpubDocument::open(&path)?;

    let meta = doc.metadata();
    println!("title:    {}", meta.title);
    println!("authors:  {:?}", meta.authors);
    println!("year:     {:?}", meta.year);
    println!("language: {:?}", meta.language);
    println!("size:     {} bytes", meta.size);
    println!("cover:    {}", meta.cover.is_some());
    println!("sections: {}", doc.section_count());

    println!("\n--- TOC ---");
    print_toc(doc.toc(), 0);

    let idx: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    println!("\n--- section {idx}, wrapped to 72 ---");
    let section = doc.load_section(idx)?;
    let lines = wrap_blocks(&section.blocks, 72);
    for line in lines.iter().take(20) {
        println!("{line}");
    }
    println!("... ({} total wrapped lines)", lines.len());
    Ok(())
}

fn print_toc(entries: &[TocEntry], depth: usize) {
    for e in entries.iter().take(40) {
        println!("{}{} -> {:?}", "  ".repeat(depth), e.label, e.section);
        print_toc(&e.children, depth + 1);
    }
}
