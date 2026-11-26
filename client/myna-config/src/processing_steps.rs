//! What the Processing page shows, GTK-free: one row per step in stage
//! order, each with the state it is in and what it needs. Rows come from the
//! merged catalogs and what the daemon offers, not from a compiled list, so a
//! plugin's step appears without a Settings change.

use std::collections::{BTreeMap, BTreeSet};

use myna_core::processing::{in_stage_order, param_key, script_of, ChineseScript, Params};

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

/// The language a dictation is in, as far as Settings can tell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// One language, as its bare code.
    One(String),
    /// More than one, or the model decides: translation has no source.
    Unknown,
}

impl Source {
    /// From the `language` setting. Empty is English, as the daemon treats it.
    pub fn of_setting(language: &str) -> Self {
        let language = language.trim();
        if language.is_empty() {
            return Self::One("en".into());
        }
        if language.contains([',', ' ', ';']) || matches!(language, "auto" | "multi") {
            return Self::Unknown;
        }
        Self::One(catalog_code(language))
    }
}

/// The catalog's code for a locale: `fr` for `fr-CA`; `zh-Hans` stays whole,
/// as the models name it, which is why this is not
/// [`myna_core::chains::primary_language`].
fn catalog_code(tag: &str) -> String {
    let tag = tag.replace('_', "-");
    if tag.starts_with("zh-") {
        let mut parts = tag.splitn(3, '-');
        return format!("zh-{}", parts.nth(1).unwrap_or_default());
    }
    tag.split('-')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// A language as a person calls it.
pub fn language_name(code: &str) -> String {
    let bare = code.split('-').next().unwrap_or(code);
    match (code, myna_core::language::endonym(bare)) {
        ("zh-Hans", _) => gettextrs::gettext("Chinese (Simplified)"),
        ("zh-Hant", _) => gettextrs::gettext("Chinese (Traditional)"),
        (_, Some(name)) => name.to_owned(),
        _ => code.to_owned(),
    }
}

/// The English name of every language the catalog offers, by code, each a
/// literal `gettext` call so the extractor sees it. Built once; [`exonym`]
/// reads the names in the interface language, so a search for "German"
/// finds Deutsch.
fn exonyms() -> &'static [(&'static str, String)] {
    static NAMES: std::sync::OnceLock<Vec<(&'static str, String)>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        vec![
            ("af", gettextrs::gettext("Afrikaans")),
            ("ar", gettextrs::gettext("Arabic")),
            ("az", gettextrs::gettext("Azerbaijani")),
            ("be", gettextrs::gettext("Belarusian")),
            ("bg", gettextrs::gettext("Bulgarian")),
            ("bn", gettextrs::gettext("Bengali")),
            ("bs", gettextrs::gettext("Bosnian")),
            ("ca", gettextrs::gettext("Catalan")),
            ("cs", gettextrs::gettext("Czech")),
            ("da", gettextrs::gettext("Danish")),
            ("de", gettextrs::gettext("German")),
            ("el", gettextrs::gettext("Greek")),
            ("en", gettextrs::gettext("English")),
            ("es", gettextrs::gettext("Spanish")),
            ("et", gettextrs::gettext("Estonian")),
            ("eu", gettextrs::gettext("Basque")),
            ("fa", gettextrs::gettext("Persian")),
            ("fi", gettextrs::gettext("Finnish")),
            ("fr", gettextrs::gettext("French")),
            ("gl", gettextrs::gettext("Galician")),
            ("gu", gettextrs::gettext("Gujarati")),
            ("he", gettextrs::gettext("Hebrew")),
            ("hi", gettextrs::gettext("Hindi")),
            ("hr", gettextrs::gettext("Croatian")),
            ("hu", gettextrs::gettext("Hungarian")),
            ("id", gettextrs::gettext("Indonesian")),
            ("is", gettextrs::gettext("Icelandic")),
            ("it", gettextrs::gettext("Italian")),
            ("ja", gettextrs::gettext("Japanese")),
            ("kn", gettextrs::gettext("Kannada")),
            ("ko", gettextrs::gettext("Korean")),
            ("lt", gettextrs::gettext("Lithuanian")),
            ("lv", gettextrs::gettext("Latvian")),
            ("ml", gettextrs::gettext("Malayalam")),
            ("mr", gettextrs::gettext("Marathi")),
            ("ms", gettextrs::gettext("Malay")),
            ("nb", gettextrs::gettext("Norwegian Bokmål")),
            ("nl", gettextrs::gettext("Dutch")),
            ("pl", gettextrs::gettext("Polish")),
            ("pt", gettextrs::gettext("Portuguese")),
            ("ro", gettextrs::gettext("Romanian")),
            ("ru", gettextrs::gettext("Russian")),
            ("sk", gettextrs::gettext("Slovak")),
            ("sl", gettextrs::gettext("Slovenian")),
            ("sq", gettextrs::gettext("Albanian")),
            ("sr", gettextrs::gettext("Serbian")),
            ("sv", gettextrs::gettext("Swedish")),
            ("ta", gettextrs::gettext("Tamil")),
            ("te", gettextrs::gettext("Telugu")),
            ("th", gettextrs::gettext("Thai")),
            ("tl", gettextrs::gettext("Tagalog")),
            ("tr", gettextrs::gettext("Turkish")),
            ("ug", gettextrs::gettext("Uyghur")),
            ("uk", gettextrs::gettext("Ukrainian")),
            ("ur", gettextrs::gettext("Urdu")),
            ("vi", gettextrs::gettext("Vietnamese")),
            ("zh", gettextrs::gettext("Chinese")),
        ]
    })
}

