//! What the Processing page shows, GTK-free: one row per step in stage
//! order, each with the state it is in and what it needs. Rows come from the
//! merged catalogs and what the daemon offers, not from a compiled list, so a
//! plugin's step appears without a Settings change.

use std::collections::{BTreeMap, BTreeSet};

use myna_core::chains::ChainStep;
use myna_core::processing::ChineseScript;

use crate::chain_solve::{why_text, Why, World};
use crate::languages::{catalog_code, language_in_text, Source};

use crate::plugin_catalog::{self, Entry};
// The Translate picker and the catalog's language page list the same
// languages with the same standing, so the one computation lives there.
use crate::step_catalog::{languages, Language, LanguageInputs, LanguageState};

/// The one step with a parameter Settings renders itself.
pub const TRANSLATE: &str = "translate";
pub const TRANSLATE_TO: &str = "to";

/// The steps the client carries itself, in no catalog.
pub const SCRIPT: &str = "script";
pub const SCRIPT_MODE: &str = "mode";

/// What the daemon says about the models it offers and the steps it compiled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Offered {
    pub models: Vec<String>,
    /// `(language, to)` of every translation model offered, catalogued or not.
    pub translations: Vec<(String, String)>,
    /// Per enabled step id, what the compiler made of it.
    pub steps: BTreeMap<String, DaemonStep>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonStep {
    Applicable,
    NotApplicable,
    /// In the graph, but no model answers its request.
    Unavailable,
    Unknown,
}

impl Offered {
    /// An `InstalledSteps` value; `None` for a daemon too old to publish it.
    pub fn parse(json: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(json).ok()?;
        let models = value.get("models")?.as_array()?;
        let mut offered = Self::default();
        for model in models {
            if let Some(id) = model.get("model").and_then(|m| m.as_str()) {
                offered.models.push(id.to_owned());
            }
            if model.get("task").and_then(|t| t.as_str()) == Some("text.translate") {
                let covered = |name: &str| {
                    model
                        .get("params")?
                        .as_array()?
                        .iter()
                        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some(name))?
                        .get("values")?
                        .as_array()?
                        .first()?
                        .as_str()
                        .map(str::to_owned)
                };
                if let Some((from, to)) = covered("language").zip(covered("to")) {
                    offered.translations.push((from, to));
                }
            }
        }
        for step in value.get("steps").and_then(|s| s.as_array())? {
            let Some(id) = step.get("id").and_then(|i| i.as_str()) else {
                continue;
            };
            let state = match step.get("state").and_then(|s| s.as_str()) {
                Some("applicable") => DaemonStep::Applicable,
                Some("not-applicable") => DaemonStep::NotApplicable,
                Some("unavailable") => DaemonStep::Unavailable,
                _ => DaemonStep::Unknown,
            };
            offered.steps.insert(id.to_owned(), state);
        }
        Some(offered)
    }

    pub(crate) fn offers(&self, model: &str) -> bool {
        self.models.iter().any(|offered| offered == model)
    }
}

/// What a step needs before it can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Need {
    /// "the French model", for the row's subtitle.
    pub what: String,
    /// The catalog entries that provide it, not yet offered.
    pub entries: Vec<Entry>,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Runs when switched on.
    Ready,
    /// Skipped because of the text at this point; no fault. The reason shows.
    NotApplicable(String),
    /// Enabled, but a model is missing: the step is off until it is there.
    Missing(Need),
    /// No model, installed or offered, does what it asks: it cannot run
    /// and nothing can be installed for it. The reason shows.
    Unavailable(String),
    /// Its model is being installed, with snapd's percentage when known.
    Installing(Option<u8>),
    /// Installed; the daemon has not bound it yet.
    Preparing,
    /// No daemon answers: nothing is known about what it has.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslateRow {
    pub targets: Vec<Language>,
    pub selected: Option<usize>,
}

/// One step as the Processing page shows it, wherever it shows it: the
/// editor's rows, the catalog's, and the picker's. Built from the step as
/// the chain holds it, and the text it would read there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub description: String,
    /// The line under the title: what is wrong, else what the step does.
    pub subtitle: String,
    pub on: bool,
    pub standing: Standing,
    pub translate: Option<TranslateRow>,
    pub script: Option<ScriptRow>,
    /// Where it sits in the chain being edited; empty elsewhere.
    pub place: Place,
    /// The catalog entry this row adds or installs, when it stands for one.
    pub entry: Option<Box<Entry>>,
    /// A worked example, for the catalog's expanded row.
    pub example: Option<String>,
    /// Size, quality and licence as `(label, text)` pairs.
    pub details: Vec<(String, String)>,
    /// The installed entry the user may take away from here.
    pub removable: Option<Box<Entry>>,
}

