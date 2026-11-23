//! The migration reads the one settings store every process shares, found
//! through the environment, so this test owns the process it sets it in.

#[test]
fn the_shared_store_is_migrated_where_the_environment_puts_it() {
    let dir = std::env::temp_dir().join(format!("myna-legacy-store-{}", std::process::id()));
    let settings = dir.join("glib-2.0/settings");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::write(
        settings.join("keyfile"),
        "[com/canonical/myna/dictation]\nprocessing-chain='keyword'\nsounds=false\n",
    )
    .unwrap();
    std::env::set_var("GSETTINGS_BACKEND", "keyfile");
    std::env::set_var("XDG_CONFIG_HOME", &dir);

    assert!(myna_core::processing::migrate_legacy_store());
    let text = std::fs::read_to_string(settings.join("keyfile")).unwrap();
    assert!(text.contains("fit-to-field=true"), "{text}");
    assert!(!text.contains("processing-steps"), "{text}");
    assert!(text.contains("sounds=false"), "{text}");
    assert!(!text.contains("processing-chain"), "{text}");
    assert!(!myna_core::processing::migrate_legacy_store());
    std::fs::remove_dir_all(&dir).unwrap();
}
