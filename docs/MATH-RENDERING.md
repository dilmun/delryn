# Universal math rendering — design

A ground-up design for rendering mathematics from **any** ebook, however the file
happens to encode it — MathML, LaTeX, MathJax span/SVG output, publisher pictures,
Word/OMML conversions, or plain Unicode. It is written for a terminal reader that can
display raster images (Kitty graphics protocol) and text.

This design is deliberately **encoding-first and code-independent**. It does not assume,
reuse, or inherit any existing parsing/encoding/decoding path. The organizing question
is not "how do we render our preferred encoding" but "given a real-world file that could
carry math in a dozen different shapes, how do we always show the reader correct math —
and beat the failures the top readers ship today."

---

## Non-negotiables

1. **Never blank.** Every equation renders *something* correct — crisp type, the
   publisher's picture, or a readable text form — and the pipeline can never drop an
   equation to nothing. (This is the single most common real-world failure; see below.)
2. **Encoding-agnostic.** Math arrives as markup *and* as pictures. Both are
   first-class inputs, not a preferred path plus a deprecated one.
3. **Recover before you render.** Machine-readable math (MathML/LaTeX) is frequently
   *hidden* in a file that only *displays* spans or an image. Find it before falling
   back to the picture.
4. **Crisp where possible, faithful where not.** Re-typeset recovered markup with a
   correct engine (so accents, braces, radicals, and limits are right); when no markup
   exists, show the publisher's own picture, sized and themed to the page.
5. **Universal, not library-specific.** The design targets the distribution of
   encodings *in the wild*, not any one collection.

---

## What the top readers get wrong — and how we beat each

The design's requirements are derived directly from where shipping readers fail.