/// How the Chinese script step writes Chinese: the modes it offers and the
/// one chosen, or the one a switch-on would choose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptRow {
    /// `(nick, name)`, in the order the combo lists them.
    pub modes: Vec<(String, String)>,
    pub selected: usize,
}

/// What the editor knows about a step's place that no other view does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Place {
    pub index: usize,
    /// The language read and left: "EN \u{2192} FR", "EN", or empty.
    pub badge: String,
    /// The step to move this one above to make it run, and its title.
    pub fix: Option<(usize, String)>,
    pub skipped: bool,
    /// Which moves keep every translation of the chain doable.
    pub moves: Moves,
    /// What the solver runs before this step, such as "Converts to Traditional first".
    pub inserted: String,
    /// What it runs after the step, such as "Then converts to Traditional".
    pub inserted_after: String,
}

/// The moves of a step that leave no translation without a model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Moves {
    pub up: bool,
    pub down: bool,
    pub top: bool,
    pub bottom: bool,
}

/// A download size in words.
pub fn size(bytes: u64) -> String {
    // TRANSLATORS: {n} is a size in megabytes, such as "24".
    let frame = gettextrs::gettext("{n} MB");
    frame.replace("{n}", &bytes.div_ceil(1_000_000).max(1).to_string())
}

/// What a row needs besides the step it shows and the text it would read.
pub struct Inputs<'a> {
    /// The `language` setting is empty: the model decides.
    pub language_unset: bool,
    /// The text the step would read.
    pub source: &'a Source,
    pub catalog: &'a [Entry],
    /// `None` when no daemon runs: nothing is known about the models.
    pub daemon: Option<&'a Offered>,
    /// Steps whose install runs now, with snapd's percentage.
    pub installing: &'a BTreeMap<String, Option<u8>>,
    /// Steps installed whose models the daemon has not offered yet.
    pub preparing: &'a BTreeSet<String>,
    pub locales: &'a [String],
}

/// The row for `step` as a chain holds it, read with the text in `source`:
/// what it is, whether its switch is on and what that switch would need.
pub fn row(step: &ChainStep, source: &Source, inputs: &Inputs) -> Row {
    let id = step.step.as_str();
    let own = inputs.catalog.iter().find(|e| e.step.id == id);
    let title = own
        .and_then(|e| plugin_catalog::label(&e.step.label_key))
        .or_else(|| (id == SCRIPT).then(|| gettextrs::gettext("Chinese script")))
        .unwrap_or_else(|| id.to_owned());
    let description = own
        .and_then(|e| plugin_catalog::label(&e.step.description_key))
        .or_else(|| {
            (id == SCRIPT).then(|| {
                gettextrs::gettext(
                    "Types Chinese in the characters you choose. Other text is left alone.",
                )
            })
        })
        .unwrap_or_default();
    let translate = (id == TRANSLATE).then(|| translate_row(step, source, inputs));
    let script = (id == SCRIPT).then(|| script_row(step, inputs));
    let standing = standing(step, translate.as_ref(), source, inputs);
    let subtitle = match &standing {
        Standing::NotApplicable(reason) | Standing::Unavailable(reason) => reason.clone(),
        Standing::Missing(need) => {
            // TRANSLATORS: {what} names a model to install, such as "the French model"; {size} is its download size.
            let frame = gettextrs::gettext("Needs {what} ({size})");
            frame
                .replace("{what}", &need.what)
                .replace("{size}", &size(need.bytes))
        }
        Standing::Installing(_) if !description.is_empty() => description.clone(),
        Standing::Installing(_) | Standing::Preparing => {
            gettextrs::gettext("Getting the model ready")
        }
        Standing::Ready | Standing::Unknown => description.clone(),
    };
    Row {
        id: step.step.clone(),
        title,
        description,
        subtitle,
        on: step.on,
        standing,
        translate,
        script,
        place: Place::default(),
        entry: None,
        example: None,
        details: Vec::new(),
        removable: None,
    }
}

/// The modes the step offers; "as recognised" is the step being off.
const SCRIPT_MODES: [ChineseScript; 4] = [
    ChineseScript::Simplified,
    ChineseScript::Traditional,
    ChineseScript::TraditionalTaiwan,
    ChineseScript::TraditionalHongKong,
];

