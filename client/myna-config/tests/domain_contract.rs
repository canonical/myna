use myna_config::domain::{
    parse_connections, parse_engine_options, parse_model_options, parse_modelctl_config,
    parse_status, ActiveBackendState, BackendIdentity, ClientSetting, ClientSettingKey,
    ClientSettingValue, ConfigScope, ConfigValue, ConnectionState, StagedChange, SwitchFailure,
};

const CONNECTIONS: &str = include_str!("fixtures/snap-connections.txt");
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
    let snapshot = parse_connections(CONNECTIONS).unwrap();
    assert_eq!(snapshot.backends().len(), 2);
    assert_eq!(
        snapshot.active_state(),
        ActiveBackendState::Connected(BackendIdentity::new("myna-parakeet"))
    );

    let disconnected =
        parse_connections(include_str!("fixtures/snap-connections-empty.txt")).unwrap();
    assert_eq!(
        disconnected.active_state(),
        ActiveBackendState::Disconnected
    );

    let multiple =
        parse_connections(include_str!("fixtures/snap-connections-multiple.txt")).unwrap();
    assert!(matches!(
        multiple.active_state(),
        ActiveBackendState::MultiplyConnected(backends) if backends.len() == 2
    ));

    let failed = ActiveBackendState::failed_switch(
        Some(BackendIdentity::new("myna-parakeet")),
        BackendIdentity::new("myna-whisper"),
        ConnectionState::MultiplyConnected(vec![
            BackendIdentity::new("myna-parakeet"),
            BackendIdentity::new("myna-other"),
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
    let partial = parse_connections(include_str!("fixtures/snap-connections-partial.txt")).unwrap();
    assert_eq!(partial.backends(), &[BackendIdentity::new("myna-whisper")]);

    assert!(parse_connections("not snap output\n").is_err());
}

#[test]
fn modelctl_get_preserves_scoped_scalar_values() {
    let config = parse_modelctl_config(MODELCTL_GET).unwrap();
    assert_eq!(
        config.get(ConfigScope::User, "ws.unix-socket"),
        Some(&ConfigValue::Text(
            "/var/snap/myna-parakeet/common/run/ubustt.sock".into()
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