/// `code`'s name in the interface language. `None` for a language with no
/// entry; the endonym alone then names it. Chinese scripts are already
/// named so by [`language_name`].
pub fn exonym(code: &str) -> Option<String> {
    let (_, name) = exonyms().iter().find(|(have, _)| *have == code)?;
    Some(name.clone())
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslateRow {
    pub targets: Vec<Language>,
    pub selected: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub description: String,
    pub enabled: bool,
    pub standing: Standing,
    pub translate: Option<TranslateRow>,
    pub script: Option<ScriptRow>,
}

/// How the Chinese script step writes Chinese: the modes it offers and the
/// one chosen, or the one a switch-on would choose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptRow {
    /// `(nick, name)`, in the order the combo lists them.
    pub modes: Vec<(String, String)>,
    pub selected: usize,
}

impl Row {
    /// Whether the switch can be turned: not while the step cannot apply.
    pub fn switchable(&self) -> bool {
        !matches!(self.standing, Standing::NotApplicable(_))
    }

    /// The line under the title: what is wrong, else what the step does.
    pub fn subtitle(&self) -> String {
        match &self.standing {
            Standing::NotApplicable(reason) => reason.clone(),
            Standing::Missing(need) => {
                // TRANSLATORS: {what} names a model to install, such as "the French model"; {size} is its download size.
                let frame = gettextrs::gettext("Needs {what} ({size})");
                frame
                    .replace("{what}", &need.what)
                    .replace("{size}", &size(need.bytes))
            }
            Standing::Unavailable(reason) => reason.clone(),
            Standing::Installing(_) => self.description_or("Getting the model ready"),
            Standing::Preparing => gettextrs::gettext("Getting the model ready"),
            Standing::Ready => self.description.clone(),
        }
    }

    fn description_or(&self, fallback: &str) -> String {
        if self.description.is_empty() {
            gettextrs::gettext(fallback)
        } else {
            self.description.clone()
        }
    }
}

/// A download size in words.
pub fn size(bytes: u64) -> String {
    // TRANSLATORS: {n} is a size in megabytes, such as "24".
    let frame = gettextrs::gettext("{n} MB");
    frame.replace("{n}", &bytes.div_ceil(1_000_000).max(1).to_string())
}

pub struct Inputs<'a> {
    /// The `language` setting is empty: the model decides.
    pub language_unset: bool,
    pub enabled: &'a [String],
    pub params: &'a Params,
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

