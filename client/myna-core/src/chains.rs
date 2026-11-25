//! Saved chains: named, ordered lists of steps, the one in use, and the
//! fit-to-field switch that runs after any of them.
//!
//! Stored as three keys (`chains`, `active-chain`, `fit-to-field`) that
//! [`write`] sets in one delayed apply, so a watcher on `change-event` wakes
//! once per change. A chain's id never changes, its name may, so renaming
//! keeps the active pointer. Each step carries its own parameters, keyed by
//! the bare parameter name (`to`): a chain may hold one step twice, which a
//! flat `<step>.<param>` map could not tell apart.

use std::collections::BTreeMap;

use gio::glib::{self, prelude::ObjectExt, value::ToValue, variant::ToVariant};
use gio::prelude::SettingsExt;

use crate::processing::{in_stage_order, param_key, Params};

pub const KEY_CHAINS: &str = "chains";
pub const KEY_ACTIVE_CHAIN: &str = "active-chain";
pub const KEY_FIT_TO_FIELD: &str = "fit-to-field";

/// Every key this module owns.
pub const KEYS: [&str; 3] = [KEY_CHAINS, KEY_ACTIVE_CHAIN, KEY_FIT_TO_FIELD];

/// The shipped example's id. Its content is a placeholder until
/// `chains-showcase-example` picks it.
pub const EXAMPLE_ID: &str = "example";

/// The id of the chain the step-list migration makes.
pub const MIGRATED_ID: &str = "my-steps";

/// A step's parameters by bare name: `to` for a translation's target.
pub type StepParams = BTreeMap<String, String>;

/// The stored shape: `a(ssa(sba{ss}))`.
type Raw = Vec<(String, String, Vec<(String, bool, StepParams)>)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainStep {
    pub step: String,
    pub on: bool,
    pub params: StepParams,
}

impl ChainStep {
    pub fn new(step: &str) -> Self {
        Self {
            step: step.to_owned(),
            on: true,
            params: StepParams::new(),
        }
    }

    pub fn with(mut self, param: &str, value: &str) -> Self {
        self.params.insert(param.to_owned(), value.to_owned());
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chain {
    pub id: String,
    pub name: String,
    pub steps: Vec<ChainStep>,
}

/// Everything the Processing settings hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Processing {
    pub chains: Vec<Chain>,
    /// As stored; [`Processing::active`] reads a dangling id as none.
    pub active: Option<String>,
    pub fit_to_field: bool,
}

/// The schema defaults, and what a machine with no schema installed reads.
impl Default for Processing {
    fn default() -> Self {
        Self {
            chains: vec![Chain {
                id: EXAMPLE_ID.into(),
                name: "Tidy and translate".into(),
                steps: vec![
                    ChainStep::new("tidy"),
                    ChainStep::new("translate").with("to", "fr"),
                ],
            }],
            active: None,
            fit_to_field: true,
        }
    }
}

impl Processing {
    pub fn chain(&self, id: &str) -> Option<&Chain> {
        self.chains.iter().find(|chain| chain.id == id)
    }

    /// The chain in use; none when nothing is active or the id is gone.
    pub fn active(&self) -> Option<&Chain> {
        self.chain(self.active.as_deref()?)
    }

    /// Make `id` the chain in use, or none. False, and nothing changed,
    /// for an id no chain has.
    pub fn activate(&mut self, id: Option<&str>) -> bool {
        match id {
            Some(id) if self.chain(id).is_none() => false,
            id => {
                self.active = id.map(str::to_owned);
                true
            }
        }
    }

    pub fn rename(&mut self, id: &str, name: &str) -> bool {
        match self.chains.iter_mut().find(|chain| chain.id == id) {
            Some(chain) => {
                chain.name = name.to_owned();
                true
            }
            None => false,
        }
    }

    /// Remove `id`; when it was in use, nothing is.
    pub fn delete(&mut self, id: &str) -> bool {
        let before = self.chains.len();
        self.chains.retain(|chain| chain.id != id);
        if self.active.as_deref() == Some(id) {
            self.active = None;
        }
        self.chains.len() != before
    }

    /// Append a chain under a fresh id and return the id.
    pub fn add(&mut self, name: &str, steps: Vec<ChainStep>) -> String {
        let id = (1..)
            .map(|n| format!("chain-{n}"))
            .find(|id| self.chain(id).is_none())
            .expect("an unbounded range has a free id");
        self.chains.push(Chain {
            id: id.clone(),
            name: name.to_owned(),
            steps,
        });
        id
    }

