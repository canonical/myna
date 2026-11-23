//! Publishes whether the chosen processing chain can do all it asks, and
//! which steps the connected shares offer.
//!
//! `ChainStatus` is `ready`, `degraded` (some model step has no installed
//! model and is skipped) or `restart` (a model was connected after this
//! daemon started, so only a fresh process sees it); `ChainMissing` lists
//! the uncovered tasks. Settings reads both to tell the user, so a chain
//! that quietly does nothing is never mistaken for a chain that works.
//! `ChainMissingRequests` is the same list as JSON with each request's
//! parameters, and `InstalledSteps` is JSON of every model every connected
//! share offers and what became of each step. None carries dictated text.

use serde_json::{json, Value};
use tokio::sync::watch;

use crate::dbus::{PropertyValue, SharedBus};
use crate::process::resolve::{Candidate, Catalog, ChainStatus, Missing, Via};
use crate::process::step;
use crate::process::steps::{StepReport, StepState};

pub const READY: &str = "ready";
const DEGRADED: &str = "degraded";
const RESTART: &str = "restart";

/// The two property values for `status`.
pub fn wire(status: &ChainStatus) -> (&'static str, String) {
    match status {
        ChainStatus::Ready => (READY, String::new()),
        ChainStatus::Degraded { needs_restart, .. } => (
            if *needs_restart { RESTART } else { DEGRADED },
            status.missing_tasks().join(","),
        ),
    }
}

fn request(task: &str, params: &crate::process::wire::Params) -> Value {
    json!({"task": task, "params": params})
}

/// `ChainMissingRequests`: the uncovered requests, `[]` while ready.
pub fn missing_json(status: &ChainStatus) -> String {
    let missing: &[Missing] = match status {
        ChainStatus::Ready => &[],
        ChainStatus::Degraded { missing, .. } => missing,
    };
    Value::Array(
        missing
            .iter()
            .map(|m| request(&m.task, &m.params))
            .collect(),
    )
    .to_string()
}

fn model_json(c: &Candidate) -> Value {
    let cap = &c.capability;
    let class = cap
        .class
        .as_deref()
        .or_else(|| step::default_class(&cap.task));
    let mut params: Vec<Value> = cap
        .params
        .iter()
        .map(|p| {
            json!({"name": p.name, "kind": p.kind, "role": p.role, "required": p.required,
                   "values": cap.covers.get(&p.name)})
        })
        .collect();
    for (name, values) in &cap.covers {
        if cap.params.iter().all(|p| &p.name != name) {
            params.push(json!({"name": name, "values": values}));
        }
    }
    json!({
        "share": c.share,
        "model": cap.id,
        "task": cap.task,
        "class": class,
        "label_key": cap.task,
        "via": match c.via { Via::Myna => "myna-text-v1", Via::Chat => "openai-chat" },
        "params": params,
        "accepts": cap.accepts,
        "needs": cap.needs,
        "quality": cap.quality,
        "latency_ms": cap.latency_ms,
        "download_bytes": cap.download_bytes,
        "license": cap.license,
    })
}

/// One step of the active chain, at `index` in it: a chain may hold one step
/// twice, so the index and not the id names it. `move_above` is the index
/// of the step a not-applicable one would apply above, else null.
fn step_json(index: usize, report: &StepReport) -> Value {
    let mut move_above = Value::Null;
    let (state, detail) = match &report.state {
        StepState::Applicable => ("applicable", Value::Null),
        StepState::NotApplicable {
            reason,
            move_above: above,
        } => {
            move_above = json!(above);
            ("not-applicable", json!(reason))
        }
        StepState::Off => ("off", Value::Null),
        StepState::Unavailable { request: r } => ("unavailable", request(&r.task, &r.params)),
        StepState::Unknown => ("unknown", Value::Null),
    };
    json!({
        "index": index,
        "id": report.id,
        "state": state,
        "detail": detail,
        "move_above": move_above,
    })
}

/// `InstalledSteps`: `{"models": [...], "steps": [...]}`.
pub fn installed_json(catalog: &Catalog) -> String {
    json!({
        "models": catalog.models.iter().map(model_json).collect::<Vec<_>>(),
        "steps": catalog
            .steps
            .iter()
            .enumerate()
            .map(|(index, report)| step_json(index, report))
            .collect::<Vec<_>>(),
    })
    .to_string()
}