/// The mode a Chinese user most likely wants, from the desktop's locales.
pub fn default_script(locales: &[String]) -> ChineseScript {
    let first = locales
        .iter()
        .map(|locale| {
            let name = locale.split(['.', '@']).next().unwrap_or_default();
            name.replace('_', "-").to_ascii_lowercase()
        })
        .find(|locale| myna_core::chains::may_be_chinese(Some(locale)))
        .unwrap_or_default();
    if first.ends_with("-tw") || first.contains("-hant-tw") {
        ChineseScript::TraditionalTaiwan
    } else if first.ends_with("-hk") || first.ends_with("-mo") {
        ChineseScript::TraditionalHongKong
    } else if first.contains("hant") {
        ChineseScript::Traditional
    } else {
        ChineseScript::Simplified
    }
}

fn script_row(step: &ChainStep, inputs: &Inputs) -> ScriptRow {
    let chosen = step
        .params
        .get(SCRIPT_MODE)
        .and_then(|nick| SCRIPT_MODES.iter().position(|mode| mode.nick() == nick));
    let selected = chosen.unwrap_or_else(|| {
        let default = default_script(inputs.locales);
        SCRIPT_MODES.iter().position(|m| *m == default).unwrap_or(0)
    });
    ScriptRow {
        modes: SCRIPT_MODES
            .iter()
            .map(|mode| {
                (
                    mode.nick().to_owned(),
                    crate::myna_settings::choice_display_label(mode.nick()),
                )
            })
            .collect(),
        selected,
    }
}

/// The language `step` translates into, when it holds one.
fn target_of(step: &ChainStep) -> Option<&str> {
    step.params
        .get(TRANSLATE_TO)
        .map(String::as_str)
        .filter(|to| !to.is_empty())
}

fn translate_row(step: &ChainStep, source: &Source, inputs: &Inputs) -> TranslateRow {
    let Source::One(from) = source else {
        return TranslateRow {
            targets: Vec::new(),
            selected: None,
        };
    };
    let likely: Vec<String> = inputs
        .locales
        .iter()
        .map(|locale| catalog_code(locale))
        .collect();
    let chosen = target_of(step).filter(|to| *to != from);
    let targets = languages(&LanguageInputs {
        catalog: inputs.catalog,
        daemon: inputs.daemon,
        from: source,
        // A step's install is the row's standing, not the target's.
        installing: &BTreeMap::new(),
        preparing: &BTreeSet::new(),
        likely: &likely,
        // The editor dims a target that would strand a later translation
        // rather than leaving it out, so its picker keeps what it had.
        strands: &|_| false,
        chosen,
    });
    let selected = chosen.and_then(|to| targets.iter().position(|t| t.code == to));
    TranslateRow { targets, selected }
}

fn standing(
    step: &ChainStep,
    translate: Option<&TranslateRow>,
    source: &Source,
    inputs: &Inputs,
) -> Standing {
    let id = step.step.as_str();
    let language = match source {
        Source::One(from) => Some(from.as_str()),
        Source::Unknown => None,
    };
    let own: Vec<&Entry> = inputs.catalog.iter().filter(|e| e.step.id == id).collect();
    if id == TRANSLATE && language.is_none() {
        return Standing::NotApplicable(gettextrs::gettext("Needs a single dictation language"));
    }
    if let Some(from) = language {
        let world = World::new(inputs.catalog, inputs.daemon);
        match world.skip(step, from) {
            Some(Why::AlreadyInTarget) if !step.on => {}
            Some(Why::NoTarget | Why::NoScript | Why::NoModel { .. }) | None => {}
            Some(why) => return Standing::NotApplicable(why_text(&why)),
        }
    }
    if id == SCRIPT {
        return Standing::Ready;
    }
    if let Some(percent) = inputs.installing.get(id) {
        return Standing::Installing(*percent);
    }
    if let (Some(target), Source::One(from)) =
        (translate.and_then(|t| t.targets.get(t.selected?)), source)
    {
        if matches!(target.state, LanguageState::NoModel) {
            // TRANSLATORS: {from} and {to} are language names, such as "فارسی" and "Français".
            let frame = gettextrs::gettext("No model translates {from} to {to} yet");
            return Standing::Unavailable(
                frame
                    .replace("{from}", &language_in_text(from))
                    .replace("{to}", &target.name),
            );
        }
    }
    if inputs.daemon.is_none() {
        return Standing::Unknown;
    }
    let Some(need) = need(step, translate, &own, source, inputs) else {
        return Standing::Ready;
    };
    if inputs.preparing.contains(id) {
        Standing::Preparing
    } else {
        Standing::Missing(need)
    }
}