    /// The active chain as the stage-ordered step list the daemon compiles
    /// today: its steps that are on, once each, parameters keyed
    /// `<step>.<param>` (the first of a repeated step wins, an off step's
    /// fill in, so a choice outlives its switch). The fit switch is not in
    /// it: the daemon fits after the graph, as an output stage. Goes once
    /// the compiler walks chains in order (`chains-compiler-user-order`).
    pub fn stage_steps(&self) -> (Vec<String>, Params) {
        let steps: Vec<&ChainStep> = self
            .active()
            .into_iter()
            .flat_map(|chain| &chain.steps)
            .collect();
        let mut ids = Vec::new();
        for step in steps.iter().filter(|step| step.on) {
            if !ids.contains(&step.step) {
                ids.push(step.step.clone());
            }
        }
        let mut params = Params::new();
        let (on, off): (Vec<&ChainStep>, Vec<&ChainStep>) = steps.iter().partition(|step| step.on);
        for step in on.into_iter().chain(off) {
            for (name, value) in &step.params {
                params
                    .entry(param_key(&step.step, name))
                    .or_insert_with(|| value.clone());
            }
        }
        (in_stage_order(&ids), params)
    }

    /// [`Processing::stage_steps`] with `fit` first when the switch is on:
    /// the list the step-switch page shows.
    pub fn page_steps(&self) -> (Vec<String>, Params) {
        let (mut ids, params) = self.stage_steps();
        if self.fit_to_field {
            ids.insert(0, "fit".to_owned());
        }
        (ids, params)
    }

    /// Replace the active chain's steps with a flat list and parameters, as
    /// the step-switch page edits them: `fit` sets the switch, the rest
    /// become the active chain in stage order, made as "My steps" when none
    /// is. A step with parameters but not in `ids` stays as an off step that
    /// keeps them. Repeated steps are lost: that page has none.
    pub fn set_stage_steps(&mut self, ids: &[String], params: &Params) {
        self.fit_to_field = ids.iter().any(|id| id == "fit");
        let enabled: Vec<String> = ids.iter().filter(|id| *id != "fit").cloned().collect();
        let mut all = enabled.clone();
        all.extend(
            params
                .keys()
                .filter_map(|key| key.split_once('.').map(|(step, _)| step.to_owned()))
                .filter(|step| step != "fit"),
        );
        let steps: Vec<ChainStep> = in_stage_order(&all)
            .into_iter()
            .map(|id| ChainStep {
                on: enabled.contains(&id),
                params: params
                    .iter()
                    .filter_map(|(key, value)| {
                        let name = key.strip_prefix(&format!("{id}."))?;
                        Some((name.to_owned(), value.clone()))
                    })
                    .collect(),
                step: id,
            })
            .collect();
        let active = self.active.clone().filter(|id| self.chain(id).is_some());
        match active {
            Some(id) => {
                let chain = self
                    .chains
                    .iter_mut()
                    .find(|chain| chain.id == id)
                    .expect("active() found it");
                chain.steps = steps;
            }
            None if enabled.is_empty() => {}
            None => {
                let id = self.add("My steps", steps);
                self.active = Some(id);
            }
        }
    }

    /// Read the three keys; any missing from the installed schema reads its
    /// default.
    pub fn read(settings: &gio::Settings) -> Self {
        let default = Self::default();
        let has = |key: &str| {
            settings
                .settings_schema()
                .is_some_and(|schema| schema.has_key(key))
        };
        let chains = has(KEY_CHAINS)
            .then(|| settings.value(KEY_CHAINS).get::<Raw>())
            .flatten()
            .map(from_raw)
            .unwrap_or(default.chains);
        let active = has(KEY_ACTIVE_CHAIN)
            .then(|| settings.string(KEY_ACTIVE_CHAIN).to_string())
            .filter(|id| !id.is_empty());
        let fit_to_field = if has(KEY_FIT_TO_FIELD) {
            settings.boolean(KEY_FIT_TO_FIELD)
        } else {
            default.fit_to_field
        };
        Self {
            chains,
            active,
            fit_to_field,
        }
    }

