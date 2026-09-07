use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use myna_config::app::smoke_build;
use myna_config::domain::{
    ClientSetting, ClientSettingKey, ClientSettingMetadata, ClientSettingValue, SettingRange,
};
use myna_config::myna_settings::{
    choice_display_label, widget_plan, DebouncedTextCommit, MynaSettingsController, PageState,
    SettingsEvent, WidgetKind,
};
use myna_config::ports::{
    ClientSettings, ClientSettingsCallback, ClientSettingsError, ClientSettingsSubscription,
};

#[derive(Default)]
struct FakeSettings {
    rows: RefCell<Vec<ClientSettingMetadata>>,
    values: RefCell<BTreeMap<String, ClientSettingValue>>,
    callbacks: RefCell<Vec<ClientSettingsCallback>>,
    writes: RefCell<Vec<(String, ClientSettingValue)>>,
    resets: RefCell<Vec<String>>,
    list_error: RefCell<Option<ClientSettingsError>>,
    write_error: RefCell<Option<ClientSettingsError>>,
}

struct FakeSubscription;
impl ClientSettingsSubscription for FakeSubscription {}

impl FakeSettings {
    fn with_rows(rows: Vec<ClientSettingMetadata>) -> Rc<Self> {
        let values = rows
            .iter()
            .map(|row| (row.key().as_str().to_owned(), row.current_value().clone()))
            .collect();
        Rc::new(Self {
            rows: RefCell::new(rows),
            values: RefCell::new(values),
            ..Self::default()
        })
    }

    fn external_change(&self, key: &str, value: ClientSettingValue) {
        self.values
            .borrow_mut()
            .insert(key.to_owned(), value.clone());
        let change = ClientSetting::new(ClientSettingKey::new(key).unwrap(), value).unwrap();
        for callback in self.callbacks.borrow().iter() {
            callback(change.clone());
        }
    }
}

impl ClientSettings for FakeSettings {
    fn list(&self) -> Result<Vec<ClientSettingMetadata>, ClientSettingsError> {
        if let Some(error) = self.list_error.borrow_mut().take() {
            Err(error)
        } else {
            Ok(self.rows.borrow().clone())
        }
    }

    fn get(&self, key: &str) -> Result<ClientSettingValue, ClientSettingsError> {
        self.values
            .borrow()
            .get(key)
            .cloned()
            .ok_or_else(|| ClientSettingsError::UnknownKey {
                key: key.to_owned(),
            })
    }

    fn set(&self, key: &str, value: ClientSettingValue) -> Result<(), ClientSettingsError> {
        self.writes
            .borrow_mut()
            .push((key.to_owned(), value.clone()));
        if let Some(error) = self.write_error.borrow_mut().take() {
            return Err(error);
        }
        self.values.borrow_mut().insert(key.to_owned(), value);
        Ok(())
    }

    fn reset(&self, key: &str) -> Result<(), ClientSettingsError> {
        self.resets.borrow_mut().push(key.to_owned());
        if let Some(error) = self.write_error.borrow_mut().take() {
            return Err(error);
        }
        let default = self
            .rows
            .borrow()
            .iter()
            .find(|row| row.key().as_str() == key)
            .unwrap()
            .default_value()
            .clone();
        self.values.borrow_mut().insert(key.to_owned(), default);
        Ok(())
    }

    fn subscribe(
        &self,
        callback: ClientSettingsCallback,
    ) -> Result<Box<dyn ClientSettingsSubscription>, ClientSettingsError> {
        self.callbacks.borrow_mut().push(callback);
        Ok(Box::new(FakeSubscription))
    }
}

fn metadata(
    key: &str,
    value: ClientSettingValue,
    range: SettingRange,
    writable: bool,
) -> ClientSettingMetadata {
    ClientSettingMetadata::new(
        ClientSettingKey::new(key).unwrap(),
        Some(format!("{key} summary")),
        Some(format!("{key} description")),
        value.clone(),
        range,
        value,
        writable,
    )
}

fn rows() -> Vec<ClientSettingMetadata> {
    vec![
        metadata(
            "streaming-mode",
            ClientSettingValue::Choice("auto".into()),
            SettingRange::Choices(vec!["auto".into(), "streaming".into(), "batch".into()]),
            true,
        ),
        metadata(
            "language",
            ClientSettingValue::Text(String::new()),
            SettingRange::Unrestricted,
            true,
        ),
    ]
}

