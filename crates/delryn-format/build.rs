//! Optionally compiles a `libpdfium` into the binary.
//!
//! delryn loads PDFium at runtime, which normally means the shared library has to
//! travel with the executable. That is fine while the release tarball stays
//! intact, but the first thing many people do is copy the `delryn` binary onto
//! their `PATH` and delete the folder — at which point PDFs stop opening with no
//! obvious cause. Embedding the library gives the shipped binary a fallback copy
//! it can always fall back on, so it keeps working wherever it is moved.
//!
//! Set `DELRYN_PDFIUM_LIB` to a library file to embed it (the release workflow
//! points it at the checksum-verified download). Unset — the ordinary
//! `cargo build` — embeds nothing and costs nothing: the constant is an empty
//! slice and the runtime falls back to looking for a library on disk.
//!
//! Upstream ships no static build (every asset is a dynamic `.tgz`), so linking
//! PDFium into the executable properly would mean building it from source with
//! depot_tools. Embedding the bytes is the practical equivalent.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-env-changed=DELRYN_PDFIUM_LIB");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("cargo always sets OUT_DIR"));
    let dest = out_dir.join("pdfium-embedded.bin");

    match env::var_os("DELRYN_PDFIUM_LIB").filter(|v| !v.is_empty()) {
        Some(src) => {
            let src = PathBuf::from(src);
            println!("cargo::rerun-if-changed={}", src.display());
            // A path that was set but can't be read is a build misconfiguration —
            // failing here beats shipping a binary that silently has no PDF
            // support because a release step moved a file.
            let bytes = fs::read(&src).unwrap_or_else(|e| {
                panic!(
                    "DELRYN_PDFIUM_LIB is set to {} but it could not be read: {e}",
                    src.display()
                )
            });
            assert!(
                !bytes.is_empty(),
                "DELRYN_PDFIUM_LIB points at an empty file: {}",
                src.display()
            );
            fs::write(&dest, bytes).expect("writing the embedded library into OUT_DIR");
        }
        None => {
            // `include_bytes!` needs the file to exist either way; empty means
            // "nothing embedded" and the runtime checks for exactly that.
            fs::write(&dest, []).expect("writing the empty embed placeholder");
        }
    }
}
