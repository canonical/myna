//! What the Processing page shows, GTK-free: one row per step in stage
//! order, each with the state it is in and what it needs. Rows come from the
//! merged catalogs and what the daemon offers, not from a compiled list, so a
//! plugin's step appears without a Settings change.

use std::collections::{BTreeMap, BTreeSet};

use myna_core::chains::ChainStep;
use myna_core::processing::ChineseScript;

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
        Standing::Ready => description.clone(),
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

/// Whether the text at the script step's position is Chinese, as far as
/// Settings can tell: the translation target when one runs, else the
/// dictation language, which an unset one leaves to the desktop's.
fn text_is_chinese(source: &Source, inputs: &Inputs) -> bool {
    match source {
        Source::Unknown => true,
        Source::One(_) if inputs.language_unset => inputs
            .locales
            .first()
            .is_some_and(|locale| myna_core::chains::may_be_chinese(Some(locale))),
        Source::One(from) => myna_core::chains::may_be_chinese(Some(from)),
    }
}

/// The language `step` translates into, when it holds one.
fn target_of(step: &ChainStep) -> Option<&str> {
    step.params
        .get(TRANSLATE_TO)
        .map(String::as_str)
        .filter(|to| !to.is_empty())
}

/// Whether some model, installed or offered by the catalogs, translates
/// `from` to `to`, directly or through English.
pub fn routable(catalog: &[Entry], daemon: Option<&Offered>, from: &str, to: &str) -> bool {
    plugin_catalog::translation_legs(catalog, from, to).is_some()
        || daemon.is_some_and(|d| d.translations.iter().any(|(f, t)| f == from && t == to))
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
    if id == SCRIPT {
        if !text_is_chinese(source, inputs) {
            return Standing::NotApplicable(gettextrs::gettext("Applies when the text is Chinese"));
        }
        return Standing::Ready;
    }
    if id == TRANSLATE {
        let Some(from) = language else {
            return Standing::NotApplicable(gettextrs::gettext(
                "Needs a single dictation language",
            ));
        };
        if step.on && target_of(step) == Some(from) {
            return Standing::NotApplicable(gettextrs::gettext(
                "The text is already in that language",
            ));
        }
    } else if !own.is_empty()
        && own
            .iter()
            .all(|e| !e.in_lang.is_empty() && e.in_lang != "any")
    {
        if let Some(from) = language {
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
    if !step.on {
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
                    .replace("{from}", &language_name(from))
                    .replace("{to}", &target.name),
            );
        }
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
