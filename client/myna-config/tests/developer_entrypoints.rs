use std::fs;
use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn assert_no_retired_prototype_reference(path: &Path, needle: &str) {
    if path.is_dir() {
        for entry in fs::read_dir(path).expect("read documentation directory") {
            let entry = entry.expect("read documentation entry");
            assert_no_retired_prototype_reference(&entry.path(), needle);
        }
    } else if matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("md" | "py" | "sh")
    ) || path.file_name().is_some_and(|name| name == "Makefile")
    {
        let contents = fs::read_to_string(path).expect("read documentation or executable");
        assert!(
            !contents.contains(needle),
            "{} still references the retired prototype",
            path.display()
        );
    }
}

#[test]
fn retired_tk_prototype_has_no_live_entrypoint() {
    let root = repository_root();
    let retired_name = ["config", "ui.py"].join("-");
    assert!(!root.join("dev").join(&retired_name).exists());
    for path in ["README.md", "Makefile", "docs", "dev", "config-ui"] {
        assert_no_retired_prototype_reference(&root.join(path), &retired_name);
    }

    let readme = fs::read_to_string(root.join("README.md")).expect("repository README");
    let makefile = fs::read_to_string(root.join("Makefile")).expect("repository Makefile");

    assert!(readme.contains("cargo run -p myna-config"));
    assert!(readme.contains("shipping `myna.config` wrapper"));
    assert!(readme.contains("until that dependency is approved and available"));
    assert!(readme.contains("config-ui/confinement.md"));
    assert!(makefile.contains("run-settings: ## Launch the native Myna configuration UI"));
    assert!(makefile.contains("cd client && cargo run -p myna-config"));
}
