// Not `#[path]`: a path through tests/ falls under cargo-llvm-cov's default
// ignore and the module vanishes from coverage.
mod version {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../build-support/version.rs"
    ));
}

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use version::resolve;

/// The checkout. `MYNA_REPO_ROOT` names it when `client/` runs as a copy of
/// its own, which is how cargo-mutants builds.
fn script() -> PathBuf {
    std::env::var_os("MYNA_REPO_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join("dev/snap-version.sh")
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("myna-client-version-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("client")).unwrap();
    dir
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn checkout(name: &str) -> (PathBuf, String) {
    let repo = scratch(name);
    fs::create_dir_all(repo.join("dev")).unwrap();
    fs::copy(script(), repo.join("dev/snap-version.sh")).unwrap();
    fs::write(repo.join("client/Cargo.toml"), "").unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    let sha = git(&repo, &["rev-parse", "--short", "HEAD"]);
    (repo, sha)
}

#[test]
fn a_staged_snap_version_is_reported_verbatim() {
    let dir = scratch("staged");
    let staged = dir.join("client/.snap-version");
    fs::write(&staged, "0+git.abc1234-dirty\n").unwrap();

    let resolved = resolve(&dir.join("client"), "0.1.0");

    assert_eq!(resolved.version, "0+git.abc1234-dirty");
    assert_eq!(resolved.watch, vec![staged]);
}

#[test]
fn the_staged_version_wins_over_the_checkout() {
    let (repo, _) = checkout("staged-over-git");
    fs::write(repo.join("client/.snap-version"), "0+git.fromsnap\n").unwrap();

    assert_eq!(
        resolve(&repo.join("client"), "0.1.0").version,
        "0+git.fromsnap"
    );
}

#[test]
fn a_checkout_reports_the_snap_version_of_head() {
    let (repo, sha) = checkout("git");

    let resolved = resolve(&repo.join("client"), "0.1.0");

    assert_eq!(resolved.version, format!("0+git.{sha}"));
}

#[test]
fn a_checkout_watches_head_and_the_refs_that_exist() {
    let (repo, _) = checkout("watch");

    let resolved = resolve(&repo.join("client"), "0.1.0");

    let git_dir = repo.join(".git").canonicalize().unwrap();
    for expected in [
        git_dir.join("HEAD"),
        git_dir.join("refs/heads"),
        git_dir.join("refs/tags"),
    ] {
        assert!(
            resolved.watch.contains(&expected),
            "{expected:?} not in {:?}",
            resolved.watch
        );
    }
    // Cargo reruns a build script on every build while a watched path is missing.
    for path in &resolved.watch {
        assert!(path.exists(), "{path:?} does not exist");
    }
}

#[test]
fn a_checkout_does_not_report_dirty() {
    let (repo, sha) = checkout("dirty");
    fs::write(repo.join("client/Cargo.toml"), "# edited").unwrap();

    assert_eq!(
        resolve(&repo.join("client"), "0.1.0").version,
        format!("0+git.{sha}")
    );
}

#[test]
fn without_a_staged_version_or_a_checkout_the_fallback_is_reported() {
    let dir = scratch("fallback");

    let resolved = resolve(&dir.join("client"), "0.1.0");

    assert_eq!(resolved.version, "0.1.0");
    assert!(resolved.watch.is_empty());
}

#[test]
fn myna_config_reports_the_resolved_version_not_the_cargo_one() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let output = Command::new(env!("CARGO_BIN_EXE_myna-config"))
        .arg("--version")
        .output()
        .unwrap();

    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!(
            "myna-config {}",
            resolve(&workspace, env!("CARGO_PKG_VERSION")).version
        )
    );
}