#[test]
fn load_builds_rows_only_from_adapter_metadata() {
    let fake = FakeSettings::with_rows(rows());
    let controller = MynaSettingsController::load(fake.clone());

    let PageState::Ready(loaded) = controller.state() else {
        panic!("expected ready state");
    };
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].metadata().key().as_str(), "streaming-mode");
    assert_eq!(loaded[1].metadata().key().as_str(), "language");
}

#[test]
fn edit_saves_immediately_and_marks_only_that_row_pending() {
    let fake = FakeSettings::with_rows(rows());
    let controller = MynaSettingsController::load(fake.clone());
    let events = Rc::new(RefCell::new(Vec::new()));
    controller.observe({
        let events = events.clone();
        move |event| events.borrow_mut().push(event)
    });

    let request = controller
        .set("streaming-mode", ClientSettingValue::Choice("batch".into()))
        .unwrap();

    assert!(fake.writes.borrow().is_empty());
    assert!(controller.row("streaming-mode").unwrap().pending());
    assert!(!controller.row("language").unwrap().pending());
    let result = request.persist(fake.as_ref());
    controller.complete(request, result);

    assert_eq!(
        fake.writes.borrow().as_slice(),
        &[(
            "streaming-mode".into(),
            ClientSettingValue::Choice("batch".into())
        )]
    );
    let events = events.borrow();
    assert!(matches!(
        events[0],
        SettingsEvent::RowChanged {
            ref key,
            pending: true,
            value: ClientSettingValue::Choice(ref value),
        } if key == "streaming-mode" && value == "batch"
    ));
    assert!(matches!(
        events.last().unwrap(),
        SettingsEvent::RowChanged {
            key,
            pending: false,
            value: ClientSettingValue::Choice(value),
        } if key == "streaming-mode" && value == "batch"
    ));
}

#[test]
fn failed_save_rolls_back_and_reports_actionable_error() {
    let fake = FakeSettings::with_rows(rows());
    *fake.write_error.borrow_mut() = Some(ClientSettingsError::StoreUnavailable {
        message: "disk is read-only".into(),
    });
    let controller = MynaSettingsController::load(fake.clone());
    let events = Rc::new(RefCell::new(Vec::new()));
    controller.observe({
        let events = events.clone();
        move |event| events.borrow_mut().push(event)
    });

    let request = controller
        .set("language", ClientSettingValue::Text("fr".into()))
        .unwrap();
    let result = request.persist(fake.as_ref());
    let error_message = result.as_ref().unwrap_err().to_string();
    controller.complete(request, result);

    assert!(error_message.contains("disk is read-only"));
    assert_eq!(
        controller.row("language").unwrap().value(),
        &ClientSettingValue::Text(String::new())
    );
    assert!(events.borrow().iter().any(|event| matches!(
        event,
        SettingsEvent::SaveFailed { key, detail }
            if key == "language" && detail.contains("disk is read-only")
    )));
}

#[test]
fn reset_restores_schema_default_immediately() {
    let mut changed = rows();
    changed[1] = ClientSettingMetadata::new(
        ClientSettingKey::new("language").unwrap(),
        Some("Language".into()),
        Some("Hint".into()),
        ClientSettingValue::Text(String::new()),
        SettingRange::Unrestricted,
        ClientSettingValue::Text("fr".into()),
        true,
    );
    let fake = FakeSettings::with_rows(changed);
    let controller = MynaSettingsController::load(fake.clone());

    let request = controller.reset("language").unwrap();
    let result = request.persist(fake.as_ref());
    controller.complete(request, result);

    assert_eq!(fake.resets.borrow().as_slice(), &["language"]);
    assert_eq!(
        controller.row("language").unwrap().value(),
        &ClientSettingValue::Text(String::new())
    );
}

#[test]
fn external_change_updates_only_the_affected_row() {
    let fake = FakeSettings::with_rows(rows());
    let controller = MynaSettingsController::load(fake.clone());
    let original_language = controller.row("language").unwrap();

    fake.external_change(
        "streaming-mode",
        ClientSettingValue::Choice("streaming".into()),
    );

    assert_eq!(
        controller.row("streaming-mode").unwrap().value(),
        &ClientSettingValue::Choice("streaming".into())
    );
    assert_eq!(controller.row("language").unwrap(), original_language);
}