/// The step ids the rows show: the ones this build knows, in the order the
/// daemon runs them, then any id only a catalog has, by name.
fn step_ids(catalog: &[Entry]) -> Vec<String> {
    let known = |id: &str| {
        myna_core::chains::KNOWN_STEPS
            .iter()
            .position(|have| *have == id)
            .unwrap_or(myna_core::chains::KNOWN_STEPS.len())
    };
    let mut ids: Vec<String> = catalog
        .iter()
        .map(|entry| entry.step.id.clone())
        .chain([SCRIPT.to_owned()])
        .collect();
    ids.sort_by(|a, b| (known(a), a).cmp(&(known(b), b)));
    ids.dedup();
    ids
}

pub fn rows(inputs: &Inputs) -> Vec<Row> {
    step_ids(inputs.catalog)
        .into_iter()
        .map(|id| row(&id, inputs))
        .collect()
}

fn row(id: &str, inputs: &Inputs) -> Row {
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
    let enabled = inputs.enabled.iter().any(|on| on == id);
    let translate = (id == TRANSLATE).then(|| translate_row(inputs));
    let script = (id == SCRIPT).then(|| script_row(inputs));
    let standing = standing(id, enabled, translate.as_ref(), inputs);
    Row {
        id: id.to_owned(),
        title,
        description,
        enabled,
        standing,
        translate,
        script,
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

fn script_row(inputs: &Inputs) -> ScriptRow {
    let stored = script_of(&[SCRIPT.to_owned()], inputs.params);
    let chosen = SCRIPT_MODES
        .iter()
        .position(|mode| *mode == stored)
        .filter(|_| inputs.params.contains_key(&param_key(SCRIPT, SCRIPT_MODE)));
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

/// Whether the text at the script step's position is Chinese, as far as
/// Settings can tell: the translation target when one runs, else the
/// dictation language, which an unset one leaves to the desktop's.
fn text_is_chinese(inputs: &Inputs) -> bool {
    let translated = inputs
        .enabled
        .iter()
        .any(|id| id == TRANSLATE)
        .then(|| target_of(inputs.params))
        .flatten()
        .filter(|to| matches!(inputs.source, Source::One(from) if from != to));
    if let Some(to) = translated {
        return myna_core::chains::may_be_chinese(Some(to));
    }
    match inputs.source {
        Source::Unknown => true,
        Source::One(_) if inputs.language_unset => inputs
            .locales
            .first()
            .is_some_and(|locale| myna_core::chains::may_be_chinese(Some(locale))),
        Source::One(from) => myna_core::chains::may_be_chinese(Some(from)),
    }
}

fn target_of(params: &Params) -> Option<&str> {
    params
        .get(&param_key(TRANSLATE, TRANSLATE_TO))
        .map(String::as_str)
        .filter(|to| !to.is_empty())
}

/// Whether some model, installed or offered by the catalogs, translates
/// `from` to `to`, directly or through English.
pub fn routable(catalog: &[Entry], daemon: Option<&Offered>, from: &str, to: &str) -> bool {
    plugin_catalog::translation_legs(catalog, from, to).is_some()
        || daemon.is_some_and(|d| d.translations.iter().any(|(f, t)| f == from && t == to))
}

fn translate_row(inputs: &Inputs) -> TranslateRow {
    let Source::One(from) = inputs.source else {
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
    let chosen = target_of(inputs.params).filter(|to| *to != from);
    let targets = languages(&LanguageInputs {
        catalog: inputs.catalog,
        daemon: inputs.daemon,
        from: inputs.source,
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
    id: &str,
    enabled: bool,
    translate: Option<&TranslateRow>,
    inputs: &Inputs,
) -> Standing {
    let source = match inputs.source {
        Source::One(from) => Some(from.as_str()),
        Source::Unknown => None,
    };
    let own: Vec<&Entry> = inputs.catalog.iter().filter(|e| e.step.id == id).collect();
    if id == SCRIPT {
        if !text_is_chinese(inputs) {
            return Standing::NotApplicable(gettextrs::gettext("Applies when the text is Chinese"));
        }
        return Standing::Ready;
    }
    if id == TRANSLATE {
        let Some(from) = source else {
            return Standing::NotApplicable(gettextrs::gettext(
                "Needs a single dictation language",
            ));
        };
        if enabled && target_of(inputs.params) == Some(from) {
            return Standing::NotApplicable(gettextrs::gettext(
                "The text is already in that language",
            ));
        }
    } else if !own.is_empty()
        && own
            .iter()
            .all(|e| !e.in_lang.is_empty() && e.in_lang != "any")
    {
        if let Some(from) = source {
            if own.iter().all(|e| e.in_lang != from) {
                // TRANSLATORS: {language} is a language name, such as "English".
                let frame = gettextrs::gettext("Applies when the text is {language}");
                let wanted = own[0].in_lang.as_str();
                return Standing::NotApplicable(
                    frame.replace("{language}", &language_name(wanted)),
                );
            }
        }
    }
    if !enabled {
        return Standing::Ready;
    }
    if let Some(percent) = inputs.installing.get(id) {
        return Standing::Installing(*percent);
    }
    if let (Some(target), Source::One(from)) = (
        translate.and_then(|t| t.targets.get(t.selected?)),
        inputs.source,
    ) {
        if matches!(target.state, LanguageState::NoModel) {
            // TRANSLATORS: {from} and {to} are language names, such as "فارسی" and "Français".
            let frame = gettextrs::gettext("No model translates {from} to {to} yet");
            return Standing::Unavailable(
                frame
                    .replace("{from}", &language_name(from))
                    .replace("{to}", &target.name),
            );
        }
    }
    let Some(need) = need(id, translate, &own, inputs) else {
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
    id: &str,
    translate: Option<&TranslateRow>,
    own: &[&Entry],
    inputs: &Inputs,
) -> Option<Need> {
    let daemon = inputs.daemon?;
    if id == TRANSLATE {
        let target = translate?
            .targets
            .iter()
            .find(|t| Some(t.code.as_str()) == target_of(inputs.params))?;
        if matches!(target.state, LanguageState::Ready) {
            return None;
        }
        let Source::One(from) = inputs.source else {
            return None;
        };
        let legs = plugin_catalog::translation_legs(inputs.catalog, from, &target.code)?;
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

/// The stored steps and parameters after the switch of `id` is set.
pub fn with_enabled(ids: &[String], id: &str, on: bool) -> Vec<String> {
    let mut next: Vec<String> = ids.iter().filter(|have| *have != id).cloned().collect();
    if on {
        next.push(id.to_owned());
    }
    in_stage_order(&next)
}

/// The parameters after the Chinese script step is switched on: a mode it
/// can run with, the one chosen before or else the locale's.
pub fn with_script_on(params: &Params, locales: &[String]) -> Params {
    let key = param_key(SCRIPT, SCRIPT_MODE);
    let mut next = params.clone();
    if !params
        .get(&key)
        .is_some_and(|nick| SCRIPT_MODES.iter().any(|mode| mode.nick() == nick))
    {
        next.insert(key, default_script(locales).nick().to_owned());
    }
    next
}

/// The parameters after a Chinese script mode is chosen.
pub fn with_script_mode(params: &Params, nick: &str) -> Params {
    let mut next = params.clone();
    next.insert(param_key(SCRIPT, SCRIPT_MODE), nick.to_owned());
    next
}

/// The parameters after `translate.to` is set, or cleared.
pub fn with_target(params: &Params, to: Option<&str>) -> Params {
    let mut next = params.clone();
    let key = param_key(TRANSLATE, TRANSLATE_TO);
    match to {
        Some(to) => next.insert(key, to.to_owned()),
        None => next.remove(&key),
    };
    next
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
    fn sizes_round_up_to_whole_megabytes() {
        assert_eq!(size(1), "1 MB");
        assert_eq!(size(24_000_001), "25 MB");
        assert_eq!(size(33_353_728), "34 MB");
    }
}