/// What an enabled step lacks, when the daemon runs and says it lacks it.
fn need(
    step: &ChainStep,
    translate: Option<&TranslateRow>,
    own: &[&Entry],
    source: &Source,
    inputs: &Inputs,
) -> Option<Need> {
    let id = step.step.as_str();
    let daemon = inputs.daemon?;
    if id == TRANSLATE {
        let target = translate?
            .targets
            .iter()
            .find(|t| Some(t.code.as_str()) == target_of(step))?;
        if matches!(target.state, LanguageState::Ready) {
            return None;
        }
        let Source::One(from) = source else {
            return None;
        };
        let legs = World::new(inputs.catalog, inputs.daemon).route(from, &target.code)?;
        let entries: Vec<Entry> = legs
            .into_iter()
            .filter(|leg| !daemon.offers(&leg.model))
            .cloned()
            .collect();
        // TRANSLATORS: {language} is a language name; this completes "Needs the French model".
        let frame = gettextrs::gettext("the {language} model");
        return Some(Need {
            what: frame.replace("{language}", &target.name),
            bytes: entries.iter().map(|e| e.download_bytes).sum(),
            entries,
        });
    }
    let trusts_daemon = daemon.steps.get(id);
    let lacking = match trusts_daemon {
        Some(DaemonStep::Unavailable) => true,
        Some(_) => false,
        None => !own.is_empty() && own.iter().all(|e| !daemon.offers(&e.model)),
    };
    if !lacking {
        return None;
    }
    let entry = (*own.first()?).clone();
    let title = plugin_catalog::label(&entry.step.label_key).unwrap_or_else(|| id.to_owned());
    // TRANSLATORS: {step} is a step's name; this completes "Needs the Tidy up speech model".
    let frame = gettextrs::gettext("the {step} model");
    Some(Need {
        what: frame.replace("{step}", &title),
        bytes: entry.download_bytes,
        entries: vec![entry],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_language_setting_reads_as_one_language_or_none() {
        assert_eq!(Source::of_setting(""), Source::One("en".into()));
        assert_eq!(Source::of_setting("fr-CA"), Source::One("fr".into()));
        assert_eq!(
            Source::of_setting("zh_Hans_CN"),
            Source::One("zh-Hans".into())
        );
        assert_eq!(Source::of_setting("auto"), Source::Unknown);
        assert_eq!(Source::of_setting("en fr"), Source::Unknown);
    }

    #[test]
    fn the_daemon_s_catalog_is_read_and_an_old_daemon_has_none() {
        let json = serde_json::json!({
            "models": [
                {"share": "s", "model": "m-en-fr", "task": "text.translate",
                 "params": [{"name": "language", "values": ["en"]}, {"name": "to", "values": ["fr"]}]},
                {"model": "m-tidy-en", "task": "text.cleanup"},
            ],
            "steps": [
                {"id": "tidy", "state": "applicable"},
                {"id": "translate", "state": "unavailable", "detail": {}},
                {"id": "x", "state": "not-applicable"},
                {"id": "y", "state": "odd"},
            ],
        })
        .to_string();
        let offered = Offered::parse(&json).unwrap();
        assert_eq!(offered.models, ["m-en-fr", "m-tidy-en"]);
        assert_eq!(offered.translations, [("en".to_owned(), "fr".to_owned())]);
        assert_eq!(offered.steps["tidy"], DaemonStep::Applicable);
        assert_eq!(offered.steps["translate"], DaemonStep::Unavailable);
        assert_eq!(offered.steps["x"], DaemonStep::NotApplicable);
        assert_eq!(offered.steps["y"], DaemonStep::Unknown);
        assert_eq!(Offered::parse(""), None);
        assert_eq!(Offered::parse("{}"), None);
    }

    #[test]
    fn a_script_step_with_no_script_chosen_still_says_it_wants_chinese() {
        let world = World::new(&[], None);
        let unset = ChainStep::new(SCRIPT);
        assert_eq!(world.skip(&unset, "fr"), Some(Why::NotChinese));
        assert_eq!(world.skip(&unset, "zh"), Some(Why::NoScript));
    }

    #[test]
    fn sizes_round_up_to_whole_megabytes() {
        assert_eq!(size(1), "1 MB");
        assert_eq!(size(24_000_001), "25 MB");
        assert_eq!(size(33_353_728), "34 MB");
    }
}
