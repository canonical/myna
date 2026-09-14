use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "build/blueprint_version.rs"]
mod blueprint_version;

const BLUEPRINTS: &[(&str, &str)] = &[
    ("active-backend-dialog.blp", "active-backend-dialog.ui"),
    ("apply-dialog.blp", "apply-dialog.ui"),
    ("backend-apply-controls.blp", "backend-apply-controls.ui"),
    ("backend-page.blp", "backend-page.ui"),
    ("diagnostics-page.blp", "diagnostics-page.ui"),
    ("main-window.blp", "main-window.ui"),
    ("myna-page.blp", "myna-page.ui"),
    ("onboarding-components.blp", "onboarding-components.ui"),
    ("onboarding-shortcut.blp", "onboarding-shortcut.ui"),
    ("onboarding-welcome.blp", "onboarding-welcome.ui"),
    ("onboarding-window.blp", "onboarding-window.ui"),
    ("operation-error-dialog.blp", "operation-error-dialog.ui"),
    ("sidebar-row.blp", "sidebar-row.ui"),
    ("status-page.blp", "status-page.ui"),
];

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let data_dir = manifest_dir.join("data");
    let typelib_paths = typelib_paths();

    println!(
        "cargo:rerun-if-changed={}",
        data_dir.join("myna-config.gresource.xml").display()
    );
    for (source, _) in BLUEPRINTS {
        println!("cargo:rerun-if-changed={}", data_dir.join(source).display());
    }

    let compiler = find_blueprint_compiler();
    for (source, output) in BLUEPRINTS {
        compile_blueprint(
            &compiler,
            &data_dir,
            &out_dir,
            &typelib_paths,
            source,
            output,
        );
    }

    glib_build_tools::compile_resources(
        &[out_dir.as_path(), data_dir.as_path()],
        "data/myna-config.gresource.xml",
        "myna-config.gresource",
    );
}

fn find_blueprint_compiler() -> PathBuf {
    match Command::new("blueprint-compiler").arg("--version").output() {
        Ok(output) if output.status.success() => {
            if let Err(error) = blueprint_version::check(&String::from_utf8_lossy(&output.stdout)) {
                fail("unsupported blueprint-compiler", &error);
            }
            PathBuf::from("blueprint-compiler")
        }
        Ok(output) => fail(
            "blueprint-compiler was found but did not run successfully",
            String::from_utf8_lossy(&output.stderr).trim(),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let (major, minor, patch) = blueprint_version::MINIMUM;
            fail(
                "blueprint-compiler is required to build myna-config",
                &format!(
                    "install blueprint-compiler {major}.{minor}.{patch} or newer and ensure it is on PATH"
                ),
            )
        }
        Err(error) => fail("could not launch blueprint-compiler", &error.to_string()),
    }
}

fn compile_blueprint(
    compiler: &Path,
    data_dir: &Path,
    out_dir: &Path,
    typelib_paths: &[PathBuf],
    source: &str,
    output: &str,
) {
    let source_path = data_dir.join(source);
    let output_path = out_dir.join(output);
    let candidate_path = out_dir.join(format!("{output}.candidate"));
    let mut command = Command::new(compiler);
    command.arg("compile").arg("--output").arg(&candidate_path);
    for path in typelib_paths {
        command.arg("--typelib-path").arg(path);
    }
    command.arg(&source_path);

    let compiler_output = command.output().unwrap_or_else(|error| {
        fail(
            &format!("failed to run blueprint-compiler for {source}"),
            &error.to_string(),
        )
    });
    if !compiler_output.status.success() {
        let _ = fs::remove_file(&candidate_path);
        let stderr = String::from_utf8_lossy(&compiler_output.stderr);
        fail(
            &format!("failed to compile {source} into {}", output_path.display()),
            &format!(
                "ensure Gtk 4 and libadwaita typelibs are installed (for example libgtk-4-dev/libadwaita-1-dev or gir1.2-gtk-4.0/gir1.2-adw-1), and that GI_TYPELIB_PATH includes non-standard locations. blueprint-compiler stderr:\n{}",
                stderr.trim()
            ),
        );
    }
    let candidate = fs::read(&candidate_path).unwrap_or_else(|error| {
        fail(
            &format!("could not read generated {output}"),
            &error.to_string(),
        )
    });
    if fs::read(&output_path).ok().as_deref() != Some(candidate.as_slice()) {
        fs::write(&output_path, candidate).unwrap_or_else(|error| {
            fail(
                &format!("could not write generated {}", output_path.display()),
                &error.to_string(),
            )
        });
    }
    fs::remove_file(&candidate_path).unwrap_or_else(|error| {
        fail(
            &format!("could not remove {}", candidate_path.display()),
            &error.to_string(),
        )
    });
}

fn typelib_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(value) = env::var_os("GI_TYPELIB_PATH") {
        paths.extend(env::split_paths(&value));
    }
    if let Ok(output) = Command::new("pkg-config")
        .args(["--variable=typelibdir", "gobject-introspection-1.0"])
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !path.is_empty() {
                paths.push(PathBuf::from(path));
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn fail(summary: &str, detail: &str) -> ! {
    panic!("{summary}. {detail}");
}