/// Publish the current state, then every change, until the senders drop.
pub async fn run(
    bus: SharedBus,
    mut statuses: watch::Receiver<ChainStatus>,
    mut catalog: watch::Receiver<Catalog>,
) {
    loop {
        let (status, missing, requests) = {
            let now = statuses.borrow_and_update();
            let (status, missing) = wire(&now);
            (status, missing, missing_json(&now))
        };
        let installed = installed_json(&catalog.borrow_and_update());
        let mut bus_guard = bus.lock().await;
        // The catalog first: a reader woken by the status finds it current.
        bus_guard
            .set_property("InstalledSteps", PropertyValue::Str(installed))
            .await;
        bus_guard
            .set_property("ChainMissingRequests", PropertyValue::Str(requests))
            .await;
        bus_guard
            .set_property("ChainStatus", PropertyValue::Str(status.into()))
            .await;
        bus_guard
            .set_property("ChainMissing", PropertyValue::Str(missing))
            .await;
        drop(bus_guard);
        tokio::select! {
            r = statuses.changed() => if r.is_err() { return },
            r = catalog.changed() => if r.is_err() { return },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::pipeline::Request;
    use crate::process::wire::{Capability, ParamSpec};

    fn degraded(requests: &[Request], needs_restart: bool) -> ChainStatus {
        ChainStatus::Degraded {
            missing: requests.iter().map(Missing::of).collect(),
            needs_restart,
        }
    }

    #[test]
    fn a_chain_is_ready_degraded_or_waiting_for_a_restart() {
        assert_eq!(wire(&ChainStatus::Ready), ("ready", String::new()));
        let both = [Request::cleanup(None), Request::translate("en", "fr")];
        assert_eq!(
            wire(&degraded(&both, false)),
            ("degraded", "text.cleanup,text.translate".to_owned())
        );
        assert_eq!(wire(&degraded(&both, true)).0, "restart");
    }

    #[test]
    fn two_missing_pairs_of_one_task_name_the_task_once() {
        let pairs = [
            Request::translate("en", "fr"),
            Request::translate("en", "de"),
        ];
        let status = degraded(&pairs, false);
        assert_eq!(wire(&status).1, "text.translate");
        let json: Value = serde_json::from_str(&missing_json(&status)).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 2);
        assert_eq!(json[0]["params"]["to"], "fr");
        assert_eq!(json[1]["params"]["to"], "de");
    }

    #[test]
    fn a_ready_chain_misses_nothing() {
        assert_eq!(missing_json(&ChainStatus::Ready), "[]");
    }

    fn candidate(share: &str, task: &str, class: Option<&str>) -> Candidate {
        Candidate {
            via: Via::Myna,
            socket: "/x".into(),
            share: share.into(),
            capability: Capability {
                id: format!("{share}-model"),
                task: task.into(),
                class: class.map(Into::into),
                covers: [("to".to_owned(), vec!["fr".to_owned()])].into(),
                params: vec![ParamSpec {
                    name: "language".into(),
                    kind: "language".into(),
                    role: "source".into(),
                    required: true,
                }],
                quality: Some(0.7),
                latency_ms: Some(11),
                download_bytes: Some(42),
                ..Capability::default()
            },
        }
    }

    #[test]
    fn installed_steps_name_every_share_with_class_params_and_values() {
        let catalog = Catalog {
            models: vec![
                candidate("myna-text", "text.cleanup", None),
                candidate("myna-translation", "text.translate", Some("translate")),
            ],
            steps: vec![
                StepReport {
                    id: "tidy".into(),
                    state: StepState::Applicable,
                },
                StepReport {
                    id: "translate".into(),
                    state: StepState::Unavailable {
                        request: Request::translate("en", "fr"),
                    },
                },
                StepReport {
                    id: "script".into(),
                    state: StepState::NotApplicable {
                        reason: "Applies when the text is Chinese".into(),
                        move_above: Some(1),
                    },
                },
            ],
        };
        let v: Value = serde_json::from_str(&installed_json(&catalog)).unwrap();
        let models = v["models"].as_array().unwrap();
        assert_eq!(models[0]["share"], "myna-text");
        assert_eq!(models[0]["class"], "edit");
        assert_eq!(models[1]["share"], "myna-translation");
        assert_eq!(models[1]["class"], "translate");
        assert_eq!(models[1]["label_key"], "text.translate");
        assert_eq!(models[1]["quality"], 0.7);
        assert_eq!(models[1]["download_bytes"], 42);
        let params = models[1]["params"].as_array().unwrap();
        assert_eq!(params[0]["name"], "language");
        assert_eq!(params[0]["role"], "source");
        assert_eq!(params[1]["name"], "to");
        assert_eq!(params[1]["values"][0], "fr");
        let steps = v["steps"].as_array().unwrap();
        assert_eq!(steps[0]["state"], "applicable");
        assert_eq!(steps[1]["state"], "unavailable");
        assert_eq!(steps[1]["detail"]["task"], "text.translate");
        assert_eq!(steps[1]["detail"]["params"]["to"], "fr");
        assert_eq!(steps[2]["state"], "not-applicable");
        assert_eq!(steps[2]["index"], 2);
        assert_eq!(steps[2]["move_above"], 1);
        assert!(steps[0]["move_above"].is_null());
        assert_eq!(steps[2]["detail"], "Applies when the text is Chinese");
    }

    #[test]
    fn an_unknown_step_is_reported_as_such() {
        let catalog = Catalog {
            models: vec![],
            steps: vec![StepReport {
                id: "bogus".into(),
                state: StepState::Unknown,
            }],
        };
        let v: Value = serde_json::from_str(&installed_json(&catalog)).unwrap();
        assert_eq!(v["steps"][0]["state"], "unknown");
        assert_eq!(v["models"], json!([]));
    }
}
