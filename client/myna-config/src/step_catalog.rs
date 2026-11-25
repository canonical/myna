//! What the Add step page lists, GTK-free: every step the merged catalogs
//! offer, plus the ones the client carries, each with what it is, what it
//! costs and what can be done with it. Entries come from the catalog schema
//! only; nothing here knows a plugin by name.

use myna_core::processing::{param_key, Params};

use crate::plugin_catalog::{self, Entry};
use std::collections::{BTreeMap, BTreeSet};

use crate::processing_steps::{
    exonym, language_name, size, BuiltIn, Offered, Row, Source, Standing, BUILT_INS, SCRIPT,
    TRANSLATE, TRANSLATE_TO,
};

/// What the page offers to do with one item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offer {
    /// Already one of the steps, switched on.
    Added,
    /// Installed (or built in): add it to the steps.
    Add,
    /// Installed, but it cannot act on this dictation; the reason shows.
    NotApplicable(String),
    /// Its model is not installed; this entry provides it.
    Install(Box<Entry>),
    /// Its model is being installed, with snapd's percentage when known.
    Installing(Option<u8>),
    /// Installed; the daemon has not offered the model yet.
    Preparing,
    /// No daemon answers, so nothing is known about the models.
    Unknown,
    /// Its model is being removed.
    Removing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail {
    pub label: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The entry's id, or the built-in step's id.
    pub id: String,
    /// The step that is switched on when the item is added.
    pub step: String,
    /// The `translate.to` the add sets, for a translation model.
    pub target: Option<String>,
    pub title: String,
    pub description: String,
    pub example: Option<String>,
    pub details: Vec<Detail>,
    pub offer: Offer,
    /// The entry whose model is installed and can be removed.
    pub removable: Option<Box<Entry>>,
}

pub struct Inputs<'a> {
    pub catalog: &'a [Entry],
    pub enabled: &'a [String],
    pub params: &'a Params,
    pub source: &'a Source,
    /// The language of the text where an added translation would go in the
    /// chain: the dictation language, or what an earlier Translate made it.
    pub translate_from: &'a Source,
    pub daemon: Option<&'a Offered>,
    /// Installs running now, by item id, with snapd's percentage.
    pub installing: &'a std::collections::BTreeMap<String, Option<u8>>,
    /// Item ids installed whose models the daemon has not offered yet.
    pub preparing: &'a std::collections::BTreeSet<String>,
    /// Item ids whose models are being removed.
    pub removing: &'a std::collections::BTreeSet<String>,
    pub locales: &'a [String],
    /// The Steps rows as the page shows them, for what cannot apply.
    pub rows: &'a [Row],
}

/// The items in the order the page lists them: what suits the user's
/// locales first, then stage order.
pub fn items(inputs: &Inputs) -> Vec<Item> {
    // Fitting to the field is a switch on the Processing page, not a step.
    let mut found: Vec<(usize, usize, Item)> = BUILT_INS
        .iter()
        .map(|b| {
            let languages: Vec<String> = b.languages.iter().map(|l| (*l).to_owned()).collect();
            (
                plugin_catalog::relevance(&languages, "", inputs.locales),
                stage_of(b.stage),
                built_in(b, inputs),
            )
        })
        .collect();
    for entry in inputs.catalog {
        let rank = plugin_catalog::relevance(&entry.locales, &entry.in_lang, inputs.locales);
        found.push((rank, stage_of(&entry.step.stage), from_entry(entry, inputs)));
    }
    let rank = |item: &Item| crate::processing_steps::known_rank(&item.step);
    found.sort_by(|a, b| (a.0, a.1, rank(&a.2), &a.2.id).cmp(&(b.0, b.1, rank(&b.2), &b.2.id)));
    found.into_iter().map(|(_, _, item)| item).collect()
}

fn stage_of(stage: &str) -> usize {
    match stage {
        "clean-up" => 1,
        "translate" => 2,
        _ => 3,
    }
}

fn is_on(inputs: &Inputs, step: &str) -> bool {
    inputs.enabled.iter().any(|on| on == step)
}

fn standing<'a>(inputs: &'a Inputs, step: &str) -> Option<&'a Standing> {
    inputs
        .rows
        .iter()
        .find(|row| row.id == step)
        .map(|row| &row.standing)
}

fn built_in(b: &BuiltIn, inputs: &Inputs) -> Item {
    let id = b.id;
    let (title, description, (heard, typed), license) =
        ((b.title)(), (b.description)(), (b.example)(), (b.credit)());
    let mut details = vec![Detail {
        label: gettextrs::gettext("Size"),
        text: gettextrs::gettext("Built in, nothing to install"),
    }];
    if !license.is_empty() {
        details.push(Detail {
            label: gettextrs::gettext("License"),
            text: license,
        });
    }
    let nothing_translated =
        id == crate::processing_steps::ORIGINAL && inputs.translate_from == inputs.source;
    let offer = if is_on(inputs, id) {
        Offer::Added
    } else if nothing_translated {
        Offer::NotApplicable(gettextrs::gettext("Applies after a translation"))
    } else {
        match standing(inputs, id) {
            Some(Standing::NotApplicable(reason)) => Offer::NotApplicable(reason.clone()),
            _ => Offer::Add,
        }
    };
    Item {
        id: id.to_owned(),
        step: id.to_owned(),
        target: None,
        title,
        description,
        example: Some(arrow(&heard, &typed)),
        details,
        offer,
        removable: None,
    }
}