    /// Write what differs from the store in one apply: watchers on
    /// `change-event` see one event, and no reader sees half of it.
    pub fn write(&self, settings: &gio::Settings) -> Result<(), glib::BoolError> {
        let now = Self::read(settings);
        let mut writes: Vec<(&str, glib::Variant)> = Vec::new();
        if self.chains != now.chains {
            writes.push((KEY_CHAINS, to_raw(&self.chains).to_variant()));
        }
        if self.active != now.active {
            writes.push((
                KEY_ACTIVE_CHAIN,
                self.active.clone().unwrap_or_default().to_variant(),
            ));
        }
        if self.fit_to_field != now.fit_to_field {
            writes.push((KEY_FIT_TO_FIELD, self.fit_to_field.to_variant()));
        }
        if writes.is_empty() {
            return Ok(());
        }
        // Delay mode never ends on an object, so a throwaway one over the
        // same backend takes the batch.
        let schema = settings
            .settings_schema()
            .ok_or_else(|| glib::bool_error!("no schema"))?;
        let backend = settings.property::<gio::SettingsBackend>("backend");
        let path = settings.property::<String>("path");
        let batch = gio::Settings::new_full(&schema, Some(&backend), Some(&path));
        batch.delay();
        let result = writes
            .iter()
            .try_for_each(|(key, value)| batch.set_value(key, value));
        if result.is_ok() {
            batch.apply();
        }
        result
    }
}

/// Why a step does not fit where it stands in a chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Misfit {
    /// The steps that hear English words read English only.
    NotEnglish,
    /// A translation with no target.
    NoTarget,
    /// A translation into the language the text is already in.
    AlreadyInTarget,
    /// The Chinese script step reads Chinese only.
    NotChinese,
    /// The Chinese script step with no script chosen.
    NoScript,
    /// Keeping the original with no translation before it.
    NothingTranslated,
}

/// What a step makes of the text where it stands in a chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Runs, reading text in `input` and leaving it in `output` (`None`
    /// when the language is not known).
    Runs {
        input: Option<String>,
        output: Option<String>,
    },
    /// Skipped: the text there is not what it reads. `move_above` is the
    /// index of the earlier step that changed the language, when the step
    /// would fit above it.
    Skipped {
        why: Misfit,
        input: Option<String>,
        move_above: Option<usize>,
    },
    /// Switched off.
    Off,
    /// An id this build does not know.
    Unknown,
}

/// The ids of the steps this build knows, in their default insert order.
pub const KNOWN_STEPS: [&str; 8] = [
    "commands",
    "tidy",
    "emoji",
    "snippets",
    "casing",
    "translate",
    "original",
    "script",
];

/// The language code before any region: `fr` for `fr-CA`.
pub fn primary_language(tag: &str) -> String {
    tag.split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Whether text in `language` may be Chinese; an unknown language may.
pub fn may_be_chinese(language: Option<&str>) -> bool {
    language.is_none_or(|l| {
        let l = l.to_ascii_lowercase();
        ["zh", "yue", "cmn"].iter().any(|c| {
            l.strip_prefix(c)
                .is_some_and(|r| r.is_empty() || r.starts_with(['-', '_']))
        })
    })
}

/// Why step `id` with `params` does not fit text in `lang`, if it does not;
/// `translated` is whether a translation runs before it. One judgement for
/// the daemon's compiler and Settings' editor.
pub fn misfit(
    id: &str,
    params: &StepParams,
    lang: Option<&str>,
    translated: bool,
) -> Option<Misfit> {
    let param = |name: &str| params.get(name).filter(|v| !v.is_empty());
    match id {
        "commands" | "tidy" | "emoji" | "snippets" | "casing"
            if lang.is_some_and(|l| l != "en") =>
        {
            Some(Misfit::NotEnglish)
        }
        "translate" => match param("to").map(|to| primary_language(to)) {
            None => Some(Misfit::NoTarget),
            Some(to) if lang == Some(to.as_str()) => Some(Misfit::AlreadyInTarget),
            Some(_) => None,
        },
        "script" if !may_be_chinese(lang) => Some(Misfit::NotChinese),
        "script"
            if param("mode")
                .and_then(|m| crate::processing::ChineseScript::from_nick(m))
                .is_none() =>
        {
            Some(Misfit::NoScript)
        }
        "original" if !translated => Some(Misfit::NothingTranslated),
        _ => None,
    }
}

/// What each step of `steps` makes of the text, in order, for dictation in
/// `language` (`None` when the speech model decides).
pub fn fits(steps: &[ChainStep], language: Option<&str>) -> Vec<Fit> {
    let mut lang = language.map(primary_language);
    let mut changed: Option<(usize, Option<String>)> = None;
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            if !step.on {
                return Fit::Off;
            }
            if !KNOWN_STEPS.contains(&step.step.as_str()) {
                return Fit::Unknown;
            }
            let translated = changed.is_some();
            if let Some(why) = misfit(&step.step, &step.params, lang.as_deref(), translated) {
                return Fit::Skipped {
                    why,
                    input: lang.clone(),
                    move_above: changed
                        .as_ref()
                        .filter(|(_, before)| {
                            misfit(&step.step, &step.params, before.as_deref(), false).is_none()
                        })
                        .map(|(at, _)| *at),
                };
            }
            let input = lang.clone();
            if step.step == "translate" {
                let to = step.params.get("to").map(|to| primary_language(to));
                changed = Some((index, std::mem::replace(&mut lang, to)));
            }
            Fit::Runs {
                input,
                output: lang.clone(),
            }
        })
        .collect()
}

