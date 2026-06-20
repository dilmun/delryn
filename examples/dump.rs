//! Smoke test for the non-UI pipeline: open an EPUB and dump metadata, TOC,
//! and the first wrapped lines. Usage: `cargo run --example dump -- <file.epub>`

use anyhow::Result;
use delryn::document::Document;
use delryn::document::epub::EpubDocument;
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

    println!("\n--- outline ---");
    for item in doc.outline().iter().take(60) {
        let indent = "  ".repeat(item.depth);
        let loc = item.locator.as_deref().unwrap_or("(top)");
        println!("{indent}{} [s{} -> {loc}]", item.label, item.section);
    }

    let idx: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    println!("\n--- section {idx}, wrapped to 72 ---");
    let section = doc.load_section(idx)?;
    let lines = wrap_blocks(&section.blocks, 72);
    for line in lines.iter() {
        println!("{}", line.text());
    }
    println!("... ({} total wrapped lines)", lines.len());
    Ok(())
}