fn arrow(heard: &str, typed: &str) -> String {
    format!("\u{201c}{heard}\u{201d} \u{2192} \u{201c}{typed}\u{201d}")
}

fn from_entry(entry: &Entry, inputs: &Inputs) -> Item {
    let translating = entry.step.id == TRANSLATE;
    let title = if translating {
        // TRANSLATORS: {from} and {to} are language names, such as "English" and "Français".
        let frame = gettextrs::gettext("Translate {from} to {to}");
        frame
            .replace("{from}", &language_name(&entry.in_lang))
            .replace("{to}", &language_name(&entry.out_lang))
    } else {
        plugin_catalog::label(&entry.step.label_key).unwrap_or_else(|| entry.step.id.clone())
    };
    let description = plugin_catalog::label(&entry.step.description_key).unwrap_or_default();
    let example = example(entry);
    let target = (translating
        && matches!(inputs.translate_from, Source::One(from) if *from != entry.out_lang))
    .then(|| entry.out_lang.clone());
    Item {
        id: entry.id.clone(),
        step: entry.step.id.clone(),
        target,
        title,
        description,
        example,
        details: details(entry),
        offer: offer(entry, inputs),
        removable: removable(entry, inputs),
    }
}

/// An installed model the user may take away; never while it is going.
fn removable(entry: &Entry, inputs: &Inputs) -> Option<Box<Entry>> {
    let offered = inputs.daemon.is_some_and(|d| d.offers(&entry.model));
    (offered && !inputs.removing.contains(&entry.id) && !inputs.installing.contains_key(&entry.id))
        .then(|| Box::new(entry.clone()))
}

fn example(entry: &Entry) -> Option<String> {
    match entry.step.id.as_str() {
        "tidy" => Some(arrow(
            &gettextrs::gettext("um so I, I think we should go"),
            &gettextrs::gettext("So I think we should go."),
        )),
        TRANSLATE if entry.in_lang == "en" && entry.out_lang == "fr" => Some(arrow(
            &gettextrs::gettext("I think we should go"),
            "Je pense que nous devrions y aller.",
        )),
        _ => None,
    }
}

/// What a person weighing the step wants to know: size, how good, whose.
fn details(entry: &Entry) -> Vec<Detail> {
    let mut details = vec![Detail {
        label: gettextrs::gettext("Download"),
        text: size(entry.download_bytes),
    }];
    if let Some(quality) = &entry.quality {
        details.push(Detail {
            label: gettextrs::gettext("Quality"),
            text: quality_note(&quality.source),
        });
    }
    if !entry.license.is_empty() {
        let mut text = entry.license.clone();
        if !entry.attribution.is_empty() {
            text = format!("{}\n{}", entry.attribution, text);
        }
        if !entry.source.is_empty() {
            text = format!("{text}\n{}", entry.source);
        }
        details.push(Detail {
            label: gettextrs::gettext("License"),
            text,
        });
    }
    details
}

/// The evidence behind a quality score, or that there is none yet.
pub fn quality_note(source: &str) -> String {
    if source.is_empty() || source.starts_with("carried over") {
        gettextrs::gettext("Not measured on a benchmark yet")
    } else {
        source.to_owned()
    }
}

fn offer(entry: &Entry, inputs: &Inputs) -> Offer {
    let step = &entry.step.id;
    if inputs.removing.contains(&entry.id) {
        return Offer::Removing;
    }
    if let Some(percent) = inputs.installing.get(&entry.id) {
        return Offer::Installing(*percent);
    }
    let added = added(entry, inputs);
    let Some(daemon) = inputs.daemon else {
        return if added { Offer::Added } else { Offer::Unknown };
    };
    // A step switched on whose model is gone offers the same install again.
    if !daemon.offers(&entry.model) {
        return if inputs.preparing.contains(&entry.id) {
            Offer::Preparing
        } else {
            Offer::Install(Box::new(entry.clone()))
        };
    }
    if added {
        return Offer::Added;
    }
    if let Some(Standing::NotApplicable(reason)) = standing(inputs, step) {
        return Offer::NotApplicable(reason.clone());
    }
    if step == TRANSLATE {
        if let Some(reason) = translate_mismatch(entry, inputs) {
            return Offer::NotApplicable(reason);
        }
    }
    Offer::Add
}

fn added(entry: &Entry, inputs: &Inputs) -> bool {
    if !is_on(inputs, &entry.step.id) {
        return false;
    }
    if entry.step.id != TRANSLATE {
        return true;
    }
    inputs.params.get(&param_key(TRANSLATE, TRANSLATE_TO)) == Some(&entry.out_lang)
}

/// Why a translation model cannot act on the text where it would go: neither
/// it nor a pivot through English reaches its target from there.
fn translate_mismatch(entry: &Entry, inputs: &Inputs) -> Option<String> {
    let Source::One(from) = inputs.translate_from else {
        return None;
    };
    if *from == entry.in_lang {
        return None;
    }
    let reachable = plugin_catalog::translation_legs(inputs.catalog, from, &entry.out_lang)
        .is_some_and(|legs| legs.iter().any(|leg| leg.id == entry.id));
    if reachable {
        return None;
    }
    // TRANSLATORS: {language} is a language name, such as "English".
    let frame = gettextrs::gettext("Applies when the text is {language}");
    Some(frame.replace("{language}", &language_name(&entry.in_lang)))
}

