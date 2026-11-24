use myna_config::domain::{
    connection_snapshot, parse_engine_options, parse_model_options, parse_modelctl_config,
    parse_status, ActiveBackendState, BackendIdentity, ClientSetting, ClientSettingKey,
    ClientSettingValue, ConfigValue, ConnectionSnapshot, ContentConnections, StagedChange,
};
use serde_json::{json, Value};

/// A `GET /v2/connections?select=all&interface=content` result from a VM,
/// trimmed: Parakeet connected, Whisper installed, a GNOME platform slot.
const CONNECTIONS: &str = include_str!("fixtures/snapd-connections.json");
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

fn snapshot_of(slots: Value) -> ConnectionSnapshot {
    let connections: ContentConnections =
        serde_json::from_value(json!({ "slots": slots })).expect("content connections");
    connection_snapshot(&connections)
}

/// A content slot with these attributes, connected to `myna:backend` or to
/// nothing.
fn slot(snap: &str, name: &str, attrs: Value, feeds_myna: bool) -> Value {
    let connections = if feeds_myna {
        json!([{ "snap": "myna", "plug": "backend" }])
    } else {
        json!([])
    };
    json!({
        "snap": snap,
        "slot": name,
        "interface": "content",
        "attrs": attrs,
        "connections": connections,
    })
}

fn provider() -> Value {
    json!({ "content": "inference-provider", "source": { "write": ["$SNAP_COMMON/share/provider"] } })
}

fn speech_provider() -> Value {
    json!({ "content": "inference-provider", "task": "speech-to-text" })
}

#[test]
fn connections_read_snapd_as_it_answers() {
    let connections: ContentConnections = serde_json::from_str(CONNECTIONS).unwrap();
    let snapshot = connection_snapshot(&connections);
    assert_eq!(
        snapshot.backends(),
        &[
            BackendIdentity::new("myna-parakeet", "provider"),
            BackendIdentity::new("myna-whisper", "provider"),
        ]
    );
    assert_eq!(
        snapshot.active_state(),
        ActiveBackendState::Connected(BackendIdentity::new("myna-parakeet", "provider"))
    );
    assert!(snapshot.strays().is_empty());
}

#[test]
fn connections_express_every_honest_active_backend_state() {
    let disconnected = snapshot_of(json!([
        slot("myna-parakeet", "provider", provider(), false),
        slot("myna-whisper", "provider", provider(), false),
    ]));
    assert_eq!(disconnected.backends().len(), 2);
    assert_eq!(
        disconnected.active_state(),
        ActiveBackendState::Disconnected
    );

    let multiple = snapshot_of(json!([
        slot("myna-parakeet", "provider", provider(), true),
        slot("myna-whisper", "provider", provider(), true),
    ]));
    assert!(matches!(
        multiple.active_state(),
        ActiveBackendState::MultiplyConnected(backends) if backends.len() == 2
    ));

    assert!(snapshot_of(json!([])).backends().is_empty());
    let nothing: ContentConnections = serde_json::from_str("{}").unwrap();
    assert!(connection_snapshot(&nothing).backends().is_empty());
}

