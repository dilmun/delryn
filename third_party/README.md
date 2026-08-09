# Vendored upstream crates

Patched copies of three crates from the [RaTeX](https://github.com/erweixin/RaTeX) maths
renderer, carried here **only until an upstream release includes the fixes**. Source is
`ratex-*` 0.1.12 as published to crates.io, unmodified except where noted below, and wired
in through `[patch.crates-io]` in the workspace manifest. They are excluded from the
workspace so they keep upstream's edition and lint settings and stay easy to diff against
the released crate.

Upstream is MIT licensed (`license = "MIT"` in each manifest); the published packages ship
no `LICENSE` file, so the canonical text lives with the project at the repository above.

## The patches

Between them these cut ~205 MB of resident memory from any document containing maths.
None of the changes is delryn-specific — each is a plain bug fix.

### `ratex-unicode-font`

* **One shared system font.** `UNICODE_FONT` and `SYSTEM_FALLBACK_FONT` each reached
  `discover_system_font` through their own `OnceLock`, reading the same file into two
  buffers — 22 MB duplicated on macOS, for the life of the process. They now share
  `DISCOVERED_SYSTEM_FONT`.
* **`is_emoji_char`**, and `emoji_raster_for_char` consults it before loading anything.
  It previously loaded the emoji face and *then* asked whether the character existed.

### `ratex-render`

* **`try_blit_emoji_raster_fallback` checks the codepoint first.** This is the
  last-resort glyph path, reached by anything the main faces cannot draw, so a maths
  document sends ordinary symbols through it — and answering "no" cost a 183 MB load of
  Apple Color Emoji, cached for the life of the process, for a book containing no emoji.

### `ratex-font-loader`

* **`EmojiFallback` is planned only for actual emoji.** It was bundled with the CJK
  fallbacks and added whenever *any* non-ASCII character lacked KaTeX metrics, so one `−`
  pulled in the emoji face. Now gated on `ratex_unicode_font::is_emoji_char`, the same test
  the renderer uses.

## Maintenance

`[patch.crates-io]` only applies when the patched version satisfies the dependency
requirement, so bumping `ratex-*` means re-syncing these copies or cargo will refuse to
build. When upstream ships the fixes, delete these directories and the patch entries.