#[test]
fn matching_store_notification_does_not_clear_pending_before_completion() {
    let fake = FakeSettings::with_rows(rows());
    let controller = MynaSettingsController::load(fake.clone());
    let request = controller
        .set("language", ClientSettingValue::Text("fr".into()))
        .unwrap();

    fake.external_change("language", ClientSettingValue::Text("fr".into()));

    assert!(controller.row("language").unwrap().pending());
    let result = request.persist(fake.as_ref());
    controller.complete(request, result);
    assert!(!controller.row("language").unwrap().pending());
}

#[test]
fn delayed_local_notification_cannot_cancel_a_newer_save() {
    let fake = FakeSettings::with_rows(rows());
    let controller = MynaSettingsController::load(fake.clone());
    let first = controller
        .set("language", ClientSettingValue::Text("fr".into()))
        .unwrap();
    let result = first.persist(fake.as_ref());
    controller.complete(first, result);
    let latest = controller
        .set("language", ClientSettingValue::Text("de".into()))
        .unwrap();

    fake.external_change("language", ClientSettingValue::Text("fr".into()));
    assert!(controller.row("language").unwrap().pending());
    let result = latest.persist(fake.as_ref());
    controller.complete(latest, result);

    assert_eq!(
        fake.values.borrow().get("language"),
        Some(&ClientSettingValue::Text("de".into()))
    );
}

#[test]
fn stale_write_echo_reconciles_after_an_external_update() {
    let fake = FakeSettings::with_rows(rows());
    let controller = MynaSettingsController::load(fake.clone());
    let request = controller
        .set("language", ClientSettingValue::Text("fr".into()))
        .unwrap();
    let result = request.persist(fake.as_ref());

    fake.external_change("language", ClientSettingValue::Text("de".into()));
    controller.complete(request, result);
    fake.external_change("language", ClientSettingValue::Text("fr".into()));

    assert_eq!(
        controller.row("language").unwrap().value(),
        &ClientSettingValue::Text("fr".into())
    );
}

#[test]
fn missing_schema_produces_an_actionable_error_state() {
    let fake = FakeSettings::with_rows(Vec::new());
    *fake.list_error.borrow_mut() = Some(ClientSettingsError::SchemaUnavailable {
        schema_id: "com.canonical.Myna.Dictation",
        guidance: "Install the schema, then restart Myna Settings.",
    });

    let controller = MynaSettingsController::load(fake);

    let PageState::Error(message) = controller.state() else {
        panic!("expected error state");
    };
    assert!(message.contains("com.canonical.Myna.Dictation"));
    assert!(message.contains("Install"));
}

#[test]
fn an_empty_schema_has_an_explicit_empty_state() {
    let controller = MynaSettingsController::load(FakeSettings::with_rows(Vec::new()));
    assert_eq!(controller.state(), PageState::Empty);
}

#[test]
fn read_only_rows_never_attempt_a_write_or_reset() {
    let fake = FakeSettings::with_rows(vec![metadata(
        "managed",
        ClientSettingValue::Text("policy".into()),
        SettingRange::Unrestricted,
        false,
    )]);
    let controller = MynaSettingsController::load(fake.clone());

    assert!(matches!(
        controller.set("managed", ClientSettingValue::Text("changed".into())),
        Err(ClientSettingsError::NotWritable { .. })
    ));
    assert!(matches!(
        controller.reset("managed"),
        Err(ClientSettingsError::NotWritable { .. })
    ));
    assert!(fake.writes.borrow().is_empty());
    assert!(fake.resets.borrow().is_empty());
}

#[test]
fn headless_smoke_build_maps_schema_metadata_to_widget_kinds() {
    let unrestricted = ClientSettingMetadata::new(
        ClientSettingKey::new("unrelated").unwrap(),
        Some("Unrelated".into()),
        Some(String::new()),
        ClientSettingValue::Text(String::new()),
        SettingRange::Unrestricted,
        ClientSettingValue::Text(String::new()),
        true,
    );
    let fake = FakeSettings::with_rows({
        let mut rows = rows();
        rows.push(unrestricted.clone());
        rows
    });

    let plans = smoke_build(fake).unwrap();

    assert_eq!(plans.len(), 3);
    assert_eq!(plans[0].kind, WidgetKind::Choice);
    assert_eq!(plans[1].kind, WidgetKind::Text);
    assert_eq!(widget_plan(&unrestricted).kind, WidgetKind::Text);
}

