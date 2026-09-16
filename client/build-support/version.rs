// The version the client binaries report: the one the snap they ship in
// carries (dev/snap-version.sh). Included by the build scripts that emit it as
// MYNA_VERSION.

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Resolved {
    pub version: String,
    // Every path exists: cargo reruns a build script on every build while a
    // watched path is missing.
    pub watch: Vec<PathBuf>,
}

// `workspace` is the Cargo workspace root, `fallback` the Cargo version.
pub fn resolve(workspace: &Path, fallback: &str) -> Resolved {
    // A snap build instance has no .git, so dev/prepare.sh stages the version.
    let staged = workspace.join(".snap-version");
    if let Ok(version) = std::fs::read_to_string(&staged) {
        return Resolved {
            version: version.trim().to_owned(),
            watch: vec![staged],
        };
    }
    from_checkout(workspace).unwrap_or_else(|| Resolved {
        version: fallback.to_owned(),
        watch: Vec::new(),
    })
}

// Only HEAD and the refs are watched, so -dirty is never reported: watching
// the working tree would rebuild on every edit.
fn from_checkout(workspace: &Path) -> Option<Resolved> {
    let script = workspace.join("../dev/snap-version.sh");
    let version = stdout(&mut Command::new(&script))?;
    let mut watch = vec![script];
    for name in ["HEAD", "packed-refs", "refs/heads", "refs/tags"] {
        let path = PathBuf::from(stdout(
            Command::new("git").arg("-C").arg(workspace).args([
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                name,
            ]),
        )?);
        if path.exists() {
            watch.push(path);
        }
    }
    Some(Resolved { version, watch })
}

fn stdout(command: &mut Command) -> Option<String> {
    let output = command.output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

#[allow(dead_code)] // Tests include this file for `resolve` alone.
pub fn emit() {
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let fallback = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let resolved = resolve(&manifest.join(".."), &fallback);
    for path in &resolved.watch {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rustc-env=MYNA_VERSION={}", resolved.version);
}