/// One row of the Add step page: a job, however many models can do it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Job {
    /// A job one model or the client does: the row is that item.
    Step(Item),
    /// Translate, whose row opens the language page.
    Translate(TranslateJob),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslateJob {
    pub title: String,
    /// From which language, and how many it reaches; else why none.
    pub subtitle: String,
    /// Whether the language page has a language to offer.
    pub open: bool,
}

/// The page's rows, in `items`' order: each step once, at the place of its
/// first item, with every translation model folded into one Translate row.
/// A step several models do shows the item furthest along (added, then
/// addable, then installing, then installable).
pub fn jobs(items: &[Item], from: &Source, languages: &[Language]) -> Vec<Job> {
    let rank = |offer: &Offer| match offer {
        Offer::Added => 0,
        Offer::Add => 1,
        Offer::Installing(_) => 2,
        Offer::Preparing => 3,
        Offer::Install(_) => 4,
        Offer::Removing => 5,
        Offer::NotApplicable(_) => 6,
        Offer::Unknown => 7,
    };
    let mut order: Vec<&str> = Vec::new();
    let mut best: BTreeMap<&str, &Item> = BTreeMap::new();
    for item in items {
        let step = item.step.as_str();
        match best.get(step) {
            None => {
                order.push(step);
                best.insert(step, item);
            }
            Some(kept) if rank(&item.offer) < rank(&kept.offer) => {
                best.insert(step, item);
            }
            Some(_) => {}
        }
    }
    order
        .into_iter()
        .map(|step| {
            if step == TRANSLATE {
                Job::Translate(translate_job(from, languages))
            } else {
                Job::Step(best[step].clone())
            }
        })
        .collect()
}

fn translate_job(from: &Source, languages: &[Language]) -> TranslateJob {
    let title = gettextrs::gettext("Translate");
    let Source::One(from) = from else {
        return TranslateJob {
            title,
            subtitle: gettextrs::gettext("Needs a single dictation language"),
            open: false,
        };
    };
    let name = language_name(from);
    if languages.is_empty() {
        // TRANSLATORS: {language} is a language name, such as "فارسی".
        let frame = gettextrs::gettext("No model translates {language} yet");
        return TranslateJob {
            title,
            subtitle: frame.replace("{language}", &name),
            open: false,
        };
    }
    let count = u32::try_from(languages.len()).unwrap_or(u32::MAX);
    let frame = gettextrs::ngettext(
        // TRANSLATORS: {language} is the language the text is in, such as "English"; {count} how many languages it can be translated into.
        "From {language} into {count} language",
        "From {language} into {count} languages",
        count,
    );
    TranslateJob {
        title,
        subtitle: frame
            .replace("{language}", &name)
            .replace("{count}", &count.to_string()),
        open: true,
    }
}

/// A language the text can be translated into at the catalog's place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Language {
    pub code: String,
    /// Its name in itself: "Deutsch".
    pub name: String,
    /// Its name in the interface language, when that differs: "German".
    pub english: Option<String>,
    pub state: LanguageState,
    /// One of the user's own languages, listed first.
    pub likely: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LanguageState {
    /// Every model it takes is installed.
    Ready,
    /// These entries download first: one, or both legs through English.
    Download {
        entries: Vec<Entry>,
        bytes: u64,
    },
    Installing(Option<u8>),
    /// Installed; the daemon has not offered it yet.
    Preparing,
    /// No daemon answers, so what is installed is not known.
    Unknown,
}

pub struct LanguageInputs<'a> {
    pub catalog: &'a [Entry],
    pub daemon: Option<&'a Offered>,
    /// The text's language where the translation would go.
    pub from: &'a Source,
    /// Installs running now, by key (`language_key` for a language).
    pub installing: &'a BTreeMap<String, Option<u8>>,
    pub preparing: &'a BTreeSet<String>,
    /// The user's languages, most likely first: desktop locales, targets
    /// already in a chain.
    pub likely: &'a [String],
    /// Whether translating into a language here would leave a later step
    /// of the chain translating what no model can.
    pub strands: &'a dyn Fn(&str) -> bool,
}

/// The install key of the models a translation into `code` needs.
pub fn language_key(code: &str) -> String {
    format!("translate>{code}")
}

/// Every language the text can be translated into from where it is, by a
/// model installed or in the catalogs, directly or through English: the
/// likely ones first in their order, then the rest by name. A language no
/// model reaches from here, or one that would strand a later step, is not
/// listed: the page offers only what works.
pub fn languages(inputs: &LanguageInputs) -> Vec<Language> {
    let Source::One(from) = inputs.from else {
        return Vec::new();
    };
    let mut codes: BTreeSet<String> = inputs
        .catalog
        .iter()
        .filter(|e| e.step.id == TRANSLATE)
        .flat_map(|e| [e.in_lang.clone(), e.out_lang.clone()])
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
    let mut found: Vec<Language> = codes
        .into_iter()
        .filter(|code| !code.is_empty() && code != from)
        .filter(|code| crate::processing_steps::routable(inputs.catalog, inputs.daemon, from, code))
        .filter(|code| !(inputs.strands)(code))
        .map(|code| {
            let name = language_name(&code);
            let english = exonym(&code).filter(|english| *english != name);
            Language {
                state: language_state(inputs, from, &code),
                likely: inputs.likely.contains(&code),
                code,
                name,
                english,
            }
        })
        .collect();
    let place = |l: &Language| {
        inputs
            .likely
            .iter()
            .position(|code| *code == l.code)
            .unwrap_or(inputs.likely.len())
    };
    found.sort_by_cached_key(|l| {
        (
            place(l),
            fold(l.english.as_deref().unwrap_or(&l.name)),
            l.code.clone(),
        )
    });
    found
}

