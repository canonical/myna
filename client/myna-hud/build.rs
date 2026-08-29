fn main() {
    println!("cargo:rustc-check-cfg=cfg(dev_lab)");
    let profile = std::env::var("PROFILE").unwrap_or_default();
    let has_feature = std::env::var("CARGO_FEATURE_DEV_LAB").is_ok();
    if profile == "debug" || has_feature {
        println!("cargo:rustc-cfg=dev_lab");
    }
}
