use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use gio::glib;
use myna_config::adapters::client_settings::GioClientSettings;
use myna_config::app::{smoke_build, WidgetKind};
use myna_config::domain::{ClientSettingValue, SettingRange};
use myna_config::myna_settings::{MynaSettingsController, PersistenceWriter};
use myna_config::ports::{ClientSettings, ClientSettingsError};

const SCHEMA_ID: &str = "com.canonical.Myna.Dictation";
static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestFiles(PathBuf);

impl TestFiles {
    fn new(label: &str) -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/myna-config-tests")
            .join(format!(
                "{}-{}-{}",
                label,
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestFiles {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn real_schema_source(files: &TestFiles) -> gio::SettingsSchemaSource {
    let schema_dir = files.0.join("schemas");
    std::fs::create_dir_all(&schema_dir).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../data/glib-2.0/schemas/com.canonical.Myna.Dictation.gschema.xml");
    std::fs::copy(
        source,
        schema_dir.join("com.canonical.Myna.Dictation.gschema.xml"),
    )
    .unwrap();
    assert!(std::process::Command::new("glib-compile-schemas")
        .arg(&schema_dir)
        .status()
        .unwrap()
        .success());
    gio::SettingsSchemaSource::from_directory(&schema_dir, None, false).unwrap()
}

fn open_adapter(files: &TestFiles) -> GioClientSettings {
    GioClientSettings::open_with_source(
        &real_schema_source(files),
        files.0.join("settings/keyfile"),
    )
    .unwrap()
}

#[test]
fn enumerates_the_real_schema_and_its_metadata() {
    let files = TestFiles::new("metadata");
    let source = real_schema_source(&files);
    let expected = source.lookup(SCHEMA_ID, false).unwrap().list_keys();
    let adapter =
        GioClientSettings::open_with_source(&source, files.0.join("settings/keyfile")).unwrap();

    let settings = adapter.list().unwrap();
    assert_eq!(settings.len(), expected.len());
    assert!(expected
        .iter()
        .all(|key| settings.iter().any(|setting| setting.key().as_str() == key)));

    let streaming = settings
        .iter()
        .find(|setting| setting.key().as_str() == "streaming-mode")
        .unwrap();
    assert_eq!(streaming.summary(), Some("When to transcribe"));
    assert!(streaming.description().unwrap().contains("partial text"));
    assert_eq!(
        streaming.default_value(),
        &ClientSettingValue::Choice("auto".into())
    );
    assert_eq!(
        streaming.range(),
        &SettingRange::Choices(vec!["auto".into(), "streaming".into(), "batch".into()])
    );
    assert_eq!(streaming.current_value(), streaming.default_value());
    assert!(streaming.writable());
}

#[test]
fn every_real_schema_key_round_trips_in_the_private_keyfile() {
    let files = TestFiles::new("round-trip");
    let adapter = open_adapter(&files);

    for metadata in adapter.list().unwrap() {
        let value = match metadata.range() {
            SettingRange::Choices(choices) => ClientSettingValue::Choice(
                choices
                    .iter()
                    .find(|choice| Some(choice.as_str()) != metadata.default_value().as_str())
                    .unwrap_or(&choices[0])
                    .clone(),
            ),
            SettingRange::Unrestricted => match metadata.default_value() {
                ClientSettingValue::Boolean(default) => ClientSettingValue::Boolean(!default),
                _ => ClientSettingValue::Text("round trip".into()),
            },
            SettingRange::Range { minimum, .. } => {
                let floor = minimum.as_integer().expect("integer range");
                let default = metadata
                    .default_value()
                    .as_integer()
                    .expect("integer default");
                ClientSettingValue::Integer(if default == floor { floor + 1 } else { floor })
            }
        };
        adapter.set(metadata.key().as_str(), value.clone()).unwrap();
        assert_eq!(adapter.get(metadata.key().as_str()).unwrap(), value);

        let reopened = open_adapter(&files);
        assert_eq!(reopened.get(metadata.key().as_str()).unwrap(), value);
    }
}

#[test]
fn concurrent_different_key_edits_share_one_writer_and_both_persist() {
    let files = TestFiles::new("concurrent-writes");
    let schema_dir = files.0.join("schemas");
    let keyfile = files.0.join("settings/keyfile");
    let controller = MynaSettingsController::load(std::rc::Rc::new(
        GioClientSettings::open_with_source(&real_schema_source(&files), &keyfile).unwrap(),
    ));
    let writer = PersistenceWriter::spawn({
        let schema_dir = schema_dir.clone();
        let keyfile = keyfile.clone();
        move || {
            let source =
                gio::SettingsSchemaSource::from_directory(&schema_dir, None, false).unwrap();
            GioClientSettings::open_with_source(&source, &keyfile)
        }
    });

    let streaming = controller
        .set("streaming-mode", ClientSettingValue::Choice("batch".into()))
        .unwrap();
    let hud = controller
        .set("hud-style", ClientSettingValue::Choice("ribbon".into()))
        .unwrap();
    let streaming_request = streaming.clone();
    let hud_request = hud.clone();
    let (streaming_job, hud_job) = std::thread::scope(|scope| {
        let streaming_writer = writer.clone();
        let hud_writer = writer.clone();
        let streaming_job =
            scope.spawn(move || streaming_writer.submit(streaming_request).unwrap());
        let hud_job = scope.spawn(move || hud_writer.submit(hud_request).unwrap());
        (streaming_job.join().unwrap(), hud_job.join().unwrap())
    });

    controller.complete(streaming, streaming_job.wait());
    controller.complete(hud, hud_job.wait());
    drop(writer);

    let reopened =
        GioClientSettings::open_with_source(&real_schema_source(&files), &keyfile).unwrap();
    assert_eq!(
        reopened.get("streaming-mode").unwrap(),
        ClientSettingValue::Choice("batch".into())
    );
    assert_eq!(
        reopened.get("hud-style").unwrap(),
        ClientSettingValue::Choice("ribbon".into())
    );
}

#[test]
fn schema_validation_rejects_wrong_types_and_values() {
    let files = TestFiles::new("validation");
    let adapter = open_adapter(&files);

    assert!(matches!(
        adapter.set(
            "streaming-mode",
            ClientSettingValue::Choice("impossible".into())
        ),
        Err(ClientSettingsError::InvalidValue { .. })
    ));
    assert!(matches!(
        adapter.set("streaming-mode", ClientSettingValue::Text("batch".into())),
        Err(ClientSettingsError::InvalidValue { .. })
    ));
    assert!(matches!(
        adapter.get("not-in-the-schema"),
        Err(ClientSettingsError::UnknownKey { .. })
    ));
}

/// A private thread-default context: the global default one also holds the
/// keyfile monitors of every other test thread, and dispatching those here
/// reloads their backends mid-write.
#[test]
fn external_keyfile_changes_are_notified_live() {
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            let files = TestFiles::new("notifications");
            let source = real_schema_source(&files);
            let path = files.0.join("settings/keyfile");
            let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
            let reader = GioClientSettings::open_with_source(&source, &path).unwrap();
            let _subscription = reader
                .subscribe({
                    let observed = Arc::clone(&observed);
                    Box::new(move |change| observed.lock().unwrap().push(change))
                })
                .unwrap();
            let writer = GioClientSettings::open_with_source(&source, &path).unwrap();

            writer
                .set("streaming-mode", ClientSettingValue::Choice("batch".into()))
                .unwrap();

            let deadline = Instant::now() + Duration::from_secs(5);
            while observed.lock().unwrap().is_empty() && Instant::now() < deadline {
                while context.iteration(false) {}
                std::thread::sleep(Duration::from_millis(10));
            }
            let observed = observed.lock().unwrap();
            assert!(observed.iter().any(|change| {
                change.key().as_str() == "streaming-mode"
                    && change.value() == &ClientSettingValue::Choice("batch".into())
            }));
        })
        .unwrap();
}

#[test]
fn missing_schema_is_explicit_and_actionable() {
    let files = TestFiles::new("missing-schema");
    let schema_dir = files.0.join("empty-schemas");
    std::fs::create_dir_all(&schema_dir).unwrap();
    std::fs::write(
        schema_dir.join("unrelated.gschema.xml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<schemalist>
  <schema id="com.example.Unrelated" path="/com/example/unrelated/">
    <key name="value" type="s"><default>''</default></key>
  </schema>
</schemalist>
"#,
    )
    .unwrap();
    assert!(std::process::Command::new("glib-compile-schemas")
        .arg(&schema_dir)
        .status()
        .unwrap()
        .success());
    let source = gio::SettingsSchemaSource::from_directory(&schema_dir, None, false).unwrap();

    let error =
        GioClientSettings::open_with_source(&source, files.0.join("settings/keyfile")).unwrap_err();
    assert!(matches!(
        error,
        ClientSettingsError::SchemaUnavailable {
            schema_id: SCHEMA_ID,
            ..
        }
    ));
    assert!(error.to_string().contains("install"));
    assert!(error.to_string().contains("restart"));
}

/// The schema declares `u`; a signed `i` key takes the other branch of the
/// width conversion, and a width the page does not hold stays refused.
#[test]
fn signed_integer_keys_round_trip_and_wider_ones_are_refused() {
    let files = TestFiles::new("signed-integer");
    let schema_dir = files.0.join("schemas");
    std::fs::create_dir_all(&schema_dir).unwrap();
    std::fs::write(
        schema_dir.join("com.canonical.Myna.Dictation.gschema.xml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<schemalist>
  <schema id="com.canonical.Myna.Dictation" path="/com/canonical/myna/dictation/">
    <key name="offset" type="i"><default>-2</default><range min="-10" max="10"/></key>
    <key name="wide" type="x"><default>7</default></key>
  </schema>
</schemalist>
"#,
    )
    .unwrap();
    assert!(std::process::Command::new("glib-compile-schemas")
        .arg(&schema_dir)
        .status()
        .unwrap()
        .success());
    let source = gio::SettingsSchemaSource::from_directory(&schema_dir, None, false).unwrap();
    let adapter =
        GioClientSettings::open_with_source(&source, files.0.join("settings/keyfile")).unwrap();

    assert_eq!(
        adapter.get("offset").unwrap(),
        ClientSettingValue::Integer(-2)
    );
    adapter
        .set("offset", ClientSettingValue::Integer(-10))
        .unwrap();
    assert_eq!(
        adapter.get("offset").unwrap(),
        ClientSettingValue::Integer(-10)
    );
    assert!(matches!(
        adapter.set("offset", ClientSettingValue::Integer(11)),
        Err(ClientSettingsError::InvalidValue { .. })
    ));
    assert!(matches!(
        adapter.get("wide"),
        Err(ClientSettingsError::InvalidValue { .. })
    ));
    assert!(matches!(
        adapter.set("wide", ClientSettingValue::Integer(1)),
        Err(ClientSettingsError::InvalidValue { .. })
    ));

    assert_eq!(ClientSettingValue::Integer(3).as_str(), None);
    assert_eq!(ClientSettingValue::Text("3".into()).as_integer(), None);
}

#[test]
fn explicit_backend_does_not_change_process_settings_environment() {
    let files = TestFiles::new("environment");
    let backend = std::env::var_os("GSETTINGS_BACKEND");
    let config_home = std::env::var_os("XDG_CONFIG_HOME");

    let _adapter = open_adapter(&files);

    assert_eq!(std::env::var_os("GSETTINGS_BACKEND"), backend);
    assert_eq!(std::env::var_os("XDG_CONFIG_HOME"), config_home);
}

#[test]
fn non_writable_keys_reject_set_without_claiming_success() {
    let files = TestFiles::new("non-writable");
    let source = real_schema_source(&files);
    let backend = gio::functions::null_settings_backend_new();
    let adapter = GioClientSettings::open_with_backend(&source, &backend).unwrap();
    let key = "streaming-mode";
    let original = adapter.get(key).unwrap();

    assert!(matches!(
        adapter.set(key, ClientSettingValue::Choice("batch".into())),
        Err(ClientSettingsError::NotWritable { key: failed_key }) if failed_key == key
    ));
    assert_eq!(adapter.get(key).unwrap(), original);
}

#[test]
fn headless_widget_smoke_covers_every_real_schema_key() {
    let files = TestFiles::new("widget-smoke");
    let adapter = open_adapter(&files);

    let plans = smoke_build(std::rc::Rc::new(adapter)).unwrap();

    assert_eq!(plans.len(), 5);
    assert!(plans.iter().any(|plan| plan.kind == WidgetKind::Choice));
    assert!(plans.iter().any(|plan| plan.kind == WidgetKind::Text));
    assert!(plans.iter().any(|plan| plan.kind == WidgetKind::Toggle));
    let number = plans
        .iter()
        .find(|plan| plan.kind == WidgetKind::Number)
        .expect("silence-timeout is a bounded integer");
    assert_eq!(number.key, "silence-timeout");
    assert_eq!(number.bounds, Some((0, 600)));
    assert!(plans.iter().all(|plan| !plan.description.is_empty()));
}

#[test]
fn the_silence_timeout_is_a_bounded_integer_that_rejects_values_outside_its_range() {
    let files = TestFiles::new("silence-timeout");
    let adapter = open_adapter(&files);

    assert_eq!(
        adapter.get("silence-timeout").unwrap(),
        ClientSettingValue::Integer(30)
    );
    adapter
        .set("silence-timeout", ClientSettingValue::Integer(0))
        .unwrap();
    assert_eq!(
        open_adapter(&files).get("silence-timeout").unwrap(),
        ClientSettingValue::Integer(0)
    );
    for outside in [-1, 601] {
        assert!(matches!(
            adapter.set("silence-timeout", ClientSettingValue::Integer(outside)),
            Err(ClientSettingsError::InvalidValue { key, .. }) if key == "silence-timeout"
        ));
    }
    assert!(matches!(
        adapter.set("silence-timeout", ClientSettingValue::Text("30".into())),
        Err(ClientSettingsError::InvalidValue { key, .. }) if key == "silence-timeout"
    ));
    assert_eq!(
        adapter.get("silence-timeout").unwrap(),
        ClientSettingValue::Integer(0)
    );
}

#[test]
fn the_chimes_enabled_flag_is_a_boolean_that_rejects_the_wrong_type() {
    let files = TestFiles::new("chimes-enabled");
    let adapter = open_adapter(&files);

    assert_eq!(
        adapter.get("chimes-enabled").unwrap(),
        ClientSettingValue::Boolean(true)
    );
    adapter
        .set("chimes-enabled", ClientSettingValue::Boolean(false))
        .unwrap();
    assert_eq!(
        open_adapter(&files).get("chimes-enabled").unwrap(),
        ClientSettingValue::Boolean(false)
    );
    assert!(matches!(
        adapter.set("chimes-enabled", ClientSettingValue::Text("false".into())),
        Err(ClientSettingsError::InvalidValue { key, .. }) if key == "chimes-enabled"
    ));
    assert_eq!(
        adapter.get("chimes-enabled").unwrap(),
        ClientSettingValue::Boolean(false)
    );
}