| Reader | Documented failure | Our countermeasure |
|---|---|---|
| **KOReader** | Ignores EPUB3 MathML **and** doesn't use the `altimg` fallback image → the equation shows *nothing* ([#6678](https://github.com/koreader/koreader/issues/6678)) | "Never blank" is rule #1: an unrenderable source always descends the render ladder to the picture, then Unicode. |
| **Kindle / KF8** | No MathML at all; math must be shipped as images ([MobileRead](https://www.mobileread.com/forums/showthread.php?t=341507)) | Pictures are a first-class input. We render them well (sized text-relative, ink-cropped, recolored), and we still *try* to recover any sidecar MathML/LaTeX to upgrade them to crisp type. |
| **Thorium ≥3.2** | Regressed rendering of MathML under/over-braces and bars ([EDRLab](https://www.edrlab.org/2025/08/11/release-of-thorium-reader-for-desktop-3-2-elevating-your-reading-experience/)); relies on the browser's MathML | We re-typeset from a math IR with our own layout engine — accents/braces/bars are our code, not the browser's, so we don't inherit its bugs. |
| **Readium** | `<switch>` MathML can fall through to the image even where MathML is viable ([readium-js #306](https://github.com/readium/readium-js-viewer/issues/306)) | We read *both* branches of a `<switch>`: prefer the MathML branch for crisp type, keep the image branch as the fallback. |
| **Most reading systems** | Fragmented MathML support; publishers therefore ship image fallbacks and hidden MathML ([EPUBSecrets](https://epubsecrets.com/mathml-support-in-epub-reading-systems.php)) | We don't depend on *anyone's* MathML engine — we own the typesetting — and we harvest the hidden MathML those fallbacks carry. |

The through-line: **the top readers lose math because they bet on exactly one encoding
and have no real fallback.** This design bets on none and always has a fallback.

---

## The encoding landscape (what "math" actually is in files)

Every one of these occurs in real books and must be handled. "Recoverable source" =
machine-readable MathML/LaTeX we can re-typeset; otherwise the picture is authoritative.

| # | How it appears in the file | Recoverable source? | Notes / where it comes from |
|---|---|---|---|
| 1 | Native `<math>` Presentation MathML | ✅ MathML | EPUB3, WordToEPUB/OMML, LaTeXML. May carry `alttext`, `altimg`, `<annotation encoding="application/x-tex">`, `<semantics>`. |
| 2 | Content MathML (semantic `<apply>`…) | ✅ MathML (semantic) | Rarer; convertible to presentation. |
| 3 | Authored LaTeX in `alttext` / `<annotation …x-tex>` | ✅ LaTeX (authoritative) | The author's exact source — highest-fidelity when present. |
| 4 | Raw TeX/MathML in the text + a MathJax `<script>` config | ✅ LaTeX or MathML | Calibre/MathJax-config books ship the *source* inline; MathJax renders it at read time in a browser — we render it ourselves. |
| 5 | MathJax **CHTML** output (`<mjx-container>` of `<mjx-*>` spans) | ⚠️ Often ✅ via `<mjx-assistive-mml>` (MathJax **v3**); ❌ in **v4** (aria-label speech only) | The visible form is styled spans; v3 embeds hidden MathML for AT, v4 replaced it with speech strings ([MathJax v4 a11y](https://docs.mathjax.org/en/v4.1/basic/accessibility.html)). |
| 6 | MathJax **SVG** output (`<mjx-container><svg>`) | ⚠️ same as CHTML (assistive MathML if v3) | The SVG *is* crisp vector already; treat as a picture if no MathML. |
| 7 | Publisher **picture**, one image per equation (PNG/JPG/GIF/SVG) | Sometimes: sidecar MathML/LaTeX (see below) | The Kindle-era default. Sizing hints live in CSS `width` in `em`/`ex`. |
| 8 | Image **+ hidden MathML** in a sibling `<div hidden>` or HTML comment | ✅ MathML | DAISY-recommended fallback pattern; Wiley/For-Dummies use a trailing `<!--<m:math>…-->` comment. |
| 9 | `<switch>` with a MathML case and an image fallback | ✅ MathML (one branch) | EPUB fallback mechanism. |
| 10 | `<object>`/image with `aria-details` → an extended-description element | ✅ sometimes MathML/text | Accessibility linkage to a fuller description. |
| 11 | `role="math"` text span (plain Unicode) | ⚠️ text only | Simple inline equations authored as Unicode. |
| 12 | Plain Unicode / special math font, no markup | ❌ text only | Nothing to recover; render as text. |

**Key finding that reframes the whole problem:** a large share of books that *look*
image-only or span-only actually carry **recoverable MathML or LaTeX** (rows 3, 4, 5, 8,
9, 10). Harvesting it turns "just a picture / just spans" into crisp type. The books
where only a picture exists (row 7 with no sidecar, row 6 SVG, MathJax v4 without MathML)
are the genuine "picture is authoritative" case — and there, the picture is the answer,
not a thing to delete.

---

## Architecture: two ladders around one IR

For every math occurrence (block or inline), the pipeline runs a **Source-Recovery
ladder** at ingest, normalizes whatever it finds to **one internal math IR**, then a
**Render ladder** turns the IR (or the picture) into pixels — with a guaranteed fallback
at every rung so it can never produce nothing.

```
FILE  ─►  detect math occurrence  ─►  ┌─ SOURCE-RECOVERY LADDER (ingest) ────────────┐
                                      │ 1 authored LaTeX (annotation / alttext)      │  highest fidelity
                                      │ 2 Presentation MathML (native / hidden /      │
                                      │   switch / comment / mjx-assistive-mml)       │
                                      │ 3 Content MathML → presentation               │
                                      │ 4 MathJax aria-label speech (weak semantic)   │
                                      │ 5 publisher PICTURE (+ sizing hints)          │  authoritative visual
                                      │ 6 Unicode / role=math text                    │  last resort
                                      └──────────────────┬───────────────────────────┘
                                                         ▼  normalize
                                          ┌─ MATH IR ──────────────────────┐
                                          │ Typeset(ast)  |  Picture(bytes, │
                                          │ size-hint)    |  Text(unicode)  │
                                          └──────────┬─────────────────────┘
                                                     ▼
                                      ┌─ RENDER LADDER (draw) ─────────────────────────┐
                                      │ A Typeset: IR → layout → raster @ text em      │  crisp
                                      │   (own engine; correct accents/braces/limits)  │
                                      │   └ on engine failure ▼                        │
                                      │ B Picture: decode → ink-crop → size text-      │  faithful
                                      │   relative → recolor to theme → place          │
                                      │   └ if no picture ▼                            │
                                      │ C Text: Unicode approximation                  │  never blank
                                      └────────────────────────────────────────────────┘
```

Two independent ladders is the crux: **recovery quality and render quality degrade
separately.** A picture-only equation still renders (rung B). A recovered-MathML
equation that the typesetter chokes on falls to its own picture if it has one, else text
— it is never lost. This is precisely what KOReader lacks.

---

## The math IR (one model; every source normalizes into it)

A single internal representation that all recovered sources map *into*, so rendering is
written once:

- **`Typeset(MathNode)`** — a semantic math tree (fractions, scripts, radicals, big
  operators with limits, accents, over/under-braces, matrices, fenced groups, spacing,
  styled runs, text). Presentation MathML maps into it directly; Content MathML maps via
  its semantic operators; LaTeX maps via a LaTeX→tree parser. This is the crisp path.
- **`Picture { bytes, format, size_hint, ink }`** — an authored raster/vector the
  publisher shipped. `size_hint` is the CSS `em`/`ex` width when present (text-relative,
  DPI-independent); `ink` is the measured tight bounding box + text-line height for
  files with no size hint. First-class, not a fallback-of-last-resort.
- **`Text(String)`** — a Unicode approximation, always computed as the floor.

The IR is the boundary: source-recovery only ever produces one of these three; rendering
only ever consumes them. Neither half needs to know the other's encodings.

*Design invariant:* the IR carries the **baseline/metrics out-of-band** for `Typeset`
(so inline math sits on the text baseline) and the **text-relative size** for `Picture`
(so a 300-DPI equation image and a re-typeset one come out the same height as the prose).

---

## Source recovery — detect → recover, per encoding

Run top-to-bottom; take the first hit. Authored markup outranks synthesized; markup
outranks pictures; a picture outranks text. Every rung is independent and best-effort —
a failure falls through, never aborts.

| Priority | Detect | Recover to IR | Confidence |
|---|---|---|---|
| 1 | `<annotation encoding="application/x-tex">` or non-empty `alttext` on `<math>`; math `<img alt="$…$/\(…\)">` | LaTeX → `Typeset` | authoritative (author's source) |
| 2 | `<math>` present (native, or inside `<div hidden>`, `<switch>`, `<object>`, a trailing `<!--…-->` comment, or `<mjx-assistive-mml>`) | Presentation MathML → `Typeset` | high |
| 3 | Content MathML (`<apply>`, `<ci>`, `<cn>`) | Content→Presentation→`Typeset` | high |
| 4 | `<mjx-container>` with only spans/SVG and an `aria-label` | speech string → best-effort `Text` (or skip) | low |
| 5 | `<img>`/`<image>`/SVG with a math class/context, or the `altimg`/`<switch>`-image branch | `Picture` (+ `em`/`ex` size hint, else measured `ink`) | authoritative visual |
| 6 | `role="math"` span, or math-classed text with no markup | `Text` | low |

Notes that matter in practice:
- **Harvest, don't just detect.** MathJax v3 books (row 5 of the landscape) *display* as
  spans/SVG but carry `<mjx-assistive-mml>` — recovering it (priority 2) upgrades the
  whole book to crisp type. Do not treat MathJax output as opaque.
- **`<switch>` and `hidden`-div patterns carry both:** the MathML branch feeds
  `Typeset`, the image branch is retained as the `Picture` fallback for the *same*
  occurrence, so if typesetting fails we still show the publisher image (unlike Readium).
- **Comment/`altimg`/`alttext`** are the sidecars the KOReader failure ignores; we use
  all of them.
- **Confidence gates the render ladder:** a low-confidence `Text` never suppresses a
  `Picture` that also exists for the same node.

---

## Rendering — per IR variant

**A. `Typeset` (crisp).** Math tree → our layout engine → a themed raster placed at the
text-relative em, baseline-aligned. Because the engine is ours, under/over-braces,
radicals, big-operator limits, and accents are correct regardless of any browser's
MathML bugs. Display vs inline is a layout mode (limits above/below vs beside; same em,
not enlarged). On any engine error → fall to B.

**B. `Picture` (faithful).** Decode → crop to measured ink (so the file's whitespace
never inflates size) → scale so one text-line of ink equals the prose's line height
(from the `em`/`ex` hint when present — DPI-independent — else the measured ink height)
→ recolor black-on-transparent ink to the theme → place inline/at the block. SVG math is
rendered at the target pixel size (crisp, no DPI loss). On no picture → fall to C.

**C. `Text` (never blank).** The Unicode approximation, styled as math. Always available.

---

## Sizing — universal and text-relative

One rule for every variant: **an equation is sized relative to the surrounding text, not
to its own pixels.** `em_text = cell_height × EM_FACTOR`. `Typeset` renders at `em_text`
directly. `Picture` scales so its ink line-height matches `em_text`, using the authored
`em`/`ex` CSS width when present (exact, DPI-independent) and a measured ink line-height
otherwise. Result: a re-typeset equation, a 150-DPI GIF, and a 600-DPI PNG of the same
formula all render at the same on-screen size as the prose. A per-book size knob scales
all of them together.

---

## Delivery to the terminal

- **Primary:** the Kitty graphics protocol — transmit once, place by cell, move on
  scroll without re-decoding, paced so a math-dense page doesn't stall.
- **Capability-gated fallback:** where graphics aren't available, `Typeset`/`Picture`
  degrade to the `Text` (Unicode) form so the reader still shows the math (never blank).
- The delivery layer is a thin sink behind a capability query; it knows nothing about
  encodings.

---

## Robustness — the "never blank" guarantee, mechanically

- Every recovery rung and every render rung is wrapped so a failure *descends the
  ladder* rather than propagating. A parser panic, an unrenderable tree, a corrupt image
  — each falls to the next rung; the last rung (Text) always succeeds.
- Typesetting runs off the interactive thread; results are cached (keyed by source +
  size) so re-opens and re-wraps are free.
- No step assumes a specific encoding is present; absence is normal and handled.

---

## Explicit non-goals

- **Not** deleting or deprecating picture rendering — pictures are a primary, permanent
  input, because a large fraction of real books ship math only as pictures.
- **Not** depending on any external/browser MathML engine — we own typesetting so we
  don't inherit its bugs or its gaps.
- **Not** line-breaking *inside* a display equation in v1 (scale-to-fit / overflow),
  matching the state of the art.
- **Not** editing math; this is a reader.

---

## Why this is different from a markup-first design

A markup-first design ("assume MathML/LaTeX; normalize to vector; treat images as a
legacy tail to delete") is elegant only for a library that is already markup. Real
libraries are a mix dominated by **pictures and MathJax output**, and the readers that
bet on one encoding (KOReader, Kindle, browser-MathML readers) fail exactly where the
file doesn't match their bet. This design inverts that: it assumes **heterogeneity**,
recovers the best source per equation, renders pictures as first-class, and guarantees a
correct result for every encoding in the landscape table — which is the point of the
rewrite.
