//! The models installed on this machine as Diagnostics lists them, to free
//! disk: each speech family with everything its snap holds, and each text
//! model component, with its size on disk as snapd reports it and how to
//! remove it. GTK-free.

use crate::diagnostics::InstalledSnap;
use crate::plugin_catalog::{self, ComponentRequest, Entry};
use crate::processing_steps::language_name;

/// What Diagnostics knows when it lists the models.
pub struct Inputs<'a> {
    /// Every installed snap, as `GET /v2/snaps` lists it.
    pub snaps: &'a [InstalledSnap],
    /// The speech model snaps discovery found.
    pub speech: &'a [String],
    /// The speech model snap Myna is connected to, if any.
    pub connected: Option<&'a str>,
    /// The text plugin catalog, which names each text model.
    pub catalog: &'a [Entry],
}

/// How a model leaves the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Removal {
    /// The speech family's snap, components and all.
    Snap(String),
    /// Components of a text snap, the snap staying.
    Components(ComponentRequest),
}

/// One installed model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    /// Stable across reads: the snap, and the component for a text model.
    pub id: String,
    pub title: String,
    /// What it is: speech recognition, a translation, a language.
    pub kind: String,
    /// Bytes on disk.
    pub bytes: u64,
    /// The connected speech model, which cannot go while Myna uses it.
    pub in_use: bool,
    pub removal: Removal,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Models {
    pub speech: Vec<Model>,
    pub text: Vec<Model>,
}

impl Models {
    /// Bytes every listed model uses on disk.
    pub fn total(&self) -> u64 {
        self.speech.iter().chain(&self.text).map(|m| m.bytes).sum()
    }

    pub fn find(&self, id: &str) -> Option<&Model> {
        self.speech.iter().chain(&self.text).find(|m| m.id == id)
    }
}

pub fn models(inputs: &Inputs) -> Models {
    Models {
        speech: speech_models(inputs),
        text: text_models(inputs),
    }
}

fn speech_models(inputs: &Inputs) -> Vec<Model> {
    inputs
        .snaps
        .iter()
        .filter(|snap| inputs.speech.contains(&snap.name))
        .map(|snap| {
            let installed = || snap.components.iter().filter(|c| c.installed());
            let gpu = installed().any(|c| c.name.starts_with("gpu-"));
            Model {
                id: snap.name.clone(),
                title: crate::model_family::model_family(&snap.name).name,
                kind: if gpu {
                    gettextrs::gettext("Speech recognition, with its GPU engine")
                } else {
                    gettextrs::gettext("Speech recognition")
                },
                bytes: snap.installed_size + installed().map(|c| c.installed_size).sum::<u64>(),
                in_use: inputs.connected == Some(snap.name.as_str()),
                removal: Removal::Snap(snap.name.clone()),
            }
        })
        .collect()
}

fn text_models(inputs: &Inputs) -> Vec<Model> {
    let mut models = Vec::new();
    for snap in inputs
        .snaps
        .iter()
        .filter(|snap| plugin_catalog::SNAPS.contains(&snap.name.as_str()))
    {
        for component in snap.components.iter().filter(|c| c.installed()) {
            let entry = inputs
                .catalog
                .iter()
                .find(|e| e.snap == snap.name && e.component == component.name);
            let (title, kind) = match entry {
                Some(entry) => named(entry),
                None => (
                    if component.summary.is_empty() {
                        component.name.clone()
                    } else {
                        component.summary.clone()
                    },
                    gettextrs::gettext("Text model"),
                ),
            };
            let request = match entry {
                Some(entry) => plugin_catalog::requests(&[entry])
                    .into_iter()
                    .next()
                    .expect("one entry makes one request"),
                None => ComponentRequest {
                    snap: snap.name.clone(),
                    components: vec![component.name.clone()],
                    models: Vec::new(),
                    expected_bytes: 0,
                },
            };
            models.push(Model {
                id: format!("{}+{}", snap.name, component.name),
                title,
                kind,
                bytes: component.installed_size,
                in_use: false,
                removal: Removal::Components(request),
            });
        }
    }
    models
}

