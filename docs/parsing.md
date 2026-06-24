# Parsing architecture (delryn-format)

How we turn a publisher's XHTML into our `Block` model **without per-book
patches**. The rule: detect on *standardized semantics first*, fall back to
*stable toolchain fingerprints*, and only then to *generic heuristics*. Route by
**toolchain, not publisher** — Packt and Manning each ship two markup families.

Researched against the [EPUB 3.3 spec](https://www.w3.org/TR/epub-33/),
[SSV 1.1](https://www.w3.org/TR/epub-ssv-11/),
[DPUB-ARIA](https://www.w3.org/TR/dpub-aria-1.0/), and a survey of the real
library (O'Reilly/HTMLBook, Apress/InDesign, No Starch, Manning FrameMaker+Sigil,
Wiley/Dummies, Packt highlight.js+MathTools, Pearson, tex4ht self-pub).

## Module layout (`delryn-format/src/html/`)

| File | Single responsibility |
|---|---|
| `mod.rs` | Orchestrate: normalize → detect toolchain → walk tree → dispatch by role → assemble `Block`s. |
| `normalize.rs` | Input fixups before parse: expand self-closing non-void tags (XHTML vs HTML5), strip BOM. |
| `dom.rs` | Shared low-level helpers: `epub:type`/`role`/`class` token matching, descendant text. |
| `toolchain.rs` | `Toolchain` detection + `ToolchainProfile` registry — the per-toolchain quirk **data**. |
| `semantics.rs` | `ElementRole` + `classify(el, profile)` — the one priority-ordered decision. |
| `inline.rs` | Inline runs: styles, links/anchors, inline code, icon glyphs, inline math, `<br>`. |
| `code.rs` | Code-block extraction + line-reassembly strategies, line-number strip, language detect. |
| `table.rs` | `<table>` → `Block::Table`. |
| `callout.rs` | Admonition extraction (kind + content). |
| `math.rs` | DOM math detection/extraction (native `<math>`, MathML/LaTeX in `<img alt>`) → `mathml.rs`. |

`mathml.rs` (string MathML → Unicode) stays format-agnostic and separate so PDF
can reuse it. Dependencies are downward-only/acyclic: `mod` → extractors →
`semantics` → `toolchain`; everyone → `dom`.

## Detection priority per content type

Detect on the first signal that matches, top to bottom.

**Code blocks:** `<pre><code class="language-X">` → toolchain fingerprint
(Pandoc `.sourceCode`, DocBook `.programlisting`, Asciidoctor `.listingblock`,
LaTeX `.lstlisting`, O'Reilly `data-type=programlisting`) → no-`<pre>` heuristic
(monospace CSS font + code-ish class, `<br>`/per-line-element reassembly).
*Line numbers are always chrome — strip; we render our own. Language class is a
hint, never required.*

**Callouts:** `epub:type`/`role`/`data-type` (`note`/`tip`/`notice`/`sidebar`) →
toolchain class (`admonitionblock`, `packt_tip`, Wiley `Normal-w-icon` + icon
`alt`) → generic `*note|tip|warning|callout*` substring.

**Footnotes:** `epub:type="noteref|footnote"` + `role="doc-noteref|doc-footnote"`
(reliably authored — high value). The definition keeps its raw `id`; a reference
resolves to it by exact id, then a digit-normalized fallback
(`Block::footnote_matches` / `find_footnote`). *Ref→def→back jump + preview is
reader-cursor work; the anchors and resolver are ready for it.*

**Math:** native `<math>` (`alttext`/`<annotation>` LaTeX, else presentation
walk) → MathML/LaTeX escaped in `<img alt>` → descriptive alt → placeholder.

**Tables / headings / figures:** real `<table>`/`<h1-6>`/`<figure>` →
heading-class fallback (`*head*`/`*title*`; Packt & old Manning fake headings).

## Adding a publisher/toolchain

1. Add a `Toolchain` variant + its fingerprint in `toolchain.rs`.
2. Fill its `ToolchainProfile` (inline-code classes, code line-reassembly
   strategy, admonition-kind source, heading-class patterns, language source).

That's it — no edits to `semantics.rs` or the extractors. New content *type* =
one `ElementRole` + one classifier branch + one extractor.

## Honest limits

A text parser can't recover image-only books (Pearson: every figure/table/
equation is a PNG with identical `alt="images"`) or font-class-only inline code
(self-pub "Practical Guide": inline and block code share one monospace class).
These get graceful placeholders, not reconstruction.
