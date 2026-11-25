//! What the Processing page shows, GTK-free: one row per step in stage
//! order, each with the state it is in and what it needs. Rows come from the
//! merged catalogs and what the daemon offers, not from a compiled list, so a
//! plugin's step appears without a Settings change.

use std::collections::{BTreeMap, BTreeSet};

use myna_core::processing::{in_stage_order, param_key, script_of, ChineseScript, Params};

use crate::plugin_catalog::{self, Entry};

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

/// The English name of every language the catalog offers, by code. The
/// msgid is the name; [`exonym`] translates it, so a search for "German"
/// finds Deutsch.
const EXONYMS: [(&str, &str); 57] = [
    ("af", "Afrikaans"),
    ("ar", "Arabic"),
    ("az", "Azerbaijani"),
    ("be", "Belarusian"),
    ("bg", "Bulgarian"),
    ("bn", "Bengali"),
    ("bs", "Bosnian"),
    ("ca", "Catalan"),
    ("cs", "Czech"),
    ("da", "Danish"),
    ("de", "German"),
    ("el", "Greek"),
    ("en", "English"),
    ("es", "Spanish"),
    ("et", "Estonian"),
    ("eu", "Basque"),
    ("fa", "Persian"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("gl", "Galician"),
    ("gu", "Gujarati"),
    ("he", "Hebrew"),
    ("hi", "Hindi"),
    ("hr", "Croatian"),
    ("hu", "Hungarian"),
    ("id", "Indonesian"),
    ("is", "Icelandic"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("kn", "Kannada"),
    ("ko", "Korean"),
    ("lt", "Lithuanian"),
    ("lv", "Latvian"),
    ("ml", "Malayalam"),
    ("mr", "Marathi"),
    ("ms", "Malay"),
    ("nb", "Norwegian Bokmål"),
    ("nl", "Dutch"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("sq", "Albanian"),
    ("sr", "Serbian"),
    ("sv", "Swedish"),
    ("ta", "Tamil"),
    ("te", "Telugu"),
    ("th", "Thai"),
    ("tl", "Tagalog"),
    ("tr", "Turkish"),
    ("ug", "Uyghur"),
    ("uk", "Ukrainian"),
    ("ur", "Urdu"),
    ("vi", "Vietnamese"),
    ("zh", "Chinese"),
];

/// `code`'s name in the interface language. `None` for a language with no
/// entry; the endonym alone then names it. Chinese scripts are already
/// named so by [`language_name`].
pub fn exonym(code: &str) -> Option<String> {
    let (_, name) = EXONYMS.iter().find(|(have, _)| *have == code)?;
    Some(gettextrs::gettext(*name))
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
pub struct Target {
    pub code: String,
    pub name: String,
    pub installed: bool,
    pub bytes: u64,
    /// Some installed model, or one the catalogs offer, translates into it
    /// from the text's language, directly or through English. A chosen
    /// target with no such model is still listed, so the picker shows it.
    pub routed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslateRow {
    pub targets: Vec<Target>,
    pub selected: Option<usize>,
    /// The dictation language, named, when it is not English.
    pub from: Option<String>,
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
            from: None,
        };
    };
    let installed = |legs: &[&Entry]| {
        inputs
            .daemon
            .is_some_and(|daemon| legs.iter().all(|leg| daemon.offers(&leg.model)))
    };
    let mut codes: BTreeSet<String> = inputs
        .catalog
        .iter()
        .filter(|e| e.step.id == TRANSLATE)
        .flat_map(|e| [e.in_lang.clone(), e.out_lang.clone()])
        .filter(|code| !code.is_empty() && code != from)
        .collect();
    if let Some(daemon) = inputs.daemon {
        codes.extend(
            daemon
                .translations
                .iter()
                .filter(|(f, _)| f == from)
                .map(|(_, to)| to.clone()),
        );
    }
    let mut targets: Vec<Target> = codes
        .into_iter()
        .filter_map(|code| {
            let direct_installed = inputs
                .daemon
                .is_some_and(|d| d.translations.iter().any(|(f, t)| f == from && *t == code));
            match plugin_catalog::translation_legs(inputs.catalog, from, &code) {
                Some(legs) => {
                    let done = direct_installed || installed(&legs);
                    let bytes = if done {
                        0
                    } else {
                        legs.iter()
                            .filter(|leg| !inputs.daemon.is_some_and(|d| d.offers(&leg.model)))
                            .map(|leg| leg.download_bytes)
                            .sum()
                    };
                    Some(Target {
                        name: language_name(&code),
                        code,
                        installed: done,
                        bytes,
                        routed: true,
                    })
                }
                None => direct_installed.then(|| Target {
                    name: language_name(&code),
                    code,
                    installed: true,
                    bytes: 0,
                    routed: true,
                }),
            }
        })
        .collect();
    let order: Vec<String> = inputs.locales.to_vec();
    let rank = |code: &str| {
        order
            .iter()
            .position(|locale| catalog_code(locale) == code || locale.starts_with(code))
            .unwrap_or(order.len())
    };
    targets.sort_by(|a, b| (rank(&a.code), &a.name).cmp(&(rank(&b.code), &b.name)));
    if let Some(to) = target_of(inputs.params).filter(|to| *to != from) {
        if !targets.iter().any(|t| t.code == to) {
            targets.push(Target {
                name: language_name(to),
                code: to.to_owned(),
                installed: false,
                bytes: 0,
                routed: false,
            });
        }
    }
    let selected =
        target_of(inputs.params).and_then(|to| targets.iter().position(|t| t.code == to));
    TranslateRow {
        targets,
        selected,
        from: (from != "en").then(|| language_name(from)),
    }
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
        if !target.routed {
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
        if target.installed {
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

/// The sentence under the steps, and what it comes out as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Example {
    pub heard: String,
    /// What gets typed; `None` for a translation with no sample here, which
    /// `note` then names rather than show the English as if it were typed.
    pub typed: Option<String>,
    /// What the text becomes beyond what is shown, such as its language.
    pub note: Option<String>,
}

/// "So I think we should go." in the targets most chains will use. Static
/// strings, no model call; a target missing here reads "Translated into".
fn translated_sample(target: &str) -> Option<&'static str> {
    Some(match target {
        "fr" => "Je pense que nous devrions y aller.",
        "de" => "Ich denke, wir sollten gehen.",
        "es" => "Creo que deberíamos irnos.",
        "it" => "Penso che dovremmo andare.",
        "pt" => "Acho que deveríamos ir.",
        "nl" => "Ik denk dat we moeten gaan.",
        _ => return None,
    })
}

pub fn example(rows: &[Row]) -> Example {
    let runs = |id: &str| {
        rows.iter()
            .any(|row| row.id == id && row.enabled && row.standing == Standing::Ready)
    };
    let heard = gettextrs::gettext("um so I, I think we should go");
    let mut typed = Some(heard.clone());
    if runs("tidy") {
        typed = Some(gettextrs::gettext("So I think we should go."));
    }
    let mut note = None;
    if runs(TRANSLATE) {
        let target = rows
            .iter()
            .find(|row| row.id == TRANSLATE)
            .and_then(|row| row.translate.as_ref())
            .and_then(|t| t.targets.get(t.selected?));
        if let Some(target) = target {
            // Not translated: it is what gets typed, in that language.
            typed = translated_sample(&target.code).map(str::to_owned);
            if typed.is_none() {
                // TRANSLATORS: {language} is a language name, such as "فارسی"; stands for the example sentence translated.
                let frame = gettextrs::gettext("Translated into {language}");
                note = Some(frame.replace("{language}", &target.name));
            }
        }
    }
    Example { heard, typed, note }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str, step: &str, stage: &str, from: &str, to: &str, bytes: u64) -> Entry {
        serde_json::from_value(json!({
            "id": id, "snap": "myna-translation", "component": id, "model": format!("m-{id}"),
            "task": "text.translate", "in_lang": from, "out_lang": to, "download_bytes": bytes,
            "step": {"id": step, "stage": stage,
                     "label_key": format!("step.{step}.name"),
                     "description_key": format!("step.{step}.description"),
                     "example_key": format!("step.{step}.example")},
        }))
        .unwrap()
    }

    fn catalog() -> Vec<Entry> {
        vec![
            entry("tidy-en", "tidy", "clean-up", "en", "en", 42_000_000),
            entry("en-fr", "translate", "translate", "en", "fr", 33_000_000),
            entry("en-de", "translate", "translate", "en", "de", 31_000_000),
            entry("de-en", "translate", "translate", "de", "en", 30_000_000),
            entry("en-fa", "translate", "translate", "en", "fa", 19_000_000),
        ]
    }

    struct Rig {
        enabled: Vec<String>,
        params: Params,
        source: Source,
        unset: bool,
        catalog: Vec<Entry>,
        daemon: Option<Offered>,
        installing: BTreeMap<String, Option<u8>>,
        preparing: BTreeSet<String>,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                enabled: Vec::new(),
                params: Params::new(),
                source: Source::One("en".into()),
                unset: false,
                catalog: catalog(),
                daemon: Some(Offered::default()),
                installing: BTreeMap::new(),
                preparing: BTreeSet::new(),
            }
        }

        fn rows(&self) -> Vec<Row> {
            rows(&Inputs {
                language_unset: self.unset,
                enabled: &self.enabled,
                params: &self.params,
                source: &self.source,
                catalog: &self.catalog,
                daemon: self.daemon.as_ref(),
                installing: &self.installing,
                preparing: &self.preparing,
                locales: &["fr_FR.UTF-8".to_owned()],
            })
        }

        fn row(&self, id: &str) -> Row {
            self.rows().into_iter().find(|r| r.id == id).unwrap()
        }

        fn on(mut self, ids: &[&str]) -> Self {
            self.enabled = ids.iter().map(|s| (*s).to_owned()).collect();
            self
        }

        fn to(mut self, code: &str) -> Self {
            self.params = with_target(&self.params, Some(code));
            self
        }

        fn offering(mut self, models: &[&str]) -> Self {
            let daemon = self.daemon.get_or_insert_with(Offered::default);
            daemon.models = models.iter().map(|s| (*s).to_owned()).collect();
            self
        }
    }

    #[test]
    fn rows_follow_the_stages_whatever_the_catalog_order() {
        let mut rig = Rig::new();
        rig.catalog.reverse();
        let ids: Vec<String> = rig.rows().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["tidy", "translate", "script"]);
    }

    #[test]
    fn the_client_s_own_step_is_a_row_with_no_catalog_and_no_snap() {
        let mut rig = Rig::new().on(&["script"]);
        rig.catalog.clear();
        rig.daemon = None;
        let rows = rig.rows();
        assert_eq!(rows.len(), 1, "Chinese script");
        assert_eq!(rows[0].id, SCRIPT);
        assert!(rows[0].enabled);
    }

    #[test]
    fn a_step_is_ready_when_off_and_when_its_model_is_offered() {
        let rig = Rig::new();
        assert_eq!(rig.row("tidy").standing, Standing::Ready);
        let on = Rig::new().on(&["tidy"]).offering(&["m-tidy-en"]);
        assert_eq!(on.row("tidy").standing, Standing::Ready);
    }

    #[test]
    fn an_enabled_step_without_its_model_names_it_and_its_size() {
        let rig = Rig::new().on(&["tidy"]);
        let Standing::Missing(need) = rig.row("tidy").standing else {
            panic!("tidy should be missing its model");
        };
        assert_eq!(need.bytes, 42_000_000);
        assert_eq!(need.entries[0].id, "tidy-en");
        assert!(rig.row("tidy").subtitle().contains("42 MB"));
    }

    #[test]
    fn the_daemon_s_word_about_a_step_outranks_the_catalog() {
        let mut rig = Rig::new().on(&["tidy"]);
        rig.daemon
            .as_mut()
            .unwrap()
            .steps
            .insert("tidy".into(), DaemonStep::Applicable);
        assert_eq!(rig.row("tidy").standing, Standing::Ready);
        rig.daemon
            .as_mut()
            .unwrap()
            .steps
            .insert("tidy".into(), DaemonStep::Unavailable);
        assert!(matches!(rig.row("tidy").standing, Standing::Missing(_)));
    }

    #[test]
    fn with_no_daemon_nothing_is_claimed_missing() {
        let mut rig = Rig::new().on(&["tidy"]);
        rig.daemon = None;
        assert_eq!(rig.row("tidy").standing, Standing::Ready);
    }

    #[test]
    fn tidy_is_dimmed_with_its_reason_when_the_text_is_not_english() {
        let mut rig = Rig::new().on(&["tidy"]);
        rig.source = Source::One("fr".into());
        let row = rig.row("tidy");
        assert_eq!(
            row.standing,
            Standing::NotApplicable("Applies when the text is English".into())
        );
        assert!(!row.switchable());
        assert_eq!(row.subtitle(), "Applies when the text is English");
    }

    #[test]
    fn an_installing_model_shows_its_percentage_and_a_finished_one_prepares() {
        let mut rig = Rig::new().on(&["translate"]).to("fr");
        rig.installing.insert("translate".into(), Some(40));
        assert_eq!(
            rig.row("translate").standing,
            Standing::Installing(Some(40))
        );
        rig.installing.clear();
        rig.preparing.insert("translate".into());
        assert_eq!(rig.row("translate").standing, Standing::Preparing);
    }

    #[test]
    fn translate_lists_every_target_with_the_users_language_first() {
        let rig = Rig::new().on(&["translate"]);
        let translate = rig.row("translate").translate.unwrap();
        let codes: Vec<&str> = translate.targets.iter().map(|t| t.code.as_str()).collect();
        // The locale's French first, then by name: Deutsch before فارسی.
        assert_eq!(codes, ["fr", "de", "fa"]);
        assert_eq!(translate.selected, None);
        assert_eq!(translate.from, None);
    }

    #[test]
    fn a_target_is_installed_when_the_daemon_offers_its_model_and_sized_when_not() {
        let rig = Rig::new()
            .on(&["translate"])
            .to("fr")
            .offering(&["m-en-fr"]);
        let translate = rig.row("translate").translate.unwrap();
        assert!(translate.targets[0].installed);
        assert_eq!(translate.targets[0].bytes, 0);
        assert!(!translate.targets[1].installed);
        assert_eq!(translate.targets[1].bytes, 31_000_000);
        assert_eq!(rig.row("translate").standing, Standing::Ready);
    }

    #[test]
    fn a_missing_target_names_its_language_and_asks_for_the_entry() {
        let rig = Rig::new().on(&["translate"]).to("de");
        let Standing::Missing(need) = rig.row("translate").standing else {
            panic!("german is not installed");
        };
        assert_eq!(need.what, "the Deutsch model");
        assert_eq!(need.entries.len(), 1);
        assert_eq!(need.entries[0].id, "en-de");
    }

    #[test]
    fn a_pivot_from_another_language_is_two_legs_and_only_the_missing_ones_count() {
        let mut rig = Rig::new().on(&["translate"]).to("fr");
        rig.source = Source::One("de".into());
        let translate = rig.row("translate").translate.unwrap();
        assert_eq!(translate.from.as_deref(), Some("Deutsch"));
        let fr = translate.targets.iter().find(|t| t.code == "fr").unwrap();
        assert_eq!(fr.bytes, 63_000_000);
        let rig = rig.offering(&["m-de-en"]);
        let Standing::Missing(need) = rig.row("translate").standing else {
            panic!("the second leg is missing");
        };
        assert_eq!(need.entries.len(), 1);
        assert_eq!(need.bytes, 33_000_000);
    }

    #[test]
    fn a_dictation_in_the_target_language_needs_no_translation() {
        let mut rig = Rig::new().on(&["translate"]).to("fr");
        rig.source = Source::One("fr".into());
        assert_eq!(
            rig.row("translate").standing,
            Standing::NotApplicable("The text is already in that language".into())
        );
    }

    #[test]
    fn several_dictation_languages_leave_translation_without_a_source() {
        let mut rig = Rig::new().on(&["translate"]);
        rig.source = Source::of_setting("en,fr");
        let row = rig.row("translate");
        assert_eq!(
            row.standing,
            Standing::NotApplicable("Needs a single dictation language".into())
        );
        assert!(row.translate.unwrap().targets.is_empty());
    }

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
    fn a_model_the_catalog_does_not_list_still_shows_as_an_installed_target() {
        let mut rig = Rig::new().on(&["translate"]).to("fa");
        rig.daemon = Some(Offered {
            translations: vec![("en".into(), "fa".into())],
            ..Offered::default()
        });
        let translate = rig.row("translate").translate.unwrap();
        let fa = translate.targets.iter().find(|t| t.code == "fa").unwrap();
        assert!(fa.installed);
        assert_eq!(rig.row("translate").standing, Standing::Ready);
    }

    #[test]
    fn switching_keeps_every_other_id_and_the_stage_order() {
        let ids = vec!["translate".to_owned(), "script".to_owned()];
        assert_eq!(
            with_enabled(&ids, "tidy", true),
            ["tidy", "translate", "script"]
        );
        assert_eq!(with_enabled(&ids, "translate", false), ["script"]);
        assert_eq!(
            with_enabled(&ids, "translate", true),
            ["translate", "script"]
        );
    }

    #[test]
    fn the_target_is_written_and_cleared_without_touching_other_parameters() {
        let mut params = Params::new();
        params.insert("script.mode".into(), "simplified".into());
        let set = with_target(&params, Some("de"));
        assert_eq!(set.get("translate.to").map(String::as_str), Some("de"));
        assert_eq!(
            set.get("script.mode").map(String::as_str),
            Some("simplified")
        );
        assert_eq!(with_target(&set, None), params);
    }

    #[test]
    fn the_example_follows_the_switches() {
        let off = example(&Rig::new().rows());
        assert_eq!(off.typed.as_deref(), Some(off.heard.as_str()));
        let tidy = example(&Rig::new().on(&["tidy"]).offering(&["m-tidy-en"]).rows());
        assert_eq!(tidy.typed.as_deref(), Some("So I think we should go."));
        let french = example(
            &Rig::new()
                .on(&["tidy", "translate"])
                .to("fr")
                .offering(&["m-tidy-en", "m-en-fr"])
                .rows(),
        );
        assert_eq!(
            french.typed.as_deref(),
            Some("Je pense que nous devrions y aller.")
        );
        let german = example(
            &Rig::new()
                .on(&["translate"])
                .to("de")
                .offering(&["m-en-de"])
                .rows(),
        );
        assert_eq!(
            german.typed.as_deref(),
            Some("Ich denke, wir sollten gehen.")
        );
        // Charles, 2026-11-23: a target with no sample repeated the English
        // sentence as if that were typed.
        let persian = example(
            &Rig::new()
                .on(&["translate"])
                .to("fa")
                .offering(&["m-en-fa"])
                .rows(),
        );
        assert_eq!(persian.typed, None);
        assert_eq!(persian.note.as_deref(), Some("Translated into فارسی"));
    }

    #[test]
    fn a_step_whose_model_is_missing_leaves_the_example_alone() {
        let example = example(&Rig::new().on(&["tidy"]).rows());
        assert_eq!(example.typed.as_deref(), Some(example.heard.as_str()));
    }

    #[test]
    fn the_daemon_s_catalog_is_read_and_an_old_daemon_has_none() {
        let json = json!({
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
    fn chinese_targets_and_codes_have_names() {
        assert_eq!(language_name("zh-Hans"), "Chinese (Simplified)");
        assert_eq!(language_name("zh-Hant"), "Chinese (Traditional)");
        assert_eq!(language_name("xx"), "xx");
    }

    #[test]
    fn a_row_without_a_description_still_has_a_subtitle_while_installing() {
        let mut rig = Rig::new().on(&["translate"]).to("fr");
        rig.installing.insert("translate".into(), None);
        assert_eq!(
            rig.row("translate").subtitle(),
            "Types your dictation in another language."
        );
        let mut bare = Rig::new().on(&["script"]);
        bare.catalog.clear();
        bare.installing.insert("script".into(), None);
        let row = Row {
            description: String::new(),
            standing: Standing::Installing(None),
            ..bare.row("script")
        };
        assert_eq!(row.subtitle(), "Getting the model ready");
    }

    #[test]
    fn only_pairs_the_source_can_reach_are_listed_and_the_source_is_never_a_target() {
        let rig = Rig::new().on(&["translate"]);
        let codes: Vec<String> = rig
            .row("translate")
            .translate
            .unwrap()
            .targets
            .into_iter()
            .map(|t| t.code)
            .collect();
        assert!(!codes.contains(&"en".to_owned()));
        let mut empty_lang = Rig::new().on(&["translate"]);
        empty_lang
            .catalog
            .push(entry("x", "translate", "translate", "", "", 1));
        let codes: Vec<String> = empty_lang
            .row("translate")
            .translate
            .unwrap()
            .targets
            .into_iter()
            .map(|t| t.code)
            .collect();
        assert!(!codes.contains(&String::new()));
    }

    #[test]
    fn an_offered_pair_from_another_source_is_not_an_installed_target() {
        let mut rig = Rig::new().on(&["translate"]);
        rig.daemon = Some(Offered {
            translations: vec![("de".into(), "fr".into())],
            ..Offered::default()
        });
        let fr = rig
            .row("translate")
            .translate
            .unwrap()
            .targets
            .into_iter()
            .find(|t| t.code == "fr")
            .unwrap();
        assert!(!fr.installed);
    }

    #[test]
    fn the_users_language_comes_first_and_only_a_matching_one() {
        let rig = Rig::new().on(&["translate"]);
        let first = |locales: &[&str]| {
            let locales: Vec<String> = locales.iter().map(|l| (*l).to_owned()).collect();
            let rows = rows(&Inputs {
                language_unset: rig.unset,
                enabled: &rig.enabled,
                params: &rig.params,
                source: &rig.source,
                catalog: &rig.catalog,
                daemon: rig.daemon.as_ref(),
                installing: &rig.installing,
                preparing: &rig.preparing,
                locales: &locales,
            });
            rows.into_iter()
                .find(|r| r.id == "translate")
                .unwrap()
                .translate
                .unwrap()
                .targets[0]
                .code
                .clone()
        };
        assert_eq!(first(&["de_DE.UTF-8"]), "de");
        assert_eq!(first(&["fr"]), "fr");
        assert_eq!(first(&["xx"]), "de");
    }

    #[test]
    fn a_step_with_an_any_input_is_never_dimmed_by_the_language() {
        let mut rig = Rig::new().on(&["tidy"]);
        rig.source = Source::One("fr".into());
        rig.catalog[0].in_lang = "any".into();
        assert!(!matches!(
            rig.row("tidy").standing,
            Standing::NotApplicable(_)
        ));
        rig.catalog[0].in_lang = String::new();
        assert!(!matches!(
            rig.row("tidy").standing,
            Standing::NotApplicable(_)
        ));
    }

    #[test]
    fn sizes_round_up_to_whole_megabytes() {
        assert_eq!(size(1), "1 MB");
        assert_eq!(size(24_000_001), "25 MB");
        assert_eq!(size(33_353_728), "34 MB");
    }

    fn zh() -> Source {
        Source::of_setting("zh")
    }

    fn state_of(rig: &Rig) -> Standing {
        rig.row(SCRIPT).standing
    }

    #[test]
    fn the_script_step_is_built_in_and_last_before_the_guards() {
        let mut rig = Rig::new();
        rig.catalog.clear();
        let row = rig.row(SCRIPT);
        assert_eq!(row.title, "Chinese script");
        assert!(!row.enabled);
        let ids: Vec<String> = rig.rows().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["script"]);
    }

    #[test]
    fn the_script_step_applies_when_the_text_is_chinese() {
        for (setting, applies) in [
            ("zh", true),
            ("zh-Hant", true),
            ("zh_TW", true),
            ("yue-HK", true),
            ("cmn", true),
            ("auto", true),
            ("en", false),
            ("fr", false),
            ("ja", false),
            ("zhuang", false),
        ] {
            let mut rig = Rig::new().on(&[SCRIPT]);
            rig.source = Source::of_setting(setting);
            let ready = state_of(&rig) == Standing::Ready;
            assert_eq!(ready, applies, "{setting}");
            if !applies {
                assert_eq!(
                    rig.row(SCRIPT).subtitle(),
                    "Applies when the text is Chinese"
                );
                assert!(!rig.row(SCRIPT).switchable());
            }
        }
    }

    #[test]
    fn the_script_step_follows_a_translation_into_or_out_of_chinese() {
        let mut rig = Rig::new().on(&[TRANSLATE, SCRIPT]).to("zh-Hans");
        assert_eq!(state_of(&rig), Standing::Ready, "English into Chinese");
        rig.source = zh();
        let mut rig = rig.to("en");
        rig.source = zh();
        assert!(
            matches!(state_of(&rig), Standing::NotApplicable(_)),
            "Chinese into English"
        );
        let mut off = Rig::new().on(&[SCRIPT]).to("zh-Hans");
        assert!(
            matches!(state_of(&off), Standing::NotApplicable(_)),
            "a translation that is off changes nothing"
        );
        off.enabled.push(TRANSLATE.to_owned());
        assert_eq!(state_of(&off), Standing::Ready);
    }

    #[test]
    fn an_unset_language_is_chinese_for_a_chinese_desktop_only() {
        let mut rig = Rig::new().on(&[SCRIPT]);
        rig.unset = true;
        assert!(matches!(state_of(&rig), Standing::NotApplicable(_)));
        let locales = ["zh_TW".to_owned(), "en".to_owned()];
        let standing = |rig: &Rig| {
            row(
                SCRIPT,
                &Inputs {
                    language_unset: rig.unset,
                    enabled: &rig.enabled,
                    params: &rig.params,
                    source: &rig.source,
                    catalog: &rig.catalog,
                    daemon: rig.daemon.as_ref(),
                    installing: &rig.installing,
                    preparing: &rig.preparing,
                    locales: &locales,
                },
            )
            .standing
        };
        assert_eq!(standing(&rig), Standing::Ready);
        rig.unset = false;
        assert!(matches!(standing(&rig), Standing::NotApplicable(_)));
    }

    #[test]
    fn the_modes_are_the_four_scripts_and_show_the_stored_one() {
        let mut rig = Rig::new().on(&[SCRIPT]);
        rig.source = zh();
        let modes = rig.row(SCRIPT).script.unwrap();
        let nicks: Vec<&str> = modes.modes.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            nicks,
            [
                "simplified",
                "traditional",
                "traditional-taiwan",
                "traditional-hongkong"
            ]
        );
        assert!(modes.modes.iter().all(|(n, name)| n != name));
        for (index, nick) in nicks.iter().enumerate() {
            rig.params = with_script_mode(&rig.params, nick);
            assert_eq!(rig.row(SCRIPT).script.unwrap().selected, index, "{nick}");
        }
        rig.params = with_script_mode(&Params::new(), "klingon");
        assert_eq!(rig.row(SCRIPT).script.unwrap().selected, 0, "unknown");
    }

    #[test]
    fn switching_the_step_on_picks_the_desktops_script_and_keeps_a_choice() {
        for (locales, mode) in [
            (&["zh_CN"][..], "simplified"),
            (&["zh_TW.UTF-8"], "traditional-taiwan"),
            (&["en", "zh_HK"], "traditional-hongkong"),
            (&["yue_HK"], "traditional-hongkong"),
            (&["zh_Hant"], "traditional"),
            (&["zh-Hant-TW"], "traditional-taiwan"),
            (&["en", "de"], "simplified"),
        ] {
            let locales: Vec<String> = locales.iter().map(|l| (*l).to_owned()).collect();
            let params = with_script_on(&Params::new(), &locales);
            assert_eq!(params["script.mode"], mode, "{locales:?}");
        }
        let kept = with_script_mode(&Params::new(), "traditional");
        assert_eq!(
            with_script_on(&kept, &["zh_TW".to_owned()])["script.mode"],
            "traditional"
        );
        let broken = with_script_mode(&Params::new(), "as-recognised");
        assert_eq!(
            with_script_on(&broken, &["zh_TW".to_owned()])["script.mode"],
            "traditional-taiwan"
        );
    }
}
