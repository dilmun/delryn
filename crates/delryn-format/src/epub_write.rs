//! Writing into an EPUB — currently just embedding a cover image.
//!
//! The `epub` crate is read-only, so we rewrite the container ourselves: copy
//! every existing zip entry verbatim, swap in a patched OPF, and add the new
//! cover image. The OPF gains both conventions so any reader picks the cover up:
//! the EPUB3 manifest `properties="cover-image"` and the legacy EPUB2
//! `<meta name="cover">`. Scope is deliberately *metadata only* — no XHTML cover
//! page is inserted into the spine (see the reader's `get_cover` path).

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use regex::Regex;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Stable id + filename stem for the cover we write, so re-embedding overwrites
/// our previous attempt rather than piling up dead images.
const COVER_ID: &str = "delryn-cover";

/// Embed `image` as the cover of the EPUB at `epub_path`, rewriting the file in
/// place via an atomic temp-then-rename. The image format is sniffed from its
/// magic bytes; unsupported formats are rejected. Returns the mime type written.
pub fn embed_cover(epub_path: &Path, image: &[u8]) -> Result<String> {
    let (mime, ext) =
        sniff_image(image).context("cover is not a supported image (jpeg/png/gif/webp)")?;

    let mut archive = ZipArchive::new(
        File::open(epub_path).with_context(|| format!("opening {}", epub_path.display()))?,
    )
    .context("reading EPUB container")?;

    // Locate the OPF package document via the OCF container, then read it.
    let container = read_entry(&mut archive, "META-INF/container.xml")
        .context("EPUB is missing META-INF/container.xml")?;
    let opf_path = opf_path_from_container(&container)
        .context("could not find the OPF path in container.xml")?;
    let opf = read_entry(&mut archive, &opf_path).context("reading the OPF package document")?;

    // The cover lives next to the OPF; its manifest href is relative to it.
    let dir = parent_dir(&opf_path);
    let cover_href = format!("{COVER_ID}.{ext}");
    let cover_entry = if dir.is_empty() {
        cover_href.clone()
    } else {
        format!("{dir}/{cover_href}")
    };
    let new_opf = patch_opf(&opf, &cover_href, mime)?;

    // Rewrite into a sibling temp file so a failure never truncates the original.
    let tmp_path = epub_path.with_extension("delryn-tmp");
    let result = write_epub(
        &mut archive,
        &tmp_path,
        &opf_path,
        &dir,
        &cover_entry,
        &new_opf,
        image,
    );
    match result {
        Ok(()) => {
            std::fs::rename(&tmp_path, epub_path).context("replacing the original EPUB")?;
            Ok(mime.to_string())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

/// Stream a rebuilt EPUB into `tmp_path`: `mimetype` first (stored), then every
/// original entry except the ones we replace, then the new cover and OPF.
fn write_epub<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    tmp_path: &Path,
    opf_path: &str,
    cover_dir: &str,
    cover_entry: &str,
    new_opf: &str,
    image: &[u8],
) -> Result<()> {
    let mut zip = ZipWriter::new(File::create(tmp_path).context("creating temp EPUB")?);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    // OCF requires `mimetype` to be the first entry and uncompressed.
    zip.start_file("mimetype", stored)?;
    zip.write_all(b"application/epub+zip")?;

    for i in 0..archive.len() {
        let name = match archive.name_for_index(i) {
            Some(n) => n.to_string(),
            None => continue,
        };
        // Skip what we write fresh: the mimetype, the OPF, and any prior cover
        // we embedded (matched by our stable stem so re-embeds don't accumulate).
        if name == "mimetype" || name == opf_path || is_prior_cover(&name, cover_dir) {
            continue;
        }
        // Copy verbatim — no recompression of existing content.
        zip.raw_copy_file(archive.by_index_raw(i)?)?;
    }

    // Cover images are already compressed; store rather than waste CPU deflating.
    zip.start_file(cover_entry, stored)?;
    zip.write_all(image)?;
    zip.start_file(opf_path, deflated)?;
    zip.write_all(new_opf.as_bytes())?;
    zip.finish().context("finalizing EPUB")?;
    Ok(())
}

/// Read a single zip entry into a string.
fn read_entry<R: Read + std::io::Seek>(archive: &mut ZipArchive<R>, name: &str) -> Result<String> {
    let mut s = String::new();
    archive
        .by_name(name)
        .with_context(|| format!("entry {name} not found"))?
        .read_to_string(&mut s)?;
    Ok(s)
}

/// True if `name` is a cover we wrote on a previous embed (same directory as the
/// OPF, filename `delryn-cover.<ext>`), so it should be dropped on rewrite.
fn is_prior_cover(name: &str, cover_dir: &str) -> bool {
    let rel = if cover_dir.is_empty() {
        Some(name)
    } else {
        name.strip_prefix(&format!("{cover_dir}/"))
    };
    rel.is_some_and(|r| !r.contains('/') && r.starts_with(&format!("{COVER_ID}.")))
}

/// Detect a supported raster format from its leading magic bytes, returning the
/// `(mime, extension)` pair to use in the OPF and zip entry.
fn sniff_image(b: &[u8]) -> Option<(&'static str, &'static str)> {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("image/jpeg", "jpg"))
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "png"))
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some(("image/gif", "gif"))
    } else if b.len() >= 12 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP" {
        Some(("image/webp", "webp"))
    } else {
        None
    }
}