/// Where a new step `id` goes in `steps` by default: after the last step
/// that runs at or before its stage, so Add puts Tidy before Translate and
/// a script step last.
pub fn insert_position(steps: &[ChainStep], id: &str) -> usize {
    let rank = |id: &str| {
        KNOWN_STEPS
            .iter()
            .position(|known| *known == id)
            .unwrap_or(KNOWN_STEPS.len())
    };
    let wanted = rank(id);
    steps
        .iter()
        .rposition(|step| rank(&step.step) <= wanted)
        .map_or(0, |at| at + 1)
}

/// Why the active chain could not be set.
#[derive(Debug, PartialEq, Eq)]
pub enum ActivateError {
    /// No saved chain has that id.
    Unknown(String),
    /// The settings store is not installed or not writable.
    Store(String),
}

/// Make `id` (or none) the active chain in the settings store every process
/// shares, the one write the bus's `ActiveChain` does. The store stays the
/// source of truth: the daemon hears it through its watch like any edit.
pub fn activate_in_store(id: Option<&str>) -> Result<(), ActivateError> {
    let store = crate::settings::Store::open()
        .ok_or_else(|| ActivateError::Store("the settings schema is not installed".into()))?;
    activate_in(store.gio(), id)
}

/// [`activate_in_store`] over an open `gio::Settings`.
pub fn activate_in(settings: &gio::Settings, id: Option<&str>) -> Result<(), ActivateError> {
    let mut processing = Processing::read(settings);
    if !processing.activate(id) {
        return Err(ActivateError::Unknown(id.unwrap_or_default().to_owned()));
    }
    processing
        .write(settings)
        .map_err(|error| ActivateError::Store(error.to_string()))?;
    gio::Settings::sync();
    Ok(())
}

/// Call `callback` once per `change-event` of `settings`: one per batch of
/// keys written together, where `changed` fires once per key. gio-rs does
/// not bind the signal.
pub fn on_change_event(
    settings: &gio::Settings,
    callback: impl Fn(&gio::Settings) + 'static,
) -> glib::SignalHandlerId {
    settings.connect_local("change-event", false, move |args| {
        if let Some(Ok(settings)) = args.first().map(|value| value.get::<gio::Settings>()) {
            callback(&settings);
        }
        // Not handled: `changed` still fires for each key.
        Some(false.to_value())
    })
}

/// The GVariant of a chain list, for writers outside a `gio::Settings`.
pub fn chains_variant(chains: &[Chain]) -> glib::Variant {
    to_raw(chains).to_variant()
}

/// A chain list out of its GVariant; `None` when the type is wrong.
pub fn chains_from_variant(value: &glib::Variant) -> Option<Vec<Chain>> {
    value.get::<Raw>().map(from_raw)
}

fn from_raw(raw: Raw) -> Vec<Chain> {
    raw.into_iter()
        .map(|(id, name, steps)| Chain {
            id,
            name,
            steps: steps
                .into_iter()
                .map(|(step, on, params)| ChainStep { step, on, params })
                .collect(),
        })
        .collect()
}