#[test]
fn connections_discover_a_provider_whose_slot_is_not_named_provider() {
    let snapshot = snapshot_of(json!([
        slot("smollm2", "llm", speech_provider(), false),
        slot("community-asr", "speech", speech_provider(), true),
    ]));
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
fn connections_preserve_text_and_llm_providers_instead_of_marking_them_stray() {
    // gemma4 and myna-text share the inference-provider content id; connected
    // alongside or without a speech model, they are preserved text providers,
    // never listed as speech models and never disconnected as strays.
    let snapshot = snapshot_of(json!([
        slot("gemma4", "provider", provider(), true),
        slot("myna-text", "provider", provider(), true),
        slot("community-asr", "provider", speech_provider(), false),
        slot(
            "other-llm",
            "provider",
            json!({ "content": "inference-provider", "task": "text-generation" }),
            true,
        ),
    ]));
    assert_eq!(
        snapshot.backends(),
        &[BackendIdentity::new("community-asr", "provider")]
    );
    assert_eq!(snapshot.active_state(), ActiveBackendState::Disconnected);
    assert_eq!(
        snapshot.text_providers(),
        &[
            BackendIdentity::new("gemma4", "provider"),
            BackendIdentity::new("myna-text", "provider"),
            BackendIdentity::new("other-llm", "provider"),
        ]
    );
    assert!(snapshot.strays().is_empty());
}

#[test]
fn connections_count_a_published_myna_model_without_a_task() {
    // The store rejects the `task` attribute, so published backends lack it.
    let snapshot = snapshot_of(json!([slot("myna-funasr", "provider", provider(), true)]));
    assert_eq!(
        snapshot.active_state(),
        ActiveBackendState::Connected(BackendIdentity::new("myna-funasr", "provider"))
    );
}

/// Nemotron is no family Settings offers to install, but it is a Myna speech
/// model: the store's revisions predate `MYNA_ROLE=speech` in provider.env
/// (48d42907), and connected it must not pass for an LLM companion.
#[test]
fn connections_count_published_nemotron_as_speech_without_a_role() {
    let snapshot = snapshot_of(json!([slot("myna-nemotron", "provider", provider(), true)]));
    assert_eq!(
        snapshot.backends(),
        &[BackendIdentity::new("myna-nemotron", "provider")]
    );
    assert_eq!(
        snapshot.active_state(),
        ActiveBackendState::Connected(BackendIdentity::new("myna-nemotron", "provider"))
    );
    assert!(snapshot.text_providers().is_empty());
}

#[test]
fn connections_ignore_a_stale_connection_to_a_slot_with_another_content_id() {
    // snapd keeps a connection across the refresh that changed the plug's
    // content id, so only the slot's own attribute counts.
    let snapshot = snapshot_of(json!([
        slot(
            "myna-funasr",
            "ubustt-socket",
            json!({ "content": "ubustt-socket", "source": { "write": ["$SNAP_COMMON/run"] } }),
            true,
        ),
        slot("myna-parakeet", "provider", provider(), true),
    ]));
    let parakeet = BackendIdentity::new("myna-parakeet", "provider");
    assert_eq!(snapshot.backends(), std::slice::from_ref(&parakeet));
    assert_eq!(
        snapshot.active_state(),
        ActiveBackendState::Connected(parakeet)
    );
    assert_eq!(
        snapshot.strays(),
        &[BackendIdentity::new("myna-funasr", "ubustt-socket")]
    );
}

#[test]
fn connections_ignore_content_slots_with_another_content_id() {
    let snapshot = snapshot_of(json!([
        slot("legacy", "socket", json!({ "content": "other" }), false),
        slot(
            "gnome-42-2204",
            "gnome-42-2204",
            json!({ "content": "gnome-42-2204", "nested": { "content": "inference-provider" } }),
            false,
        ),
        slot("gtk-common-themes", "content", json!({}), false),
        // Another plug of myna's on a platform slot is not the backend.
        json!({
            "snap": "gnome-46-2404",
            "slot": "gnome-46-2404",
            "interface": "content",
            "attrs": { "content": "gnome-46-2404" },
            "connections": [{ "snap": "myna", "plug": "gnome-46-2404" }],
        }),
    ]));
    assert!(snapshot.backends().is_empty());
    assert_eq!(snapshot.active_state(), ActiveBackendState::Disconnected);
    assert!(snapshot.strays().is_empty());
}

#[test]
fn modelctl_get_preserves_scalar_values() {
    let config = parse_modelctl_config(MODELCTL_GET).unwrap();
    assert_eq!(
        config.get("ws.unix-socket"),
        Some(&ConfigValue::Text(
            "/var/snap/myna-parakeet/common/share/provider/myna.sock".into()
        ))
    );
    assert_eq!(config.get("streaming"), Some(&ConfigValue::Boolean(true)));

    assert!(parse_modelctl_config("").unwrap().is_empty());
    assert!(parse_modelctl_config("indented:\n  nested: value\n").is_err());
}

#[test]
fn status_exposes_service_health_only_as_diagnostics() {
    let status = parse_status(STATUS).unwrap();
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

    let engines = parse_engine_options(ENGINES).unwrap();
    assert_eq!(engines.active(), Some("cpu"));
    assert!(engines.options()[0].compatible());
}

#[test]
fn option_lists_handle_partial_and_empty_payloads_and_reject_malformed_json() {
    let models =
        parse_model_options(include_str!("fixtures/modelctl-list-models-partial.json")).unwrap();
    assert_eq!(models.options()[0].name(), "tiny");
    assert!(parse_model_options("{}").unwrap().options().is_empty());

    let engines =
        parse_engine_options(include_str!("fixtures/modelctl-list-engines-partial.json")).unwrap();
    assert!(!engines.options()[0].compatible());
    assert!(parse_engine_options("{}").unwrap().options().is_empty());

    assert!(parse_model_options(include_str!("fixtures/malformed.json")).is_err());
    assert!(parse_engine_options(include_str!("fixtures/malformed.json")).is_err());
}

#[test]
fn staged_changes_reject_noops() {
    assert!(StagedChange::new(
        "stream-arm-seconds",
        ConfigValue::Integer(15),
        ConfigValue::Integer(20),
    )
    .is_ok());

    let error = StagedChange::new(
        "streaming",
        ConfigValue::Boolean(true),
        ConfigValue::Boolean(true),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "streaming: staged value is unchanged");
}
