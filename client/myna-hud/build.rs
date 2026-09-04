fn main() {
    println!("cargo:rustc-check-cfg=cfg(dev_lab)");
    println!("cargo:rustc-check-cfg=cfg(coverage)");
    // Coverage builds instrument with `--cfg coverage`; the lab is dev-only
    // and would otherwise count as uncovered changed lines in patch-coverage
    // (T171). Disable it when coverage is active.
    if std::env::var("CARGO_CFG_COVERAGE").is_ok() {
        return;
    }
    let profile = std::env::var("PROFILE").unwrap_or_default();
    let has_feature = std::env::var("CARGO_FEATURE_DEV_LAB").is_ok();
    if profile == "debug" || has_feature {
        println!("cargo:rustc-cfg=dev_lab");
    }
}
