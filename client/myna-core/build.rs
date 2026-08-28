//! Compile the GSettings schema into `OUT_DIR` for the settings tests.
//!
//! `Settings` reads `com.canonical.Myna.Dictation` out of whatever schema source the
//! machine has installed; the tests must not depend on that, so they build
//! their own source from a directory this compiles here. Shipping is a
//! separate matter: the snap compiles the same XML into
//! `$SNAP/usr/share/glib-2.0/schemas`, and `make install-schema` puts it on the
//! host - see `client/data/glib-2.0/schemas/`.
//!
//! `glib-compile-schemas` comes from `libglib2.0-bin`, which `libglib2.0-dev`
//! depends on - and gio-sys already needs the -dev package to link at all, so
//! anything that can build this crate can compile the schema. A missing
//! compiler is therefore a broken environment, not a reason to skip.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let source = manifest.join("../data/glib-2.0/schemas");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("schemas");

    std::fs::create_dir_all(&out).expect("create schema dir");
    let mut compiled_any = false;
    for entry in std::fs::read_dir(&source).expect("read schema source dir") {
        let path = entry.expect("schema dir entry").path();
        if path.extension().is_some_and(|e| e == "xml") {
            println!("cargo:rerun-if-changed={}", path.display());
            let name = path.file_name().expect("schema file name");
            std::fs::copy(&path, out.join(name)).expect("stage schema");
            compiled_any = true;
        }
    }
    assert!(compiled_any, "no schemas found in {}", source.display());

    let status = Command::new("glib-compile-schemas")
        .arg(&out)
        .status()
        .expect("glib-compile-schemas (install libglib2.0-dev)");
    assert!(status.success(), "glib-compile-schemas failed");

    println!("cargo:rustc-env=MYNA_TEST_SCHEMA_DIR={}", out.display());
}
