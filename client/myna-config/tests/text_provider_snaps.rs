//! `myna-text` and `myna-translation` are two text shares on one `myna:backend`
//! plug: both survive a speech-model switch, neither is a speech model.

use myna_config::active_backend::SwitchPlan;
use myna_config::domain::{
    connection_snapshot, ActiveBackendState, BackendIdentity, ConnectionSnapshot,
    ContentConnections,
};
use serde_json::{json, Value};

fn provider(snap: &str, feeds_myna: bool) -> Value {
    let connections = if feeds_myna {
        json!([{ "snap": "myna", "plug": "backend" }])
    } else {
        json!([])
    };
    json!({
        "snap": snap,
        "slot": "provider",
        "interface": "content",
        "attrs": { "content": "inference-provider" },
        "connections": connections,
    })
}

fn snapshot(active_speech: &str) -> ConnectionSnapshot {
    let slots = json!([
        provider("myna-parakeet", active_speech == "myna-parakeet"),
        provider("myna-whisper", active_speech == "myna-whisper"),
        provider("myna-text", true),
        provider("myna-translation", true),
    ]);
    let connections: ContentConnections =
        serde_json::from_value(json!({ "slots": slots })).unwrap();
    connection_snapshot(&connections)
}

#[test]
fn both_text_snaps_are_preserved_text_providers_not_speech_models() {
    let snapshot = snapshot("myna-parakeet");
    assert_eq!(
        snapshot.text_providers(),
        &[
            BackendIdentity::new("myna-text", "provider"),
            BackendIdentity::new("myna-translation", "provider"),
        ]
    );
    assert_eq!(
        snapshot.active_state(),
        ActiveBackendState::Connected(BackendIdentity::new("myna-parakeet", "provider"))
    );
    assert!(snapshot.strays().is_empty());
}

#[test]
fn switching_the_speech_model_keeps_both_text_connections() {
    let plan = SwitchPlan::new(
        &snapshot("myna-parakeet"),
        BackendIdentity::new("myna-whisper", "provider"),
    )
    .unwrap();
    let commands: Vec<_> = plan
        .operations()
        .iter()
        .map(|op| format!("{} {}", op.executable(), op.arguments().join(" ")))
        .collect();
    assert_eq!(
        commands,
        [
            "snap disconnect myna:backend myna-parakeet:provider",
            "snap connect myna:backend myna-whisper:provider",
            "systemctl --user restart snap.myna.myna.service",
        ]
    );
}