fn language_state(inputs: &LanguageInputs, from: &str, to: &str) -> LanguageState {
    let key = language_key(to);
    if let Some(percent) = inputs.installing.get(&key) {
        return LanguageState::Installing(*percent);
    }
    let Some(daemon) = inputs.daemon else {
        return LanguageState::Unknown;
    };
    if daemon
        .translations
        .iter()
        .any(|(f, t)| f == from && t == to)
    {
        return LanguageState::Ready;
    }
    let legs = plugin_catalog::translation_legs(inputs.catalog, from, to).unwrap_or_default();
    let entries: Vec<Entry> = legs
        .into_iter()
        .filter(|leg| !daemon.offers(&leg.model))
        .cloned()
        .collect();
    if entries.is_empty() {
        LanguageState::Ready
    } else if inputs.preparing.contains(&key) {
        LanguageState::Preparing
    } else {
        LanguageState::Download {
            bytes: entries.iter().map(|e| e.download_bytes).sum(),
            entries,
        }
    }
}

/// Whether `language` answers a search for `query`: by its own name, its
/// name in the interface language or its code, ignoring case and accents.
pub fn matches(language: &Language, query: &str) -> bool {
    let query = fold(query.trim());
    query.is_empty()
        || [
            Some(language.name.as_str()),
            language.english.as_deref(),
            Some(language.code.as_str()),
        ]
        .into_iter()
        .flatten()
        .any(|text| fold(text).contains(&query))
}

