//! What the Add step page lists, GTK-free: every step the merged catalogs
//! offer, plus the ones the client carries, each with what it is, what it
//! costs and what can be done with it. Entries come from the catalog schema
//! only; nothing here knows a plugin by name.

use myna_core::processing::{param_key, Params};

use crate::plugin_catalog::{self, Entry};
use std::collections::{BTreeMap, BTreeSet};

use crate::processing_steps::{
    exonym, language_name, size, Offered, Row, Source, Standing, SCRIPT, TRANSLATE, TRANSLATE_TO,
};

/// The languages the Chinese script step is useful for.
const CHINESE: [&str; 3] = ["zh", "yue", "cmn"];

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
    let mut found: Vec<(usize, usize, Item)> = vec![(
        plugin_catalog::relevance(&CHINESE.map(str::to_owned), "", inputs.locales),
        stage_of(SCRIPT),
        built_in(SCRIPT, inputs),
    )];
    for entry in inputs.catalog {
        let rank = plugin_catalog::relevance(&entry.locales, &entry.in_lang, inputs.locales);
        found.push((rank, stage_of(&entry.step.stage), from_entry(entry, inputs)));
    }
    found.sort_by(|a, b| (a.0, a.1, &a.2.id).cmp(&(b.0, b.1, &b.2.id)));
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

fn built_in(id: &str, inputs: &Inputs) -> Item {
    let (title, description, example, license) = (
        gettextrs::gettext("Chinese script"),
        gettextrs::gettext("Types Chinese in the characters you choose. Other text is left alone."),
        Some(("\u{8f6f}\u{4ef6}".to_owned(), "\u{8edf}\u{9ad4}".to_owned())),
        // TRANSLATORS: credits the libraries and word lists behind Chinese script conversion.
        gettextrs::gettext("opencc-jieba-rs (MIT); OpenCC conversion lists (Apache-2.0)"),
    );
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
    let offer = if is_on(inputs, id) {
        Offer::Added
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
        example: example.map(|(heard, typed)| arrow(&heard, &typed)),
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

/// A language the text can be translated into from where it is: one row of
/// the catalog's language page, and one entry of the chain editor's Translate
/// picker.
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
    /// Chosen in the editor, but no model installed or offered translates
    /// into it: the picker lists it so the choice is not lost.
    NoModel,
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
    /// The target a chain already translates into, listed even when no model
    /// reaches it: the editor's picker shows what is chosen, the page offers
    /// only what works.
    pub chosen: Option<&'a str>,
}

/// The install key of the models a translation into `code` needs.
pub fn language_key(code: &str) -> String {
    format!("translate>{code}")
}

/// Every language the text can be translated into from where it is, by a
/// model installed or in the catalogs, directly or through English: the
/// likely ones first in their order, then the rest by name. A language no
/// model reaches from here, or one that would strand a later step, is not
/// listed: the page offers only what works. The one a chain already chose is
/// listed whatever it takes, so the editor's picker shows the choice it has.
pub fn languages(inputs: &LanguageInputs) -> Vec<Language> {
    let Source::One(from) = inputs.from else {
        return Vec::new();
    };
    let chosen = inputs
        .chosen
        .filter(|code| !code.is_empty() && *code != from);
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
        .filter(|code| {
            Some(code.as_str()) == chosen
                || crate::processing_steps::routable(inputs.catalog, inputs.daemon, from, code)
        })
        .filter(|code| Some(code.as_str()) == chosen || !(inputs.strands)(code))
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
    if !crate::processing_steps::routable(inputs.catalog, inputs.daemon, from, to) {
        return LanguageState::NoModel;
    }
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
