use std::fs;
use std::path::{Path, PathBuf};

const BLUEPRINTS: &[(&str, &str)] = &[
    ("active-backend-dialog.blp", "active-backend-dialog.ui"),
    ("apply-dialog.blp", "apply-dialog.ui"),
    ("backend-apply-controls.blp", "backend-apply-controls.ui"),
    ("backend-page.blp", "backend-page.ui"),
    ("diagnostics-page.blp", "diagnostics-page.ui"),
    ("main-window.blp", "main-window.ui"),
    ("myna-page.blp", "myna-page.ui"),
    ("sidebar-row.blp", "sidebar-row.ui"),
    ("status-page.blp", "status-page.ui"),
];

#[test]
fn active_backend_selector_and_confirmation_are_static_blueprint_shells() {
    let root = crate_root();
    let page = fs::read_to_string(root.join("data/myna-page.blp")).unwrap();
    let dialog = fs::read_to_string(root.join("data/active-backend-dialog.blp")).unwrap();
    let apply_controls = fs::read_to_string(root.join("data/backend-apply-controls.blp")).unwrap();
    assert!(page.contains("active_backend_group"));
    assert!(page.contains("active_backend_row"));
    assert!(page.contains("switch_backend_button"));
    assert!(page.contains("sensitive: false;"));
    assert!(apply_controls
        .contains("apply_button {\n      label: _(\"Apply\");\n      sensitive: false;"));
    assert!(dialog.contains("switch_confirmation_label"));
    assert!(dialog.contains("Cancel"));
    assert!(dialog.contains("Switch Backend"));
}

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn cargo_build_pipeline_has_an_explicit_sorted_blueprint_contract() {
    let root = crate_root();
    let build = fs::read_to_string(root.join("build.rs")).expect("myna-config needs build.rs");
    let manifest = fs::read_to_string(root.join("data/myna-config.gresource.xml"))
        .expect("myna-config needs one GResource manifest");

    let mut previous = "";
    for (source, output) in BLUEPRINTS {
        assert!(previous < *source, "Blueprint source list must stay sorted");
        previous = source;
        assert!(
            root.join("data").join(source).is_file(),
            "missing Blueprint source {source}"
        );
        assert!(
            build.contains(source) && build.contains(output),
            "build.rs must explicitly map {source} to {output}"
        );
        assert!(
            manifest.contains(&format!("<file>{output}</file>")),
            "resource manifest must embed {output}"
        );
        assert!(
            !root.join("data").join(output).exists(),
            "generated UI must remain in OUT_DIR: {output}"
        );
    }

    assert!(build.contains("blueprint-compiler"));
    assert!(build.contains("compile_resources"));
    assert!(build.contains("cargo:rerun-if-changed"));
}

#[test]
fn templates_use_the_single_application_resource_prefix() {
    let manifest = fs::read_to_string(crate_root().join("data/myna-config.gresource.xml")).unwrap();
    assert!(manifest.contains("prefix=\"/com/canonical/Myna/Config/ui\""));
}

#[test]
fn diagnostics_accessibility_metadata_lives_in_the_production_template() {
    let template = fs::read_to_string(crate_root().join("data/diagnostics-page.blp")).unwrap();
    for metadata in [
        "label: _(\"Refresh diagnostics\")",
        "label: _(\"Copy diagnostics\")",
        "label: _(\"Diagnostic report\")",
        "description: _(",
    ] {
        assert!(
            template.contains(metadata),
            "missing production accessibility metadata: {metadata}"
        );
    }
}
