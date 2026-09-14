use myna_config::domain::{
    parse_connections, parse_engine_options, parse_model_options, parse_modelctl_config,
    parse_status, ActiveBackendState, BackendIdentity, ClientSetting, ClientSettingKey,
    ClientSettingValue, ConfigScope, ConfigValue, ConnectionState, StagedChange, SwitchFailure,
};

const CONNECTIONS: &str = include_str!("fixtures/snap-connections.txt");
const CONTENT_INTERFACE: &str = include_str!("fixtures/snap-interface-content.txt");
const MODELCTL_GET: &str = include_str!("fixtures/modelctl-get.txt");
const STATUS: &str = include_str!("fixtures/modelctl-status.json");
const MODELS: &str = include_str!("fixtures/modelctl-list-models.json");
const ENGINES: &str = include_str!("fixtures/modelctl-list-engines.json");

#[test]
fn client_settings_are_typed_and_validated() {
    let setting = ClientSetting::new(
        ClientSettingKey::new("streaming-mode").unwrap(),
        ClientSettingValue::Choice("streaming".into()),
    )
    .unwrap();
    assert_eq!(
        setting.value(),
        &ClientSettingValue::Choice("streaming".into())
    );

    let discovered = ClientSetting::new(
        ClientSettingKey::new("future-setting").unwrap(),
        ClientSettingValue::Text("value".into()),
    )
    .unwrap();
    assert_eq!(discovered.key().as_str(), "future-setting");

    assert!(ClientSettingKey::new(" ").is_err());
}

#[test]
fn connections_express_every_honest_active_backend_state() {
    let snapshot = parse_connections(CONNECTIONS, CONTENT_INTERFACE).unwrap();
    assert_eq!(snapshot.backends().len(), 2);
    assert_eq!(
        snapshot.active_state(),
        ActiveBackendState::Connected(BackendIdentity::new("myna-parakeet", "provider"))
    );

    let disconnected = parse_connections(
        include_str!("fixtures/snap-connections-empty.txt"),
        CONTENT_INTERFACE,
    )
    .unwrap();
    assert_eq!(
        disconnected.active_state(),
        ActiveBackendState::Disconnected
    );

    let multiple = parse_connections(
        include_str!("fixtures/snap-connections-multiple.txt"),
        CONTENT_INTERFACE,
    )
    .unwrap();
    assert!(matches!(
        multiple.active_state(),
        ActiveBackendState::MultiplyConnected(backends) if backends.len() == 2
    ));

    let failed = ActiveBackendState::failed_switch(
        Some(BackendIdentity::new("myna-parakeet", "provider")),
        BackendIdentity::new("myna-whisper", "provider"),
        ConnectionState::MultiplyConnected(vec![
            BackendIdentity::new("myna-parakeet", "provider"),
            BackendIdentity::new("myna-other", "provider"),
        ]),
        SwitchFailure::Connect {
            message: "permission denied".into(),
        },
    );
    assert!(matches!(
        failed,
        ActiveBackendState::FailedSwitch {
            observed: ConnectionState::MultiplyConnected(backends),
            ..
        } if backends.len() == 2
    ));
}

#[test]
fn connections_accept_partial_rows_and_reject_an_invalid_header() {
    let partial = parse_connections(
        include_str!("fixtures/snap-connections-partial.txt"),
        CONTENT_INTERFACE,
    )
    .unwrap();
    assert_eq!(
        partial.backends(),
        &[BackendIdentity::new("myna-whisper", "provider")]
    );

    assert!(parse_connections("not snap output\n", CONTENT_INTERFACE).is_err());
    assert!(parse_connections(CONNECTIONS, "not snap output\n").is_err());
}

#[test]
fn connections_record_the_slot_name_of_each_provider() {
    let snapshot = parse_connections(CONNECTIONS, CONTENT_INTERFACE).unwrap();
    let slots: Vec<_> = snapshot
        .backends()
        .iter()
        .map(BackendIdentity::slot)
        .collect();
    assert_eq!(slots, ["myna-parakeet:provider", "myna-whisper:provider"]);
}

#[test]
fn connections_discover_a_provider_whose_slot_is_not_named_provider() {
    let snapshot = parse_connections(
        "Interface Plug Slot Notes\n\
         content - smollm2:llm -\n\
         content[inference-provider] myna:backend community-asr:speech manual\n",
        "name: content\n\
         slots:\n  \
         - community-asr:speech:\n      \
         content: inference-provider\n  \
         - smollm2:llm (label: with colons):\n      \
         content: inference-provider\n",
    )
    .unwrap();
    assert_eq!(
        snapshot.backends(),
        &[
            BackendIdentity::new("community-asr", "speech"),
            BackendIdentity::new("smollm2", "llm"),
        ]
    );
    assert_eq!(
        snapshot.active_state(),
        ActiveBackendState::Connected(BackendIdentity::new("community-asr", "speech"))
    );
}