#[test]
fn superseded_debounced_edits_and_apply_cannot_commit_stale_or_duplicate_values() {
    let mut policy = DebouncedTextCommit::new("en");

    let stale = policy.changed("f").unwrap();
    let latest = policy.changed("fr").unwrap();
    assert_eq!(policy.take(stale), None);
    assert_eq!(policy.take(latest), Some("fr".into()));

    policy.committed("fr");
    assert_eq!(policy.apply("fr"), None);
    let scheduled = policy.changed("de").unwrap();
    assert_eq!(policy.apply("de"), Some("de".into()));
    assert_eq!(policy.take(scheduled), None);
}

#[test]
fn closing_before_debounce_flushes_the_latest_edit_once() {
    let mut policy = DebouncedTextCommit::new("en");

    let scheduled = policy.changed("fr").unwrap();
    assert_eq!(policy.flush(), Some("fr".into()));
    assert_eq!(policy.take(scheduled), None);
    assert_eq!(policy.flush(), None);
}

#[test]
fn stale_persistence_completion_cannot_overwrite_a_newer_edit() {
    let fake = FakeSettings::with_rows(rows());
    let controller = MynaSettingsController::load(fake.clone());
    let failures = Rc::new(RefCell::new(Vec::new()));
    controller.observe({
        let failures = failures.clone();
        move |event| {
            if let SettingsEvent::SaveFailed { key, .. } = event {
                failures.borrow_mut().push(key);
            }
        }
    });

    let stale = controller
        .set("language", ClientSettingValue::Text("fr".into()))
        .unwrap();
    let latest = controller
        .set("language", ClientSettingValue::Text("de".into()))
        .unwrap();

    let latest_result = latest.persist(fake.as_ref());
    let stale_result = stale.persist(fake.as_ref());
    controller.complete(latest, latest_result);
    controller.complete(stale, stale_result);

    let row = controller.row("language").unwrap();
    assert_eq!(row.value(), &ClientSettingValue::Text("de".into()));
    assert!(!row.pending());
    assert!(failures.borrow().is_empty());
    assert_eq!(
        fake.writes.borrow().as_slice(),
        &[("language".into(), ClientSettingValue::Text("de".into()))]
    );
}

#[test]
fn failed_latest_save_rolls_back_past_a_superseded_optimistic_edit() {
    let fake = FakeSettings::with_rows(rows());
    let controller = MynaSettingsController::load(fake.clone());
    let stale = controller
        .set("language", ClientSettingValue::Text("fr".into()))
        .unwrap();
    let latest = controller
        .set("language", ClientSettingValue::Text("de".into()))
        .unwrap();
    *fake.write_error.borrow_mut() = Some(ClientSettingsError::StoreUnavailable {
        message: "disk is read-only".into(),
    });

    let stale_result = stale.persist(fake.as_ref());
    let latest_result = latest.persist(fake.as_ref());
    controller.complete(stale, stale_result);
    controller.complete(latest, latest_result);

    assert_eq!(
        controller.row("language").unwrap().value(),
        &ClientSettingValue::Text(String::new())
    );
    assert!(!controller.row("language").unwrap().pending());
}

#[test]
fn schema_choices_have_translated_labels_but_keep_raw_index_mapping() {
    let raw = [
        "auto",
        "streaming",
        "batch",
        "ribbon",
        "vumeter",
        "bar",
        "progress",
        "future-mode",
    ];
    let labels: Vec<_> = raw
        .iter()
        .map(|choice| choice_display_label(choice))
        .collect();

    assert_eq!(
        labels,
        [
            "Automatic",
            "Streaming",
            "Batch",
            "Ribbon",
            "VU meter",
            "Bar",
            "Progress",
            "future-mode",
        ]
    );
    assert_ne!(labels[0], raw[0]);
    assert_eq!(raw[5], "bar");
}

#[test]
fn enum_display_labels_are_extracted_into_the_gettext_template() {
    let pot = include_str!("../po/myna-config.pot");
    for label in [
        "Automatic",
        "Streaming",
        "Batch",
        "Ribbon",
        "VU meter",
        "Bar",
        "Progress",
    ] {
        assert!(pot.contains(&format!("msgid \"{label}\"")), "{label}");
    }
}
