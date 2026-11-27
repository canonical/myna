//! Saved chains: named, ordered lists of steps, and the one in use.
//!
//! Stored as two keys (`chains`, `active-chain`) that
//! [`write`] sets in one delayed apply, so a watcher on `change-event` wakes
//! once per change. A chain's id never changes, its name may, so renaming
//! keeps the active pointer. Each step carries its own parameters, keyed by
//! the bare parameter name (`to`): a chain may hold one step twice, which a
//! flat `<step>.<param>` map could not tell apart.

use std::collections::BTreeMap;

use gio::glib::{self, prelude::ObjectExt, value::ToValue, variant::ToVariant};
use gio::prelude::SettingsExt;

use crate::processing::{param_key, Params};

pub const KEY_CHAINS: &str = "chains";
pub const KEY_ACTIVE_CHAIN: &str = "active-chain";

/// Every key this module owns.
pub const KEYS: [&str; 2] = [KEY_CHAINS, KEY_ACTIVE_CHAIN];

/// The shipped example's id. Its content is a placeholder until
/// `chains-showcase-example` picks it.
pub const EXAMPLE_ID: &str = "example";

/// A named set of steps the Settings page offers as one choice. Not stored:
/// a choice writes its steps and parameters, and the stored list reads back
/// as the nearest preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessingChain(&'static str);

impl ProcessingChain {
    /// Nothing: typed as heard.
    pub const EXACT: Self = Self("exact");
    /// Hesitations, false starts and repeats dropped.
    pub const TIDY: Self = Self("tidy");
    /// Tidied, then translated from English to French.
    pub const TIDY_FRENCH: Self = Self("tidy-french");

    /// Every preset, in schema order.
    pub const ALL: [Self; 3] = [Self::EXACT, Self::TIDY, Self::TIDY_FRENCH];

    /// The schema enum nick.
    pub fn nick(self) -> &'static str {
        self.0
    }

    /// The preset a nick names; `None` for one this build does not know.
    pub fn from_nick(nick: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|chain| chain.nick() == nick)
    }

    /// The steps a preset enables: the first `n` of [`KNOWN_STEPS`], which
    /// is in stage order.
    pub fn steps(self) -> Vec<String> {
        let n = match self {
            Self::TIDY => 1,
            Self::TIDY_FRENCH => 2,
            _ => 0,
        };
        KNOWN_STEPS[..n].iter().map(|id| (*id).to_owned()).collect()
    }

    /// The parameters a preset sets.
    pub fn params(self) -> Params {
        match self {
            Self::TIDY_FRENCH => [(param_key("translate", "to"), "fr".to_owned())].into(),
            _ => Params::new(),
        }
    }
}

impl Default for ProcessingChain {
    fn default() -> Self {
        Self::EXACT
    }
}

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

    /// Read the two keys; any missing from the installed schema reads its
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
        Self { chains, active }
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

/// The ids of the steps this build knows, in their default insert order.
pub const KNOWN_STEPS: [&str; 3] = ["tidy", "translate", "script"];

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

    fn empty() -> Processing {
        Processing {
            chains: Vec::new(),
            active: None,
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
    fn chinese_is_recognised_by_its_code() {
        assert!(may_be_chinese(None));
        assert!(may_be_chinese(Some("zh_TW")));
        assert!(!may_be_chinese(Some("zhx")));
    }

    #[test]
    fn a_new_step_goes_in_at_its_stage() {
        let steps = chain(&[("tidy", ""), ("translate", "fr")]);
        assert_eq!(insert_position(&steps, "script"), 2);
        assert_eq!(insert_position(&steps, "tidy"), 1);
        assert_eq!(insert_position(&chain(&[("translate", "fr")]), "tidy"), 0);
        assert_eq!(insert_position(&[], "translate"), 0);
        assert_eq!(insert_position(&steps, "future"), 2);
    }
}