#[test]
fn connections_ignore_content_slots_with_another_content_id() {
    let snapshot = parse_connections(
        "Interface Plug Slot Notes\n\
         content[other] myna:backend legacy:socket manual\n\
         content - gnome-42-2204:gnome-42-2204 -\n\
         content - myna-whisper:provider -\n\
         content - gtk-common-themes:content -\n",
        "name: content\n\
         plugs:\n  \
         - firefox:themes:\n      \
         content: inference-provider\n\
         slots:\n  \
         - gnome-42-2204:\n      \
         content: gnome-42-2204\n      \
         nested:\n        \
         content: inference-provider\n  \
         - gtk-common-themes\n  \
         - legacy:socket:\n      \
         content: other\n",
    )
    .unwrap();
    assert!(snapshot.backends().is_empty());
    assert_eq!(snapshot.active_state(), ActiveBackendState::Disconnected);
}

#[test]
fn connections_ignore_rows_with_an_empty_snap_or_slot() {
    let snapshot = parse_connections(
        "Interface Plug Slot Notes\n\
         content[inference-provider] myna:backend :provider manual\n\
         content[inference-provider] myna:backend myna-parakeet: manual\n",
        CONTENT_INTERFACE,
    )
    .unwrap();
    assert!(snapshot.backends().is_empty());
    assert_eq!(snapshot.active_state(), ActiveBackendState::Disconnected);
}

#[test]
fn content_interface_listing_must_name_the_content_interface() {
    for header in ["name: other\n", "summary: content\n", "content\n", ""] {
        assert!(
            parse_connections(CONNECTIONS, header).is_err(),
            "{header:?} should be rejected"
        );
    }
}

#[test]
fn connections_resolve_a_slot_named_like_the_interface() {
    let snapshot = parse_connections(
        "Interface Plug Slot Notes\ncontent - odd:content -\n",
        "name: content\nslots:\n  - odd:\n      content: inference-provider\n",
    )
    .unwrap();
    assert_eq!(
        snapshot.backends(),
        &[BackendIdentity::new("odd", "content")]
    );
}

#[test]
fn modelctl_get_preserves_scoped_scalar_values() {
    let config = parse_modelctl_config(MODELCTL_GET).unwrap();
    assert_eq!(
        config.get(ConfigScope::User, "ws.unix-socket"),
        Some(&ConfigValue::Text(
            "/var/snap/myna-parakeet/common/share/provider/myna.sock".into()
        ))
    );
    assert_eq!(
        config.get(ConfigScope::User, "streaming"),
        Some(&ConfigValue::Boolean(true))
    );

    assert!(parse_modelctl_config("").unwrap().is_empty());
    assert!(parse_modelctl_config("indented:\n  nested: value\n").is_err());
}

#[test]
fn status_exposes_service_health_only_as_diagnostics() {
    let status = parse_status(STATUS).unwrap();
    assert_eq!(status.engine(), Some("cpu"));
    assert_eq!(status.services()[0].name(), "server");
    assert!(status.services()[0].is_active());

    let partial = parse_status(include_str!("fixtures/modelctl-status-partial.json")).unwrap();
    assert!(partial.services().is_empty());
    assert!(parse_status("{}").is_err());
    assert!(parse_status(r#"{"unrelated":"json"}"#).is_err());
    assert!(parse_status(include_str!("fixtures/malformed.json")).is_err());
}

#[test]
fn model_and_engine_options_follow_the_live_contract() {
    let models = parse_model_options(MODELS).unwrap();
    assert_eq!(models.active(), Some("parakeet-tdt-0.6b-v3"));
    assert_eq!(models.options()[0].disk_size(), Some("687M"));

    let engines = parse_engine_options(ENGINES).unwrap();
    assert_eq!(engines.active(), Some("cpu"));
    assert!(engines.options()[0].compatible());
    assert_eq!(
        engines.options()[0]
            .configuration()
            .get("stream-partial-cadence-seconds"),
        Some(&ConfigValue::Number(0.5))
    );
}

#[test]
fn option_lists_handle_partial_and_empty_payloads_and_reject_malformed_json() {
    let models =
        parse_model_options(include_str!("fixtures/modelctl-list-models-partial.json")).unwrap();
    assert_eq!(models.options()[0].description(), None);
    assert!(parse_model_options("{}").unwrap().options().is_empty());

    let engines =
        parse_engine_options(include_str!("fixtures/modelctl-list-engines-partial.json")).unwrap();
    assert!(!engines.options()[0].compatible());
    assert!(parse_engine_options("{}").unwrap().options().is_empty());

    assert!(parse_model_options(include_str!("fixtures/malformed.json")).is_err());
    assert!(parse_engine_options(include_str!("fixtures/malformed.json")).is_err());
}

#[test]
fn staged_changes_reject_noops_and_retain_restart_requirements() {
    let change = StagedChange::new(
        ConfigScope::Engine,
        "stream-arm-seconds",
        ConfigValue::Integer(15),
        ConfigValue::Integer(20),
        true,
    )
    .unwrap();
    assert!(change.restart_required());

    let error = StagedChange::new(
        ConfigScope::Package,
        "streaming",
        ConfigValue::Boolean(true),
        ConfigValue::Boolean(true),
        false,
    )
    .unwrap_err();
    assert_eq!(error.field(), "streaming");
}

#[test]
fn modelctl_null_stands_in_for_an_empty_map() {
    // Every backend but parakeet writes `"configurations": null`, and serde's
    // `default` only covers a *missing* field.
    let engines =
        parse_engine_options(include_str!("fixtures/modelctl-list-engines-null.json")).unwrap();

    assert_eq!(engines.options().len(), 1);
    assert!(engines.options()[0].configuration().is_empty());
}
