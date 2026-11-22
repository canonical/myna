use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use gio::glib;
use myna_config::adapters::client_settings::GioClientSettings;
use myna_config::domain::{ClientSettingValue, SettingRange};
use myna_config::myna_settings::{
    choice_detail, choice_display_label, past_dictation, widget_plan, MynaSettingsController,
    PersistenceWriter, Placement, WidgetKind,
};
use myna_config::ports::{ClientSettings, ClientSettingsError};
use myna_core::history::Skip;

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

/// A throwaway schema under the real id, holding just `keys`.
fn fixture_schema_source(files: &TestFiles, keys: &str) -> gio::SettingsSchemaSource {
    let schema_dir = files.0.join("fixture-schemas");
    std::fs::create_dir_all(&schema_dir).unwrap();
    std::fs::write(
        schema_dir.join("com.canonical.Myna.Dictation.gschema.xml"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<schemalist>
  <schema id="com.canonical.Myna.Dictation" path="/com/canonical/myna/dictation/">
    {keys}
  </schema>
</schemalist>
"#
        ),
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

    // The page shows the chain and the Chinese script in place of the two
    // stored step keys.
    let stored = ["processing-steps", "processing-params"];
    let presented = ["processing-chain", "chinese-script"];
    let settings = adapter.list().unwrap();
    assert_eq!(settings.len(), expected.len());
    assert!(expected
        .iter()
        .filter(|key| !stored.contains(&key.as_str()))
        .all(|key| settings.iter().any(|setting| setting.key().as_str() == key)));
    assert!(presented.iter().all(|key| settings
        .iter()
        .any(|setting| setting.key().as_str() == *key)));
    assert!(!settings
        .iter()
        .any(|setting| stored.contains(&setting.key().as_str())));

    let streaming = settings
        .iter()
        .find(|setting| setting.key().as_str() == "streaming-mode")
        .unwrap();
    assert_eq!(streaming.summary(), Some("When to transcribe"));
    assert!(streaming.description().unwrap().contains("partial text"));
    assert_eq!(
        streaming.default_value(),
        &ClientSettingValue::Choice("streaming".into())
    );
    assert_eq!(
        streaming.range(),
        &SettingRange::Choices(vec!["streaming".into(), "batch".into()])
    );
    assert_eq!(streaming.current_value(), streaming.default_value());
    assert!(streaming.writable());
}

#[test]
fn a_value_equal_to_the_default_still_counts_as_the_users() {
    let files = TestFiles::new("user-value");
    let adapter = open_adapter(&files);
    assert!(!adapter.has_user_value("streaming-mode").unwrap());

    adapter
        .set(
            "streaming-mode",
            ClientSettingValue::Choice("streaming".into()),
        )
        .unwrap();
    assert!(adapter.has_user_value("streaming-mode").unwrap());
    assert!(open_adapter(&files)
        .has_user_value("streaming-mode")
        .unwrap());
    assert!(!adapter.has_user_value("hud-style").unwrap());
    assert!(matches!(
        adapter.has_user_value("no-such-key"),
        Err(ClientSettingsError::UnknownKey { .. })
    ));
}

#[test]
fn every_real_schema_key_round_trips_in_the_private_keyfile() {
    let files = TestFiles::new("round-trip");
    let adapter = open_adapter(&files);

    for metadata in adapter.list().unwrap() {
        let value = match (metadata.range(), metadata.default_value()) {
            (_, ClientSettingValue::Boolean(default)) => ClientSettingValue::Boolean(!default),
            (SettingRange::Choices(choices), _) => ClientSettingValue::Choice(
                choices
                    .iter()
                    .find(|choice| Some(choice.as_str()) != metadata.default_value().as_str())
                    .unwrap_or(&choices[0])
                    .clone(),
            ),
            (SettingRange::Unrestricted, ClientSettingValue::List(_)) => {
                ClientSettingValue::List(vec!["round".into(), "trip".into()])
            }
            (SettingRange::Unrestricted, _) => ClientSettingValue::Text("round trip".into()),
            (SettingRange::Range { minimum, .. }, _) => {
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
    let source = fixture_schema_source(
        &files,
        r#"<key name="offset" type="i"><default>-2</default><range min="-10" max="10"/></key>
    <key name="wide" type="x"><default>7</default></key>"#,
    );
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

/// One key of a type the page cannot hold must cost that row, not the page.
#[test]
fn listing_skips_keys_of_a_type_the_page_cannot_hold() {
    let files = TestFiles::new("unsupported-type");
    let source = fixture_schema_source(
        &files,
        r#"<key name="offset" type="i"><default>-2</default></key>
    <key name="on" type="b"><default>true</default></key>
    <key name="wide" type="x"><default>7</default></key>"#,
    );
    let adapter =
        GioClientSettings::open_with_source(&source, files.0.join("settings/keyfile")).unwrap();

    let keys: Vec<_> = adapter
        .list()
        .unwrap()
        .iter()
        .map(|setting| setting.key().as_str().to_owned())
        .collect();
    assert_eq!(keys, ["offset", "on"]);
}

#[test]
fn string_list_keys_round_trip_and_refuse_other_types() {
    let files = TestFiles::new("string-list");
    let source = fixture_schema_source(
        &files,
        r#"<key name="words" type="as"><default>['kernel']</default></key>
    <key name="name" type="s"><default>''</default></key>"#,
    );
    let open =
        || GioClientSettings::open_with_source(&source, files.0.join("settings/keyfile")).unwrap();
    let adapter = open();
    let words = |list: &[&str]| {
        ClientSettingValue::List(list.iter().map(|word| (*word).to_owned()).collect())
    };

    let listed = adapter.list().unwrap();
    let metadata = listed
        .iter()
        .find(|setting| setting.key().as_str() == "words")
        .unwrap();
    assert_eq!(metadata.default_value(), &words(&["kernel"]));
    assert_eq!(metadata.range(), &SettingRange::Unrestricted);

    adapter.set("words", words(&["Siobhan", "kernel"])).unwrap();
    assert_eq!(open().get("words").unwrap(), words(&["Siobhan", "kernel"]));
    adapter.set("words", words(&[])).unwrap();
    assert_eq!(open().get("words").unwrap(), words(&[]));

    assert!(matches!(
        adapter.set("words", ClientSettingValue::Text("kernel".into())),
        Err(ClientSettingsError::InvalidValue { .. })
    ));
    assert!(matches!(
        adapter.set("name", words(&["kernel"])),
        Err(ClientSettingsError::InvalidValue { .. })
    ));
    assert_eq!(words(&["a"]).as_list(), Some(&["a".to_owned()][..]));
    assert_eq!(words(&["a"]).as_str(), None);
    assert_eq!(words(&["a"]).as_integer(), None);
    assert_eq!(ClientSettingValue::Text("a".into()).as_list(), None);
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
fn a_past_dictation_shows_what_was_heard_only_when_it_differs() {
    let entry = |heard: &str, typed: &str| myna_core::history::Entry {
        time: 1,
        heard: heard.into(),
        typed: typed.into(),
        ..Default::default()
    };
    let changed = past_dictation(&entry("um so I think ", "So I think."));
    assert_eq!(changed.typed, "So I think.");
    assert_eq!(changed.detail, "Heard: um so I think");
    assert!(changed.made_by.is_empty(), "an older record says no more");
    let same = past_dictation(&entry("So I think. ", "So I think."));
    assert_eq!(same.detail, "Typed as heard");
}

#[test]
fn a_past_dictation_says_why_it_was_not_typed() {
    use myna_core::history::Outcome;
    for (outcome, said) in [
        (Outcome::FocusLost, "Not typed: focus left the field"),
        (Outcome::Refused, "Not typed: the field refused it"),
        (Outcome::Discarded, "Not typed: the dictation was cancelled"),
        (Outcome::Empty, "Nothing to type"),
    ] {
        let lost = myna_core::history::Entry {
            time: 1,
            heard: "hello".into(),
            outcome: Some(outcome),
            ..Default::default()
        };
        let shown = past_dictation(&lost);
        assert_eq!(shown.detail, said, "{outcome:?}");
        assert_eq!(shown.typed, "hello", "what was heard is what is left");
    }
}

#[test]
fn a_past_dictation_says_what_made_it() {
    let step =
        |task: &str, model: Option<&str>, ms, skipped: Option<Skip>| myna_core::history::Step {
            task: task.into(),
            model: model.map(Into::into),
            ms,
            skipped,
        };
    let entry = myna_core::history::Entry {
        time: 1,
        heard: "um bonjour".into(),
        typed: "Bonjour.".into(),
        chain: Some("tidy-french".into()),
        steps: vec![
            step("text.cleanup", Some("fdt-small"), 4, None),
            step("text.translate", None, 1000, Some(Skip::Timeout)),
            step("text.summarise", Some("gemma"), 7, None),
        ],
        speech_model: Some("parakeet-tdt".into()),
        version: Some("0.3.3+git22".into()),
        ..Default::default()
    };
    let made_by: Vec<(String, String)> = past_dictation(&entry).made_by;
    let pair = |label: &str, value: &str| (label.to_owned(), value.to_owned());
    assert_eq!(
        made_by,
        [
            pair("Speech model", "parakeet-tdt"),
            pair("Processing", "A French translation"),
            pair("Tidying", "fdt-small, 4 ms"),
            pair("Translation", "Not used (1000 ms): the model was too slow"),
            pair("text.summarise", "gemma, 7 ms"),
            pair("Myna version", "0.3.3+git22"),
        ]
    );
}

#[test]
fn a_past_dictation_shows_the_wait_after_the_stop() {
    let entry = myna_core::history::Entry {
        heard: "hi".into(),
        typed: "Hi.".into(),
        asr_tail_ms: Some(120),
        inject_ms: Some(3),
        dead_air_ms: Some(480),
        ..Default::default()
    };
    assert_eq!(
        past_dictation(&entry).made_by,
        [(
            "Wait after you stopped".to_owned(),
            "480 ms (120 ms to hear, 3 ms to type)".to_owned()
        )]
    );
    let older = myna_core::history::Entry::default();
    assert!(past_dictation(&older).made_by.is_empty());
}

#[test]
fn a_dictation_lost_with_the_focus_shows_what_can_be_recovered() {
    let entry = myna_core::history::Entry {
        heard: " um hello there ".into(),
        outcome: Some(myna_core::history::Outcome::FocusLost),
        ..Default::default()
    };
    let shown = past_dictation(&entry);
    assert_eq!(shown.typed, "um hello there");
    assert_eq!(shown.detail, "Not typed: focus left the field");
}

#[test]
fn every_reason_a_step_was_not_used_reads_differently() {
    use Skip::*;
    let all = [
        NoModel,
        Refused,
        Timeout,
        Empty,
        NewWord,
        TooManyDropped,
        LongDrop,
        DroppedNegation,
        Chatty,
        OddLength,
        Other,
    ];
    let shown: std::collections::HashSet<String> = all
        .into_iter()
        .map(|why| {
            let entry = myna_core::history::Entry {
                steps: vec![myna_core::history::Step {
                    task: "text.cleanup".into(),
                    skipped: Some(why),
                    ..Default::default()
                }],
                ..Default::default()
            };
            past_dictation(&entry).made_by[0].1.clone()
        })
        .collect();
    assert_eq!(shown.len(), all.len(), "{shown:?}");
}

#[test]
fn headless_widget_smoke_covers_every_real_schema_key() {
    let files = TestFiles::new("widget-smoke");
    let adapter = open_adapter(&files);

    let plans: Vec<_> = adapter.list().unwrap().iter().map(widget_plan).collect();

    assert_eq!(plans.len(), 10);
    assert!(plans.iter().any(|plan| plan.kind == WidgetKind::Choice));
    let vocabulary = plans
        .iter()
        .find(|plan| plan.key == "vocabulary")
        .expect("vocabulary is listed");
    assert_eq!(vocabulary.kind, WidgetKind::List);
    assert!(vocabulary.limits.is_some());
    let sound_set = plans
        .iter()
        .find(|plan| plan.key == "sound-set")
        .expect("sound-set is listed");
    assert_eq!(sound_set.kind, WidgetKind::Choice);
    assert_eq!(sound_set.title, "Sound style");
    assert_eq!(
        sound_set.choices,
        ["tine", "myna", "hum", "marimba", "drop", "radio", "koto"]
    );
    let switch = plans
        .iter()
        .find(|plan| plan.key == "sounds")
        .expect("sounds is listed");
    assert_eq!(switch.kind, WidgetKind::Switch);
    assert_eq!(switch.title, "Sounds on start, stop and error");
    let chain = plans
        .iter()
        .find(|plan| plan.key == "processing-chain")
        .expect("processing-chain is listed");
    assert_eq!(chain.kind, WidgetKind::Options);
    assert_eq!(chain.placement, Placement::Processing);
    assert_eq!(chain.choices, ["exact", "tidy", "tidy-french", "keyword"]);
    for choice in &chain.choices {
        assert_ne!(&choice_display_label(choice), choice);
        let detail = choice_detail(choice).expect("every chain explains itself");
        assert!(!detail.description.is_empty());
        assert!(!detail.example.is_empty());
    }
    assert_eq!(choice_detail("tine"), None);
    let script = plans
        .iter()
        .find(|plan| plan.key == "chinese-script")
        .expect("chinese-script is listed");
    assert_eq!(script.kind, WidgetKind::Choice);
    assert_eq!(script.placement, Placement::ProcessingScript);
    assert_eq!(
        script.choices,
        [
            "as-recognised",
            "simplified",
            "traditional",
            "traditional-taiwan",
            "traditional-hongkong"
        ]
    );
    for choice in &script.choices {
        assert_ne!(&choice_display_label(choice), choice);
    }
    let record = plans
        .iter()
        .find(|plan| plan.key == "processing-history")
        .expect("processing-history is listed");
    assert_eq!(record.kind, WidgetKind::Switch);
    assert_eq!(record.placement, Placement::ProcessingRecord);
    assert!(plans.iter().any(|plan| plan.kind == WidgetKind::Text));
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
fn sounds_are_a_boolean_that_defaults_on() {
    let files = TestFiles::new("sounds");
    let adapter = open_adapter(&files);

    assert_eq!(
        adapter.get("sounds").unwrap(),
        ClientSettingValue::Boolean(true)
    );
    adapter
        .set("sounds", ClientSettingValue::Boolean(false))
        .unwrap();
    assert_eq!(
        open_adapter(&files).get("sounds").unwrap(),
        ClientSettingValue::Boolean(false)
    );
    for wrong in [
        ClientSettingValue::Text("true".into()),
        ClientSettingValue::Integer(1),
    ] {
        assert!(matches!(
            adapter.set("sounds", wrong),
            Err(ClientSettingsError::InvalidValue { key, .. }) if key == "sounds"
        ));
    }
    assert!(matches!(
        adapter.set("silence-timeout", ClientSettingValue::Boolean(true)),
        Err(ClientSettingsError::InvalidValue { key, .. }) if key == "silence-timeout"
    ));
}

fn choice(value: &str) -> ClientSettingValue {
    ClientSettingValue::Choice(value.into())
}

/// The processing keys of the page are views over the stored steps: a
/// choice writes the steps and their parameters, and the stored list reads
/// back as the nearest choice.
#[test]
fn the_chain_and_the_script_are_views_over_the_stored_steps() {
    let files = TestFiles::new("views");
    let adapter = open_adapter(&files);
    assert_eq!(adapter.get("processing-chain").unwrap(), choice("exact"));
    assert_eq!(
        adapter.get("chinese-script").unwrap(),
        choice("as-recognised")
    );
    assert!(!adapter.has_user_value("processing-chain").unwrap());

    adapter
        .set("processing-chain", choice("tidy-french"))
        .unwrap();
    assert_eq!(
        adapter.get("processing-chain").unwrap(),
        choice("tidy-french")
    );
    assert!(adapter.has_user_value("processing-chain").unwrap());

    adapter.set("chinese-script", choice("simplified")).unwrap();
    assert_eq!(adapter.get("chinese-script").unwrap(), choice("simplified"));
    // The two settings do not disturb each other.
    assert_eq!(
        adapter.get("processing-chain").unwrap(),
        choice("tidy-french")
    );

    adapter.set("processing-chain", choice("exact")).unwrap();
    assert_eq!(adapter.get("chinese-script").unwrap(), choice("simplified"));
    adapter
        .set("chinese-script", choice("as-recognised"))
        .unwrap();
    assert_eq!(
        adapter.get("chinese-script").unwrap(),
        choice("as-recognised")
    );

    assert!(matches!(
        adapter.set("processing-chain", choice("summary")),
        Err(ClientSettingsError::InvalidValue { .. })
    ));
    assert!(matches!(
        adapter.set("chinese-script", ClientSettingValue::Boolean(true)),
        Err(ClientSettingsError::InvalidValue { .. })
    ));
}

/// What the daemon reads is what the page wrote: the step ids in stage
/// order and their parameters, in the keyfile.
#[test]
fn a_choice_reaches_the_keyfile_as_steps_and_parameters() {
    let files = TestFiles::new("views-keyfile");
    let adapter = open_adapter(&files);
    adapter
        .set("processing-chain", choice("tidy-french"))
        .unwrap();
    adapter
        .set("chinese-script", choice("traditional-taiwan"))
        .unwrap();
    gio::Settings::sync();
    let text = std::fs::read_to_string(files.0.join("settings/keyfile")).unwrap();
    assert!(
        text.contains("processing-steps=['tidy', 'translate', 'script']"),
        "{text}"
    );
    assert!(text.contains("'translate.to': 'fr'"), "{text}");
    assert!(
        text.contains("'script.mode': 'traditional-taiwan'"),
        "{text}"
    );
    assert!(!text.contains("processing-chain"), "{text}");
}

/// A list the page cannot name reads as the nearest choice, and reading it
/// changes nothing.
#[test]
fn a_stored_list_that_matches_no_choice_reads_as_the_nearest() {
    let files = TestFiles::new("views-nearest");
    let path = files.0.join("settings/keyfile");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "[com/canonical/myna/dictation]\nprocessing-steps=['fit', 'tidy', 'script']\n\
         processing-params={'script.mode': 'traditional'}\n",
    )
    .unwrap();
    let adapter = open_adapter(&files);
    assert_eq!(adapter.get("processing-chain").unwrap(), choice("tidy"));
    assert_eq!(
        adapter.get("chinese-script").unwrap(),
        choice("traditional")
    );
    let before = std::fs::read_to_string(&path).unwrap();
    let _ = adapter.list().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

/// A step list written by anyone moves the page's chain and script rows.
#[test]
fn a_change_to_the_stored_steps_is_notified_as_the_chain_and_the_script() {
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            let files = TestFiles::new("views-notify");
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
            writer.set("processing-chain", choice("keyword")).unwrap();

            let deadline = Instant::now() + Duration::from_secs(5);
            while observed.lock().unwrap().len() < 2 && Instant::now() < deadline {
                while context.iteration(false) {}
                std::thread::sleep(Duration::from_millis(10));
            }
            let observed = observed.lock().unwrap();
            let seen = |key: &str| {
                observed
                    .iter()
                    .find(|setting| setting.key().as_str() == key)
                    .map(|setting| setting.value().clone())
            };
            assert_eq!(seen("processing-chain"), Some(choice("keyword")));
            assert_eq!(seen("chinese-script"), Some(choice("as-recognised")));
        })
        .unwrap();
}