/// Lower case with accents taken off: "Čeština" reads "cestina".
fn fold(text: &str) -> String {
    gio::glib::normalize(text, gio::glib::NormalizeMode::All)
        .chars()
        .filter(|c| {
            !matches!(u32::from(*c),
                0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// The stored steps and parameters after `item` is added: its step on, and a
/// translation's target set.
pub fn with_added(
    ids: &[String],
    params: &Params,
    item: &Item,
    locales: &[String],
) -> (Vec<String>, Params) {
    let mut params = params.clone();
    if item.step == SCRIPT {
        params = crate::processing_steps::with_script_on(&params, locales);
    }
    if let Some(to) = &item.target {
        params = crate::processing_steps::with_target(&params, Some(to));
    }
    (
        crate::processing_steps::with_enabled(ids, &item.step, true),
        params,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processing_steps::{rows, Inputs as RowInputs};
    use serde_json::json;

    fn entry(id: &str, step: &str, stage: &str, from: &str, to: &str) -> Entry {
        serde_json::from_value(json!({
            "id": id, "snap": "myna-translation", "component": id, "model": format!("m-{id}"),
            "task": "text.translate", "in_lang": from, "out_lang": to, "download_bytes": 33_000_000,
            "locales": [from], "license": "MPL-2.0", "attribution": "Mozilla", "source": "https://x",
            "quality": {"value": 0.7, "source": "chrF 0.700"},
            "step": {"id": step, "stage": stage, "label_key": format!("step.{step}.name"),
                     "description_key": format!("step.{step}.description"),
                     "example_key": format!("step.{step}.example")},
        }))
        .unwrap()
    }

    fn catalog() -> Vec<Entry> {
        vec![
            entry("tidy", "tidy", "clean-up", "en", "en"),
            entry("en-fr", "translate", "translate", "en", "fr"),
            entry("fr-en", "translate", "translate", "fr", "en"),
        ]
    }

    const BOTH: &str = r#"{"models":[{"model":"m-tidy","task":"text.cleanup"},{"model":"m-en-fr","task":"text.translate","params":[{"name":"language","values":["en"]},{"name":"to","values":["fr"]}]}],"steps":[]}"#;

    struct Case {
        enabled: Vec<String>,
        params: Params,
        language: &'static str,
        /// The text's language where a translation would go, when an
        /// earlier step changed it.
        place: Option<&'static str>,
        daemon: Option<Offered>,
        installing: BTreeMap<String, Option<u8>>,
        preparing: BTreeSet<String>,
        removing: BTreeSet<String>,
        locales: Vec<String>,
    }

    fn case() -> Case {
        Case {
            enabled: Vec::new(),
            params: Params::new(),
            language: "en",
            place: None,
            daemon: Offered::parse(BOTH),
            installing: BTreeMap::new(),
            preparing: BTreeSet::new(),
            removing: BTreeSet::new(),
            locales: vec!["en_US.UTF-8".into()],
        }
    }

    fn listed(case: &Case) -> Vec<Item> {
        let catalog = catalog();
        let source = Source::of_setting(case.language);
        let step_rows = rows(&RowInputs {
            language_unset: false,
            enabled: &case.enabled,
            params: &case.params,
            source: &source,
            catalog: &catalog,
            daemon: case.daemon.as_ref(),
            installing: &case.installing,
            preparing: &case.preparing,
            locales: &case.locales,
        });
        let place = case
            .place
            .map_or_else(|| source.clone(), Source::of_setting);
        items(&Inputs {
            catalog: &catalog,
            enabled: &case.enabled,
            params: &case.params,
            source: &source,
            translate_from: &place,
            daemon: case.daemon.as_ref(),
            installing: &case.installing,
            preparing: &case.preparing,
            removing: &case.removing,
            locales: &case.locales,
            rows: &step_rows,
        })
    }

    fn find<'a>(items: &'a [Item], id: &str) -> &'a Item {
        items.iter().find(|i| i.id == id).unwrap()
    }

    /// English dictation, but an earlier step leaves the text in Persian
    /// where a translation would go: English to French cannot run there and
    /// nothing takes Persian to French, so it is not offered.
    #[test]
    fn a_translation_is_judged_where_it_would_go_in_the_chain() {
        let here = listed(&case());
        assert_eq!(find(&here, "en-fr").offer, Offer::Add);
        let after_persian = listed(&Case {
            place: Some("fa"),
            ..case()
        });
        assert!(
            matches!(find(&after_persian, "en-fr").offer, Offer::NotApplicable(_)),
            "{:?}",
            find(&after_persian, "en-fr").offer
        );
        let after_french = listed(&Case {
            place: Some("fr"),
            ..case()
        });
        assert_eq!(find(&after_french, "en-fr").target, None);
    }

    fn titles(jobs: &[Job]) -> Vec<String> {
        jobs.iter()
            .map(|job| match job {
                Job::Step(item) => item.title.clone(),
                Job::Translate(t) => t.title.clone(),
            })
            .collect()
    }

    /// Charles, 2026-11-23: one row per job, never one per model.
    #[test]
    fn the_page_lists_each_job_once_and_folds_translations_into_one() {
        let items = listed(&case());
        let en = Source::One("en".into());
        let found = jobs(&items, &en, &language_list(&case(), "en", &[]));
        assert_eq!(
            titles(&found),
            [
                "Spoken commands",
                "Tidy up speech",
                "Spoken emoji",
                "Snippets",
                "Code casing",
                "Spoken addresses",
                "Numbers as digits",
                "Translate",
                "Keep the original",
                "Typography",
                "Chinese script"
            ]
        );
        let Job::Translate(translate) = &found[7] else {
            panic!("not the Translate job")
        };
        assert!(translate.open);
        assert!(
            translate.subtitle.contains("English"),
            "{}",
            translate.subtitle
        );
    }

    #[test]
    fn translate_cannot_open_without_a_language_to_translate_from() {
        let items = listed(&case());
        let found = jobs(&items, &Source::Unknown, &[]);
        let Some(Job::Translate(translate)) = found.iter().find(|j| matches!(j, Job::Translate(_)))
        else {
            panic!("no Translate job")
        };
        assert!(!translate.open);
        let fa = Source::One("fa".into());
        let found = jobs(&items, &fa, &language_list(&case(), "fa", &[]));
        let Some(Job::Translate(translate)) = found.iter().find(|j| matches!(j, Job::Translate(_)))
        else {
            panic!("no Translate job")
        };
        assert!(!translate.open, "nothing takes Persian anywhere");
        assert!(
            translate.subtitle.contains("فارسی"),
            "{}",
            translate.subtitle
        );
    }

    fn language_list(case: &Case, from: &str, likely: &[&str]) -> Vec<Language> {
        let catalog = catalog();
        let likely: Vec<String> = likely.iter().map(|l| (*l).to_owned()).collect();
        languages(&LanguageInputs {
            catalog: &catalog,
            daemon: case.daemon.as_ref(),
            from: &Source::One(from.into()),
            installing: &case.installing,
            preparing: &case.preparing,
            likely: &likely,
            strands: &|_| false,
        })
    }

    fn codes(languages: &[Language]) -> Vec<&str> {
        languages.iter().map(|l| l.code.as_str()).collect()
    }

    #[test]
    fn the_language_page_lists_what_the_text_can_be_translated_into() {
        // en-fr installed, fr-en offered by the catalog.
        let from_en = language_list(&case(), "en", &[]);
        assert_eq!(codes(&from_en), ["fr"]);
        assert_eq!(from_en[0].state, LanguageState::Ready);
        assert_eq!(from_en[0].name, "Français");
        assert_eq!(from_en[0].english.as_deref(), Some("French"));
        let from_fr = language_list(&case(), "fr", &[]);
        assert_eq!(codes(&from_fr), ["en"]);
        let LanguageState::Download { entries, bytes } = &from_fr[0].state else {
            panic!("{:?}", from_fr[0].state)
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(*bytes, 33_000_000);
        assert!(language_list(&case(), "fa", &[]).is_empty());
    }

    #[test]
    fn a_pivot_needs_both_legs_and_says_what_downloads() {
        let mut case = case();
        case.daemon = Offered::parse(r#"{"models":[],"steps":[]}"#);
        let mut catalog = catalog();
        catalog.push(entry("en-de", "translate", "translate", "en", "de"));
        let languages = languages(&LanguageInputs {
            catalog: &catalog,
            daemon: case.daemon.as_ref(),
            from: &Source::One("fr".into()),
            installing: &case.installing,
            preparing: &case.preparing,
            likely: &[],
            strands: &|_| false,
        });
        assert_eq!(codes(&languages), ["en", "de"], "English, then by name");
        let LanguageState::Download { entries, bytes } = &languages[1].state else {
            panic!("{:?}", languages[1].state)
        };
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["fr-en", "en-de"]);
        assert_eq!(*bytes, 66_000_000);
    }

    #[test]
    fn likely_languages_come_first_and_stranding_ones_are_left_out() {
        let mut catalog = catalog();
        for to in ["de", "es", "it"] {
            catalog.push(entry(
                &format!("en-{to}"),
                "translate",
                "translate",
                "en",
                to,
            ));
        }
        let case = case();
        let list = |likely: &[String], strands: &dyn Fn(&str) -> bool| {
            languages(&LanguageInputs {
                catalog: &catalog,
                daemon: case.daemon.as_ref(),
                from: &Source::One("en".into()),
                installing: &case.installing,
                preparing: &case.preparing,
                likely,
                strands,
            })
        };
        let plain = list(&[], &|_| false);
        assert_eq!(
            codes(&plain),
            ["fr", "de", "it", "es"],
            "by name in the UI language"
        );
        let liked = list(&["es".into(), "it".into()], &|_| false);
        assert_eq!(codes(&liked), ["es", "it", "fr", "de"]);
        assert!(liked[0].likely && !liked[2].likely);
        let kept = list(&[], &|code| code == "de");
        assert_eq!(codes(&kept), ["fr", "it", "es"]);
    }

    #[test]
    fn an_install_running_or_settling_shows_on_its_language() {
        let mut case = case();
        case.installing.insert(language_key("en"), Some(40));
        let from_fr = language_list(&case, "fr", &[]);
        assert_eq!(from_fr[0].state, LanguageState::Installing(Some(40)));
        case.installing.clear();
        case.preparing.insert(language_key("en"));
        let from_fr = language_list(&case, "fr", &[]);
        assert_eq!(from_fr[0].state, LanguageState::Preparing);
        case.daemon = None;
        case.preparing.clear();
        assert_eq!(
            language_list(&case, "fr", &[])[0].state,
            LanguageState::Unknown
        );
    }

    #[test]
    fn search_matches_either_name_or_the_code_whatever_the_accents() {
        let mut catalog = catalog();
        catalog.push(entry("en-cs", "translate", "translate", "en", "cs"));
        catalog.push(entry("en-de", "translate", "translate", "en", "de"));
        let case = case();
        let all = languages(&LanguageInputs {
            catalog: &catalog,
            daemon: case.daemon.as_ref(),
            from: &Source::One("en".into()),
            installing: &case.installing,
            preparing: &case.preparing,
            likely: &[],
            strands: &|_| false,
        });
        let hits = |query: &str| -> Vec<&str> {
            all.iter()
                .filter(|l| matches(l, query))
                .map(|l| l.code.as_str())
                .collect()
        };
        assert_eq!(hits("german"), ["de"]);
        assert_eq!(hits("deu"), ["de"]);
        assert_eq!(hits("cestina"), ["cs"]);
        assert_eq!(hits("  CZECH "), ["cs"]);
        assert_eq!(hits("fr"), ["fr"]);
        assert_eq!(hits(""), ["cs", "fr", "de"]);
        assert!(hits("klingon").is_empty());
    }

    #[test]
    fn keeping_the_original_is_offered_only_after_a_translation() {
        let en = Source::One("en".into());
        let fr = Source::One("fr".into());
        let offer = |translate_from: &Source| {
            let all = items(&Inputs {
                catalog: &[],
                enabled: &[],
                params: &Params::new(),
                source: &en,
                translate_from,
                daemon: None,
                installing: &BTreeMap::new(),
                preparing: &BTreeSet::new(),
                removing: &BTreeSet::new(),
                locales: &[],
                rows: &[],
            });
            find(&all, "original").offer.clone()
        };
        assert_eq!(offer(&fr), Offer::Add);
        assert_eq!(
            offer(&en),
            Offer::NotApplicable("Applies after a translation".into())
        );
    }

    #[test]
    fn built_ins_and_every_entry_are_listed_with_their_facts() {
        let all = listed(&case());
        let ids: Vec<_> = all.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids.len(), 12, "{ids:?}");
        assert!(!ids.contains(&"fit"), "fitting is a switch, not a step");
        let fr = find(&all, "en-fr");
        assert_eq!(fr.title, "Translate English to Français");
        assert_eq!(fr.step, "translate");
        assert_eq!(fr.target.as_deref(), Some("fr"));
        let labels: Vec<_> = fr.details.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(labels, ["Download", "Quality", "License"]);
        assert_eq!(fr.details[0].text, "33 MB");
        assert_eq!(fr.details[1].text, "chrF 0.700");
        assert_eq!(fr.details[2].text, "Mozilla\nMPL-2.0\nhttps://x");
        assert!(fr.example.as_deref().unwrap().contains("Je pense"));
        let tidy = find(&all, "tidy");
        assert!(tidy.example.as_deref().unwrap().contains("So I think"));
        let script = find(&all, "script");
        assert_eq!(script.details[0].text, "Built in, nothing to install");
        assert!(script.details[1].text.contains("OpenCC"));
    }

    #[test]
    fn an_entry_is_not_made_up_when_the_catalog_has_none() {
        let mut empty = case();
        empty.daemon = None;
        let source = Source::of_setting("en");
        let all = items(&Inputs {
            catalog: &[],
            enabled: &[],
            params: &Params::new(),
            source: &source,
            translate_from: &source,
            daemon: None,
            installing: &BTreeMap::new(),
            preparing: &BTreeSet::new(),
            removing: &BTreeSet::new(),
            locales: &[],
            rows: &[],
        });
        assert_eq!(all.len(), BUILT_INS.len());
    }

    #[test]
    fn what_suits_the_users_locales_comes_first() {
        let mut french = case();
        french.locales = vec!["fr_FR.UTF-8".into()];
        assert_eq!(listed(&french)[0].id, "fr-en");
        let mut chinese = case();
        chinese.locales = vec!["zh_TW.UTF-8".into()];
        assert_eq!(listed(&chinese)[0].id, "script");
        let english = listed(&case());
        let ids: Vec<_> = english.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "commands",
                "tidy",
                "emoji",
                "snippets",
                "casing",
                "addresses",
                "numbers",
                "en-fr",
                "fr-en",
                "original",
                "typography",
                "script"
            ],
            "{ids:?}"
        );
    }

    #[test]
    fn an_installed_item_can_be_added_and_a_missing_one_installed() {
        let all = listed(&case());
        assert_eq!(find(&all, "tidy").offer, Offer::Add);
        assert_eq!(find(&all, "en-fr").offer, Offer::Add);
        let Offer::Install(entry) = &find(&all, "fr-en").offer else {
            panic!("{:?}", find(&all, "fr-en"));
        };
        assert_eq!(entry.id, "fr-en");
    }

    #[test]
    fn a_step_already_on_is_added() {
        let mut on = case();
        on.enabled = vec!["tidy".into(), "translate".into()];
        on.params
            .insert(param_key(TRANSLATE, TRANSLATE_TO), "fr".into());
        let all = listed(&on);
        assert_eq!(find(&all, "tidy").offer, Offer::Added);
        assert_eq!(find(&all, "en-fr").offer, Offer::Added);
        on.params
            .insert(param_key(TRANSLATE, TRANSLATE_TO), "de".into());
        assert_eq!(find(&listed(&on), "en-fr").offer, Offer::Add);
        on.params.clear();
        on.enabled = vec!["translate".into()];
        assert_eq!(find(&listed(&on), "en-fr").offer, Offer::Add);
    }

    #[test]
    fn an_install_shows_progress_then_preparing_then_add() {
        let mut busy = case();
        busy.daemon = Offered::parse(r#"{"models":[],"steps":[]}"#);
        busy.installing.insert("en-fr".into(), Some(40));
        assert_eq!(
            find(&listed(&busy), "en-fr").offer,
            Offer::Installing(Some(40))
        );
        busy.installing.clear();
        busy.preparing.insert("en-fr".into());
        assert_eq!(find(&listed(&busy), "en-fr").offer, Offer::Preparing);
        busy.preparing.clear();
        assert!(matches!(
            find(&listed(&busy), "en-fr").offer,
            Offer::Install(_)
        ));
    }

    #[test]
    fn only_an_installed_model_can_be_removed_and_not_while_it_is_going() {
        let mut case = case();
        let all = listed(&case);
        assert_eq!(find(&all, "tidy").removable.as_ref().unwrap().id, "tidy");
        assert_eq!(find(&all, "en-fr").removable.as_ref().unwrap().id, "en-fr");
        assert!(find(&all, "fr-en").removable.is_none(), "not installed");
        assert!(find(&all, "script").removable.is_none(), "built in");
        case.removing.insert("en-fr".into());
        let all = listed(&case);
        assert_eq!(find(&all, "en-fr").offer, Offer::Removing);
        assert!(find(&all, "en-fr").removable.is_none());
        assert!(find(&all, "tidy").removable.is_some());
        case.removing.clear();
        case.installing.insert("en-fr".into(), None);
        assert!(find(&listed(&case), "en-fr").removable.is_none());
        case.installing.clear();
        case.daemon = None;
        assert!(find(&listed(&case), "tidy").removable.is_none());
    }

    #[test]
    fn a_step_on_whose_model_is_gone_offers_that_entrys_install_again() {
        let mut case = case();
        case.enabled = vec!["tidy".into(), "translate".into()];
        case.params
            .insert(param_key(TRANSLATE, TRANSLATE_TO), "fr".into());
        let all = listed(&case);
        assert_eq!(find(&all, "tidy").offer, Offer::Added);
        assert_eq!(find(&all, "en-fr").offer, Offer::Added);
        case.daemon = Offered::parse(r#"{"models":[],"steps":[]}"#);
        let all = listed(&case);
        let Offer::Install(entry) = &find(&all, "en-fr").offer else {
            panic!("{:?}", find(&all, "en-fr").offer);
        };
        assert_eq!(entry.id, "en-fr");
        assert!(matches!(find(&all, "tidy").offer, Offer::Install(_)));
        assert!(find(&all, "tidy").removable.is_none());
        case.preparing.insert("en-fr".into());
        assert_eq!(find(&listed(&case), "en-fr").offer, Offer::Preparing);
    }

    #[test]
    fn with_no_daemon_nothing_is_known_about_the_models() {
        let mut none = case();
        none.daemon = None;
        let all = listed(&none);
        assert_eq!(find(&all, "tidy").offer, Offer::Unknown);
        assert!(
            !matches!(find(&all, "script").offer, Offer::Unknown),
            "a built-in step needs no daemon"
        );
    }

    #[test]
    fn a_model_that_cannot_act_on_the_dictation_says_why() {
        let mut french = case();
        french.language = "fr";
        let all = listed(&french);
        assert_eq!(
            find(&all, "en-fr").offer,
            Offer::NotApplicable("Applies when the text is English".into())
        );
        assert!(matches!(find(&all, "tidy").offer, Offer::NotApplicable(_)));
        assert_eq!(find(&all, "en-fr").target, None);
        let mut several = case();
        several.language = "en,fr";
        let all = listed(&several);
        assert!(matches!(find(&all, "en-fr").offer, Offer::NotApplicable(_)));
        assert_eq!(find(&all, "en-fr").target, None);
        assert_eq!(
            find(&listed(&case()), "script").offer,
            Offer::NotApplicable("Applies when the text is Chinese".into())
        );
    }

    #[test]
    fn a_pivot_leg_is_usable_when_it_completes_the_route() {
        let catalog = vec![
            entry("fr-en", "translate", "translate", "fr", "en"),
            entry("en-de", "translate", "translate", "en", "de"),
            entry("xx-de", "translate", "translate", "xx", "de"),
        ];
        let source = Source::of_setting("fr");
        let daemon = Offered::parse(
            r#"{"models":[{"model":"m-fr-en","task":"text.translate"},{"model":"m-en-de","task":"text.translate"},{"model":"m-xx-de","task":"text.translate"}],"steps":[]}"#,
        );
        let step_rows = Vec::new();
        let all = items(&Inputs {
            catalog: &catalog,
            enabled: &[],
            params: &Params::new(),
            source: &source,
            translate_from: &source,
            daemon: daemon.as_ref(),
            installing: &BTreeMap::new(),
            preparing: &BTreeSet::new(),
            removing: &BTreeSet::new(),
            locales: &[],
            rows: &step_rows,
        });
        assert_eq!(find(&all, "en-de").offer, Offer::Add);
        assert_eq!(find(&all, "fr-en").offer, Offer::Add);
        assert_eq!(
            find(&all, "xx-de").offer,
            Offer::NotApplicable("Applies when the text is xx".into())
        );
    }

    #[test]
    fn only_the_french_pair_has_a_worked_example() {
        let examples: Vec<(&str, bool)> = [("en", "fr"), ("en", "de"), ("de", "fr"), ("fr", "en")]
            .iter()
            .map(|(from, to)| {
                let e = entry("x", "translate", "translate", from, to);
                (*to, example(&e).is_some())
            })
            .collect();
        assert_eq!(
            examples,
            [("fr", true), ("de", false), ("fr", false), ("en", false)]
        );
    }

    #[test]
    fn adding_switches_the_step_on_and_picks_the_target() {
        let all = listed(&case());
        let (ids, params) = with_added(
            &["tidy".to_owned()],
            &Params::new(),
            find(&all, "en-fr"),
            &[],
        );
        assert_eq!(ids, ["tidy", "translate"]);
        assert_eq!(params.get("translate.to").map(String::as_str), Some("fr"));
        let (ids, params) = with_added(&[], &Params::new(), find(&all, "tidy"), &[]);
        assert_eq!(ids, ["tidy"]);
        assert!(params.is_empty());
        let (ids, params) = with_added(
            &[],
            &Params::new(),
            find(&all, "script"),
            &["zh_TW.UTF-8".to_owned()],
        );
        assert_eq!(ids, ["script"]);
        assert_eq!(
            params.get("script.mode").map(String::as_str),
            Some("traditional-taiwan")
        );
    }

    #[test]
    fn items_of_one_rank_follow_the_stage_order_not_the_ids() {
        let catalog = vec![
            entry("a-late", "later", "target-language", "en", "en"),
            entry("m-clean", "tidy", "clean-up", "en", "en"),
            entry("z-translate", "translate", "translate", "en", "fr"),
        ];
        let source = Source::of_setting("en");
        let all = items(&Inputs {
            catalog: &catalog,
            enabled: &[],
            params: &Params::new(),
            source: &source,
            translate_from: &source,
            daemon: None,
            installing: &BTreeMap::new(),
            preparing: &BTreeSet::new(),
            removing: &BTreeSet::new(),
            locales: &["en".to_owned()],
            rows: &[],
        });
        let ids: Vec<_> = all.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "commands",
                "m-clean",
                "emoji",
                "snippets",
                "casing",
                "addresses",
                "numbers",
                "z-translate",
                "a-late",
                "original",
                "typography",
                "script"
            ],
            "{ids:?}"
        );
    }

    #[test]
    fn a_quality_without_evidence_is_not_passed_off_as_measured() {
        assert_eq!(quality_note("chrF 0.7"), "chrF 0.7");
        assert_eq!(quality_note(""), "Not measured on a benchmark yet");
        assert_eq!(
            quality_note("carried over from the shipped manifest"),
            "Not measured on a benchmark yet"
        );
    }
}