/// Pull the OPF's full path out of the OCF container document.
fn opf_path_from_container(xml: &str) -> Option<String> {
    let re = Regex::new(r#"full-path\s*=\s*["']([^"']+)["']"#).ok()?;
    re.captures(xml).map(|c| c[1].to_string())
}

/// The directory portion of a zip-internal path (`""` when at the root).
fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

/// Rewrite the OPF so our image is the one and only cover: strip any existing
/// `cover-image` property and prior `delryn-cover` item / `<meta name="cover">`,
/// then add a fresh manifest item plus an EPUB2-compatible cover meta.
fn patch_opf(opf: &str, cover_href: &str, mime: &str) -> Result<String> {
    let mut out = strip_cover_image_property(opf);
    out = remove_prior_delryn_cover(&out);
    out = remove_cover_meta(&out);

    let meta = format!(r#"<meta name="cover" content="{COVER_ID}"/>"#);
    let item = format!(
        r#"<item id="{COVER_ID}" href="{cover_href}" media-type="{mime}" properties="cover-image"/>"#
    );

    let with_meta = insert_before(&out, "</metadata>", &meta)
        .context("OPF has no <metadata> section to add the cover to")?;
    let with_item = insert_before(&with_meta, "</manifest>", &item)
        .context("OPF has no <manifest> section to add the cover to")?;
    Ok(with_item)
}

/// Insert `fragment` (indented, on its own line) before the first `needle`.
fn insert_before(haystack: &str, needle: &str, fragment: &str) -> Option<String> {
    haystack
        .find(needle)
        .map(|i| format!("{}  {fragment}\n  {}", &haystack[..i], &haystack[i..]))
}

/// Remove the `cover-image` token from every manifest item's `properties`,
/// dropping the attribute entirely when it becomes empty.
fn strip_cover_image_property(opf: &str) -> String {
    let re = Regex::new(r#"\s+properties\s*=\s*"([^"]*)""#).expect("static regex");
    re.replace_all(opf, |caps: &regex::Captures| {
        let kept: Vec<&str> = caps[1]
            .split_whitespace()
            .filter(|t| *t != "cover-image")
            .collect();
        if kept.is_empty() {
            String::new()
        } else {
            format!(r#" properties="{}""#, kept.join(" "))
        }
    })
    .into_owned()
}

/// Drop any manifest item we wrote on a previous embed (identified by our stem).
fn remove_prior_delryn_cover(opf: &str) -> String {
    let re = Regex::new(&format!(r#"\s*<item\b[^>]*{COVER_ID}[^>]*?/?>"#)).expect("static regex");
    re.replace_all(opf, "").into_owned()
}

/// Drop any existing `<meta name="cover" .../>` regardless of attribute order.
fn remove_cover_meta(opf: &str) -> String {
    let re = Regex::new(r#"\s*<meta\b[^>]*\bname\s*=\s*"cover"[^>]*?/?>"#).expect("static regex");
    re.replace_all(opf, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_known_formats() {
        assert_eq!(
            sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(("image/jpeg", "jpg"))
        );
        assert_eq!(
            sniff_image(b"\x89PNG\r\n\x1a\n....").map(|m| m.1),
            Some("png")
        );
        assert_eq!(sniff_image(b"GIF89a..").map(|m| m.1), Some("gif"));
        assert_eq!(
            sniff_image(b"RIFF\0\0\0\0WEBP").map(|m| m.0),
            Some("image/webp")
        );
        assert!(sniff_image(b"not an image").is_none());
    }

    #[test]
    fn reads_opf_path_from_container() {
        let xml = r#"<?xml version="1.0"?><container><rootfiles>
            <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
            </rootfiles></container>"#;
        assert_eq!(
            opf_path_from_container(xml).as_deref(),
            Some("OEBPS/content.opf")
        );
    }

    #[test]
    fn parent_dir_handles_root_and_nested() {
        assert_eq!(parent_dir("content.opf"), "");
        assert_eq!(parent_dir("OEBPS/content.opf"), "OEBPS");
        assert_eq!(parent_dir("a/b/c.opf"), "a/b");
    }

    #[test]
    fn prior_cover_only_matches_our_stem_in_opf_dir() {
        assert!(is_prior_cover("OEBPS/delryn-cover.jpg", "OEBPS"));
        assert!(is_prior_cover("OEBPS/delryn-cover.png", "OEBPS"));
        assert!(is_prior_cover("delryn-cover.jpg", ""));
        // A user's own cover or a nested path must be left alone.
        assert!(!is_prior_cover("OEBPS/cover.jpg", "OEBPS"));
        assert!(!is_prior_cover("OEBPS/sub/delryn-cover.jpg", "OEBPS"));
        assert!(!is_prior_cover("delryn-cover.jpg", "OEBPS"));
    }

    #[test]
    fn strips_only_the_cover_image_token() {
        let opf = r#"<item id="a" properties="cover-image"/><item id="b" properties="nav cover-image"/><item id="c" properties="nav"/>"#;
        let out = strip_cover_image_property(opf);
        assert!(!out.contains("cover-image"));
        assert!(out.contains(r#"<item id="a"/>"#)); // attribute dropped entirely
        assert!(out.contains(r#"properties="nav""#)); // sibling token kept
    }

    #[test]
    fn patch_repoints_existing_epub2_cover_and_is_idempotent() {
        let opf = r#"<package><metadata>
  <meta name="cover" content="old-id"/>
</metadata><manifest>
  <item id="old-id" href="img/cover.jpeg" media-type="image/jpeg" properties="cover-image"/>
  <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
</manifest></package>"#;

        let once = patch_opf(opf, "delryn-cover.jpg", "image/jpeg").unwrap();
        // Exactly one cover meta, pointing at our id.
        assert_eq!(once.matches(r#"name="cover""#).count(), 1);
        assert!(once.contains(r#"content="delryn-cover""#));
        // Exactly one cover-image property, on our new item.
        assert_eq!(once.matches("cover-image").count(), 1);
        assert!(once.contains(r#"id="delryn-cover" href="delryn-cover.jpg""#));
        // The old item survives but is no longer a cover.
        assert!(once.contains(r#"id="old-id""#));
        assert!(once.contains(r#"id="ch1""#));

        // Re-embedding (e.g. a PNG) must not duplicate items or metas.
        let twice = patch_opf(&once, "delryn-cover.png", "image/png").unwrap();
        assert_eq!(twice.matches(r#"name="cover""#).count(), 1);
        assert_eq!(twice.matches("cover-image").count(), 1);
        assert_eq!(twice.matches(r#"id="delryn-cover""#).count(), 1);
        assert!(twice.contains(r#"href="delryn-cover.png""#));
    }

    #[test]
    fn patch_inserts_cover_when_none_existed() {
        let opf = r#"<package><metadata>
  <dc:title>X</dc:title>
</metadata><manifest>
  <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
</manifest></package>"#;
        let out = patch_opf(opf, "delryn-cover.jpg", "image/jpeg").unwrap();
        assert!(out.contains(r#"<meta name="cover" content="delryn-cover"/>"#));
        assert!(out.contains(r#"properties="cover-image""#));
    }

    #[test]
    fn patch_errors_on_malformed_opf() {
        assert!(patch_opf("<package>no sections</package>", "c.jpg", "image/jpeg").is_err());
    }
}
