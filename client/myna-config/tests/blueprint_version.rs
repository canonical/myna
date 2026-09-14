#[path = "../build/blueprint_version.rs"]
mod blueprint_version;

use blueprint_version::{check, MINIMUM};

#[test]
fn minimum_is_the_oldest_release_that_compiles_the_templates_identically() {
    assert_eq!(MINIMUM, (0, 16, 0));
}

#[test]
fn accepts_the_minimum_and_every_newer_release() {
    for output in [
        "0.16.0\n", "0.16.1", "0.19.0\n", "0.20.4\n", "0.22.2", "1.0.0",
    ] {
        assert_eq!(check(output), Ok(()), "{output:?}");
    }
}

#[test]
fn rejects_releases_below_the_minimum_naming_both_versions() {
    for (output, found) in [
        ("0.12.0\n", "0.12.0"),
        ("0.14.0", "0.14.0"),
        ("0.15.99", "0.15.99"),
    ] {
        let error = check(output).expect_err(output);
        assert!(error.contains(found), "{error}");
        assert!(error.contains("0.16.0"), "{error}");
    }
}

#[test]
fn rejects_output_that_is_not_a_release_version() {
    for output in [
        "uninstalled\n",
        "",
        "0.20",
        "0.20.x",
        "0.20.4.1",
        "blueprint-compiler 0.20.4",
    ] {
        let error = check(output).expect_err(output);
        assert!(error.contains("0.16.0"), "{error}");
        assert!(error.contains(output.trim()), "{error}");
    }
}