/// A text model's title and what it is, from its catalog entry.
fn named(entry: &Entry) -> (String, String) {
    if entry.step.id == "translate" {
        // TRANSLATORS: {from} and {to} are language names, such as "English" and "Français".
        let frame = gettextrs::gettext("{from} to {to}");
        let title = frame
            .replace("{from}", &language_name(&entry.in_lang))
            .replace("{to}", &language_name(&entry.out_lang));
        return (title, gettextrs::gettext("Translation"));
    }
    let title = plugin_catalog::label(&entry.step.label_key).unwrap_or_else(|| entry.model.clone());
    (title, language_name(&entry.in_lang))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::SnapComponent;

    fn component(name: &str, bytes: Option<u64>) -> SnapComponent {
        SnapComponent {
            name: name.to_owned(),
            summary: format!("{name} summary"),
            installed_size: bytes.unwrap_or(0),
            install_date: bytes.map(|_| "2026-11-23T22:52:13Z".to_owned()),
        }
    }

    fn snap(name: &str, bytes: u64, components: Vec<SnapComponent>) -> InstalledSnap {
        InstalledSnap {
            name: name.to_owned(),
            version: "1".to_owned(),
            installed_size: bytes,
            components,
        }
    }

    /// Shaped as the shipped catalogs are: `task` is `text.<job>`.
    fn entry(snap: &str, component: &str, step: &str, from: &str, to: &str) -> Entry {
        serde_json::from_value(serde_json::json!({
            "id": component, "snap": snap, "component": component,
            "model": format!("{component}-model"), "task": format!("text.{step}"),
            "step": {"id": step, "stage": "text", "label_key": format!("step.{step}.name"),
                     "description_key": "", "example_key": ""},
            "in_lang": from, "out_lang": to, "download_bytes": 7,
        }))
        .unwrap()
    }

    fn machine() -> Vec<InstalledSnap> {
        vec![
            snap("firefox", 300, Vec::new()),
            snap("myna", 16, Vec::new()),
            snap(
                "myna-parakeet",
                46,
                vec![
                    component("gpu-nvidia", None),
                    component("model-parakeet-int8", Some(729)),
                ],
            ),
            snap(
                "myna-text",
                52,
                vec![component("model-disfluency-en", Some(42))],
            ),
            snap(
                "myna-translation",
                28,
                vec![
                    component("model-translate-af-en", None),
                    component("model-translate-en-it", Some(33)),
                    component("model-translate-xx-yy", Some(5)),
                ],
            ),
            snap(
                "myna-whisper",
                124,
                vec![
                    component("gpu-nvidia", Some(4000)),
                    component("model-base", Some(145)),
                    component("model-small", None),
                ],
            ),
        ]
    }

    fn catalog() -> Vec<Entry> {
        vec![
            entry("myna-text", "model-disfluency-en", "tidy", "en", ""),
            entry(
                "myna-translation",
                "model-translate-en-it",
                "translate",
                "en",
                "it",
            ),
        ]
    }

    fn listed() -> Models {
        let snaps = machine();
        let speech = ["myna-parakeet".to_owned(), "myna-whisper".to_owned()];
        let catalog = catalog();
        models(&Inputs {
            snaps: &snaps,
            speech: &speech,
            connected: Some("myna-parakeet"),
            catalog: &catalog,
        })
    }

    #[test]
    fn a_speech_family_counts_its_snap_and_installed_components() {
        let models = listed();
        let ids: Vec<_> = models.speech.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            ["myna-parakeet", "myna-whisper"],
            "found by discovery only"
        );
        assert_eq!(
            models.speech[0].bytes,
            46 + 729,
            "an uninstalled engine weighs nothing"
        );
        assert_eq!(models.speech[1].bytes, 124 + 4000 + 145);
        assert_eq!(models.speech[0].title, "Parakeet");
        assert_eq!(
            models.speech[0].removal,
            Removal::Snap("myna-parakeet".into())
        );
    }

    #[test]
    fn a_family_says_when_it_holds_a_gpu_engine() {
        let models = listed();
        assert_eq!(models.speech[0].kind, "Speech recognition");
        assert_eq!(
            models.speech[1].kind,
            "Speech recognition, with its GPU engine"
        );
    }

    #[test]
    fn only_the_connected_family_is_in_use() {
        let models = listed();
        assert!(models.speech[0].in_use);
        assert!(!models.speech[1].in_use);
        assert!(
            models.text.iter().all(|m| !m.in_use),
            "a chain's model can go"
        );
    }

    #[test]
    fn each_installed_text_component_is_a_model() {
        let models = listed();
        let ids: Vec<_> = models.text.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "myna-text+model-disfluency-en",
                "myna-translation+model-translate-en-it",
                "myna-translation+model-translate-xx-yy",
            ]
        );
        assert_eq!(
            models.text[1].bytes, 33,
            "the component alone, not its snap"
        );
    }

    #[test]
    fn a_text_model_is_named_from_the_catalog() {
        let models = listed();
        assert_eq!(models.text[0].title, "Tidy up speech");
        assert_eq!(models.text[0].kind, "English");
        assert_eq!(models.text[1].title, "English to Italiano");
        assert_eq!(models.text[1].kind, "Translation");
    }

    #[test]
    fn a_component_the_catalog_lacks_is_named_by_snapd() {
        let models = listed();
        assert_eq!(models.text[2].title, "model-translate-xx-yy summary");
        assert_eq!(models.text[2].kind, "Text model");
        let mut bare = machine();
        bare[4].components[2].summary.clear();
        let catalog = catalog();
        let bare = super::models(&Inputs {
            snaps: &bare,
            speech: &[],
            connected: None,
            catalog: &catalog,
        });
        assert_eq!(bare.text[2].title, "model-translate-xx-yy");
    }

    #[test]
    fn a_text_model_is_removed_by_its_component() {
        let models = listed();
        let Removal::Components(request) = &models.text[1].removal else {
            panic!("{:?}", models.text[1].removal);
        };
        assert_eq!(request.snap, "myna-translation");
        assert_eq!(request.components, ["model-translate-en-it"]);
        assert_eq!(
            request.models,
            ["model-translate-en-it-model"],
            "waited on after"
        );
        let Removal::Components(unknown) = &models.text[2].removal else {
            panic!("{:?}", models.text[2].removal);
        };
        assert_eq!(unknown.components, ["model-translate-xx-yy"]);
        assert!(unknown.models.is_empty());
    }

    #[test]
    fn the_total_adds_every_listed_model() {
        let models = listed();
        assert_eq!(
            models.total(),
            (46 + 729) + (124 + 4000 + 145) + 42 + 33 + 5
        );
        assert_eq!(models.find("myna-whisper").map(|m| m.bytes), Some(4269));
        assert_eq!(models.find("firefox"), None);
        assert_eq!(Models::default().total(), 0);
    }
}