fn to_raw(chains: &[Chain]) -> Raw {
    chains
        .iter()
        .map(|chain| {
            (
                chain.id.clone(),
                chain.name.clone(),
                chain
                    .steps
                    .iter()
                    .map(|step| (step.step.clone(), step.on, step.params.clone()))
                    .collect(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn empty() -> Processing {
        Processing {
            chains: Vec::new(),
            active: None,
            fit_to_field: false,
        }
    }

    #[test]
    fn rename_keeps_the_active_pointer() {
        let mut p = empty();
        let id = p.add("A", vec![ChainStep::new("tidy")]);
        assert!(p.activate(Some(&id)));
        assert!(p.rename(&id, "B"));
        assert_eq!(p.active().map(|c| c.name.as_str()), Some("B"));
        assert!(!p.rename("nope", "C"));
    }

    #[test]
    fn deleting_the_active_chain_falls_back_to_none() {
        let mut p = empty();
        let a = p.add("A", Vec::new());
        let b = p.add("B", Vec::new());
        assert_ne!(a, b);
        p.activate(Some(&b));
        assert!(p.delete(&a));
        assert_eq!(p.active.as_deref(), Some(b.as_str()));
        assert!(p.delete(&b));
        assert_eq!(p.active, None);
        assert!(!p.delete(&b));
    }

    #[test]
    fn a_fresh_id_never_reuses_a_live_one() {
        let mut p = empty();
        let a = p.add("A", Vec::new());
        let b = p.add("B", Vec::new());
        p.delete(&a);
        let c = p.add("C", Vec::new());
        assert_eq!(c, a, "a freed id may come back");
        assert_ne!(c, b);
    }

    #[test]
    fn activating_an_unknown_chain_changes_nothing() {
        let mut p = empty();
        let id = p.add("A", Vec::new());
        p.activate(Some(&id));
        assert!(!p.activate(Some("nope")));
        assert_eq!(p.active.as_deref(), Some(id.as_str()));
        assert!(p.activate(None));
        assert_eq!(p.active, None);
    }

    #[test]
    fn stage_steps_take_the_active_chain_on_steps_once_each() {
        let mut p = empty();
        let id = p.add(
            "A",
            vec![
                ChainStep::new("translate").with("to", "de"),
                ChainStep::new("tidy"),
                ChainStep {
                    on: false,
                    ..ChainStep::new("script").with("mode", "simplified")
                },
                ChainStep::new("translate").with("to", "fr"),
                ChainStep::new("future").with("x", "1"),
            ],
        );
        p.fit_to_field = true;
        assert_eq!(p.stage_steps(), (Vec::new(), Params::new()));
        assert_eq!(p.page_steps().0, ids(&["fit"]));
        p.activate(Some(&id));
        assert_eq!(
            p.page_steps().0,
            ids(&["fit", "tidy", "translate", "future"])
        );
        let (list, params) = p.stage_steps();
        assert_eq!(list, ids(&["tidy", "translate", "future"]));
        assert_eq!(
            params,
            Params::from([
                ("translate.to".into(), "de".into()),
                ("script.mode".into(), "simplified".into()),
                ("future.x".into(), "1".into())
            ])
        );
    }

    #[test]
    fn the_step_page_edits_the_active_chain_or_makes_one() {
        let mut p = empty();
        p.set_stage_steps(&ids(&["fit"]), &Params::new());
        assert!(p.fit_to_field);
        assert!(p.chains.is_empty(), "fit alone makes no chain");

        let params = Params::from([("translate.to".into(), "de".into())]);
        p.set_stage_steps(&ids(&["tidy", "translate"]), &params);
        assert!(!p.fit_to_field);
        let chain = p.active().expect("made and activated").clone();
        assert_eq!(chain.name, "My steps");
        assert_eq!(
            chain.steps,
            [
                ChainStep::new("tidy"),
                ChainStep::new("translate").with("to", "de")
            ]
        );
        p.set_stage_steps(&ids(&["tidy"]), &params);
        assert_eq!(p.chains.len(), 1);
        assert_eq!(
            p.active().unwrap().steps,
            [
                ChainStep::new("tidy"),
                ChainStep {
                    on: false,
                    ..ChainStep::new("translate").with("to", "de")
                }
            ],
            "a choice outlives its switch"
        );
        assert_eq!(p.page_steps(), (ids(&["tidy"]), params.clone()));
        p.set_stage_steps(&[], &Params::new());
        assert_eq!(p.chains.len(), 1, "an emptied chain stays");
        assert!(p.active().unwrap().steps.is_empty());
    }

    #[test]
    fn the_variant_round_trips_and_rejects_a_wrong_type() {
        let chains = Processing::default().chains;
        assert_eq!(chains_from_variant(&chains_variant(&chains)), Some(chains));
        assert_eq!(chains_from_variant(&"x".to_variant()), None);
    }

    fn chain(steps: &[(&str, &str)]) -> Vec<ChainStep> {
        steps
            .iter()
            .map(|(id, to)| match *to {
                "" => ChainStep::new(id),
                to if *id == "script" => ChainStep::new(id).with("mode", to),
                to => ChainStep::new(id).with("to", to),
            })
            .collect()
    }

    #[test]
    fn each_step_reads_the_language_the_one_before_it_leaves() {
        let fits = fits(&chain(&[("tidy", ""), ("translate", "fr")]), Some("en-GB"));
        assert_eq!(
            fits,
            [
                Fit::Runs {
                    input: Some("en".into()),
                    output: Some("en".into())
                },
                Fit::Runs {
                    input: Some("en".into()),
                    output: Some("fr".into())
                },
            ]
        );
        let swapped = super::fits(&chain(&[("translate", "fr"), ("tidy", "")]), Some("en"));
        assert_eq!(
            swapped[1],
            Fit::Skipped {
                why: Misfit::NotEnglish,
                input: Some("fr".into()),
                move_above: Some(0)
            }
        );
    }

    #[test]
    fn a_skip_with_no_step_to_move_above_says_so() {
        let fits = fits(&chain(&[("tidy", "")]), Some("fr"));
        assert!(matches!(
            fits[0],
            Fit::Skipped {
                why: Misfit::NotEnglish,
                move_above: None,
                ..
            }
        ));
        let away = super::fits(
            &chain(&[("translate", "en"), ("script", "simplified")]),
            Some("zh"),
        );
        assert!(matches!(
            away[1],
            Fit::Skipped {
                why: Misfit::NotChinese,
                move_above: Some(0),
                ..
            }
        ));
        let blank = super::fits(&chain(&[("translate", ""), ("script", "")]), None);
        assert!(matches!(
            blank[0],
            Fit::Skipped {
                why: Misfit::NoTarget,
                ..
            }
        ));
        assert!(matches!(
            blank[1],
            Fit::Skipped {
                why: Misfit::NoScript,
                ..
            }
        ));
        let same = super::fits(&chain(&[("translate", "en")]), Some("en"));
        assert!(matches!(
            same[0],
            Fit::Skipped {
                why: Misfit::AlreadyInTarget,
                ..
            }
        ));
    }

    #[test]
    fn off_and_unknown_steps_change_nothing_downstream() {
        let mut steps = chain(&[("translate", "fr"), ("future", ""), ("tidy", "")]);
        steps[0].on = false;
        assert_eq!(
            fits(&steps, Some("en")),
            [
                Fit::Off,
                Fit::Unknown,
                Fit::Runs {
                    input: Some("en".into()),
                    output: Some("en".into())
                }
            ]
        );
        assert!(may_be_chinese(None));
        assert!(may_be_chinese(Some("zh_TW")));
        assert!(!may_be_chinese(Some("zhx")));
    }

    #[test]
    fn keeping_the_original_needs_a_translation_before_it() {
        let after = fits(&chain(&[("translate", "fr"), ("original", "")]), Some("en"));
        assert_eq!(
            after[1],
            Fit::Runs {
                input: Some("fr".into()),
                output: Some("fr".into())
            }
        );
        for alone in [
            chain(&[("original", "")]),
            chain(&[("original", ""), ("translate", "fr")]),
            chain(&[("tidy", ""), ("original", "")]),
        ] {
            let at = alone.iter().position(|s| s.step == "original").unwrap();
            assert_eq!(
                fits(&alone, Some("en"))[at],
                Fit::Skipped {
                    why: Misfit::NothingTranslated,
                    input: Some("en".into()),
                    move_above: None
                },
                "{alone:?}"
            );
        }
        let mut skipped = chain(&[("translate", "fr"), ("original", "")]);
        skipped[0].on = false;
        assert!(matches!(
            fits(&skipped, Some("en"))[1],
            Fit::Skipped {
                why: Misfit::NothingTranslated,
                ..
            }
        ));
    }

    #[test]
    fn a_new_step_goes_in_at_its_stage() {
        let steps = chain(&[("tidy", ""), ("translate", "fr")]);
        assert_eq!(insert_position(&steps, "script"), 2);
        assert_eq!(insert_position(&steps, "tidy"), 1);
        assert_eq!(insert_position(&chain(&[("translate", "fr")]), "tidy"), 0);
        assert_eq!(insert_position(&[], "translate"), 0);
        assert_eq!(insert_position(&steps, "future"), 2);
        let translated = chain(&[("translate", "fr"), ("script", "simplified")]);
        assert_eq!(insert_position(&translated, "original"), 1);
        assert_eq!(insert_position(&translated, "emoji"), 0);
        assert_eq!(insert_position(&steps, "emoji"), 1, "after tidy");
    }
}
