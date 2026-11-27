//! The models installed on this machine as Diagnostics lists them, to free
//! disk: each speech family with everything its snap holds, and each text
//! model component, with its size on disk as snapd reports it and how to
//! remove it. GTK-free.

use crate::diagnostics::InstalledSnap;
use crate::languages::{language_in_text, language_name};
use crate::plugin_catalog::{self, ComponentRequest, Entry};

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
            .replace("{from}", &language_in_text(&entry.in_lang))
            .replace("{to}", &language_in_text(&entry.out_lang));
        return (title, gettextrs::gettext("Translation"));
    }
    let title = plugin_catalog::label(&entry.step.label_key).unwrap_or_else(|| entry.model.clone());
    let languages = if entry.languages.is_empty() {
        language_name(&entry.in_lang)
    } else {
        let names: Vec<String> = entry.languages.iter().map(|l| language_name(l)).collect();
        names.join(", ")
    };
    (title, languages)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tidy(languages: &[&str]) -> Entry {
        serde_json::from_value(serde_json::json!({
            "id": "model-tidy", "snap": "myna-text", "component": "model-tidy",
            "model": "m-tidy", "task": "text.cleanup", "in_lang": "en", "languages": languages,
            "step": {
                "id": "tidy", "stage": "clean-up", "label_key": "step.tidy.name",
                "description_key": "step.tidy.description", "example_key": "step.tidy.example",
            },
        }))
        .unwrap()
    }

    #[test]
    fn a_multilingual_text_model_names_every_language_it_reads() {
        assert_eq!(named(&tidy(&[])).1, language_name("en"));
        let names = ["en", "fr", "de", "es"].map(language_name).join(", ");
        assert_eq!(named(&tidy(&["en", "fr", "de", "es"])).1, names);
    }
}
