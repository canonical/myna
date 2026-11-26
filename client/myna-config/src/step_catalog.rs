//! What the Add step page lists, GTK-free: every step the merged catalogs
//! offer, plus the ones the client carries, each with what it is, what it
//! costs and what can be done with it. Entries come from the catalog schema
//! only; nothing here knows a plugin by name.

use myna_core::chains::ChainStep;

use crate::languages::{exonym, language_in_text, language_name, Source};
use crate::plugin_catalog::{self, Entry};
use crate::processing_steps::{self as steps, size, Offered, Row, Standing, SCRIPT, TRANSLATE};
use std::collections::{BTreeMap, BTreeSet};

/// The languages the Chinese script step is useful for.
const CHINESE: [&str; 3] = ["zh", "yue", "cmn"];

/// What the page needs to judge each step: the chain it adds to, and what
/// the daemon offers now.
pub struct Inputs<'a> {
    pub catalog: &'a [Entry],
    /// The chain's steps, as the editor judges them.
    pub rows: &'a [Row],
    /// The languages the text can be translated into from where a
    /// translation would go, as the language page lists them.
    pub languages: &'a [Language],
    /// The language of the text where an added translation would go in the
    /// chain: the dictation language, or what an earlier Translate made it.
    pub translate_from: &'a Source,
    /// The dictation language, which a step the chain does not hold reads.
    pub source: &'a Source,
    /// The `language` setting is empty: the model decides.
    pub language_unset: bool,
    pub daemon: Option<&'a Offered>,
    /// Installs running now, by step id, with snapd's percentage.
    pub installing: &'a BTreeMap<String, Option<u8>>,
    /// Step ids installed whose models the daemon has not offered yet.
    pub preparing: &'a BTreeSet<String>,
    /// Entry ids whose models are being removed.
    pub removing: &'a BTreeSet<String>,
    pub locales: &'a [String],
}

/// The page's rows, in the order it lists them: what suits the user's
/// locales first, then stage order. One row per step, the model furthest
/// along where several do it, and one Translate row that opens the
/// language page.
pub fn rows(inputs: &Inputs) -> Vec<Row> {
    // Fitting to the field is a switch on the Processing page, not a step.
    let mut ranked: Vec<(usize, usize, Row)> = vec![(
        plugin_catalog::relevance(&CHINESE.map(str::to_owned), "", inputs.locales),
        stage_of(SCRIPT),
        script_row(inputs),
    )];
    for entry in inputs.catalog.iter().filter(|e| e.step.id != TRANSLATE) {
        let rank = plugin_catalog::relevance(&entry.locales, &entry.in_lang, inputs.locales);
        ranked.push((rank, stage_of(&entry.step.stage), entry_row(entry, inputs)));
    }
    let translations: Vec<&Entry> = inputs
        .catalog
        .iter()
        .filter(|e| e.step.id == TRANSLATE)
        .collect();
    if !translations.is_empty() {
        let rank = translations
            .iter()
            .map(|e| plugin_catalog::relevance(&e.locales, &e.in_lang, inputs.locales))
            .min()
            .unwrap_or(0);
        ranked.push((rank, stage_of(TRANSLATE), translate_row(inputs)));
    }
    ranked.sort_by(|a, b| (a.0, a.1, &a.2.id).cmp(&(b.0, b.1, &b.2.id)));
    let mut out: Vec<Row> = Vec::new();
    for (_, _, row) in ranked {
        match out.iter().position(|have| have.id == row.id) {
            None => out.push(row),
            Some(at) if rank(&row) < rank(&out[at]) => out[at] = row,
            Some(_) => {}
        }
    }
    out
}

fn stage_of(stage: &str) -> usize {
    match stage {
        "clean-up" => 1,
        "translate" => 2,
        _ => 3,
    }
}

/// How far along a row is, so a step several models do shows the one to
/// act on: added, addable, installing, installable, then what cannot run.
fn rank(row: &Row) -> u8 {
    match &row.standing {
        Standing::Ready | Standing::NotApplicable(_) if row.on => 0,
        Standing::Ready | Standing::NotApplicable(_) => 1,
        Standing::Installing(_) => 2,
        Standing::Preparing => 3,
        Standing::Missing(_) => 4,
        Standing::Unavailable(_) => 5,
        Standing::Unknown => 6,
    }
}

/// The Chinese script step: built in, so it has nothing to download.
fn script_row(inputs: &Inputs) -> Row {
    let mut row = judge(SCRIPT, inputs);
    row.title = gettextrs::gettext("Chinese script");
    row.description =
        gettextrs::gettext("Types Chinese in the characters you choose. Other text is left alone.");
    row.example = Some(arrow("\u{8f6f}\u{4ef6}", "\u{8edf}\u{9ad4}"));
    row.details = vec![(
        gettextrs::gettext("Size"),
        gettextrs::gettext("Built in, nothing to install"),
    )];
    // TRANSLATORS: credits the libraries and word lists behind Chinese script conversion.
    let license = gettextrs::gettext("opencc-jieba-rs (MIT); OpenCC conversion lists (Apache-2.0)");
    if !license.is_empty() {
        row.details.push((gettextrs::gettext("License"), license));
    }
    row
}

/// One catalog entry's row, with what a person weighing the step wants.
fn entry_row(entry: &Entry, inputs: &Inputs) -> Row {
    let mut row = judge(&entry.step.id, inputs);
    row.title =
        plugin_catalog::label(&entry.step.label_key).unwrap_or_else(|| entry.step.id.clone());
    row.description = plugin_catalog::label(&entry.step.description_key).unwrap_or_default();
    row.example = example(entry);
    row.details = details(entry);
    let installed = inputs
        .daemon
        .is_some_and(|daemon| daemon.offers(&entry.model));
    let going = inputs.removing.contains(&entry.id) || inputs.installing.contains_key(&row.id);
    row.removable = (installed && !going).then(|| Box::new(entry.clone()));
    row.entry = Some(Box::new(entry.clone()));
    row
}

/// The row of step `id` as the page judges it: the chain's own row when the
/// chain holds it, else one read where the step would go.
fn judge(id: &str, inputs: &Inputs) -> Row {
    if let Some(row) = inputs.rows.iter().find(|row| row.id == id) {
        return row.clone();
    }
    let step = ChainStep::new(id);
    let mut row = steps::row(&step, inputs.source, &judging(inputs));
    // A step the chain does not hold is not switched on.
    row.on = false;
    row
}

/// The row builder's view of what the page knows.
fn judging<'a>(inputs: &'a Inputs<'a>) -> steps::Inputs<'a> {
    steps::Inputs {
        language_unset: inputs.language_unset,
        source: inputs.source,
        catalog: inputs.catalog,
        daemon: inputs.daemon,
        installing: inputs.installing,
        preparing: inputs.preparing,
        locales: inputs.locales,
    }
}

/// The one Translate row: the page it opens lists the languages, so the row
/// says how many the text can reach from where it is.
fn translate_row(inputs: &Inputs) -> Row {
    let (subtitle, open) = translate_subtitle(inputs);
    let installed = |entry: &&Entry| {
        inputs
            .daemon
            .is_some_and(|daemon| daemon.offers(&entry.model))
            && !inputs.removing.contains(&entry.id)
            && !inputs.installing.contains_key(TRANSLATE)
    };
    let removable = inputs
        .catalog
        .iter()
        .filter(|entry| entry.step.id == TRANSLATE)
        .find(installed)
        .map(|entry| Box::new(entry.clone()));
    Row {
        id: TRANSLATE.to_owned(),
        title: gettextrs::gettext("Translate"),
        description: String::new(),
        standing: if open {
            Standing::Ready
        } else {
            Standing::NotApplicable(subtitle.clone())
        },
        subtitle,
        on: inputs.rows.iter().any(|row| row.id == TRANSLATE && row.on),
        translate: None,
        script: None,
        place: steps::Place::default(),
        entry: None,
        example: None,
        details: Vec::new(),
        removable,
    }
}

/// What the Translate row says, and whether the language page has anything
/// to offer.
fn translate_subtitle(inputs: &Inputs) -> (String, bool) {
    let Source::One(from) = inputs.translate_from else {
        return (
            gettextrs::gettext("Needs a single dictation language"),
            false,
        );
    };
    let name = language_in_text(from);
    if inputs.languages.is_empty() {
        // TRANSLATORS: {language} is a language name, such as "فارسی".
        let frame = gettextrs::gettext("No model translates {language} yet");
        return (frame.replace("{language}", &name), false);
    }
    let count = u32::try_from(inputs.languages.len()).unwrap_or(u32::MAX);
    let frame = gettextrs::ngettext(
        // TRANSLATORS: {language} is the language the text is in, such as "English"; {count} how many languages it can be translated into.
        "From {language} into {count} language",
        "From {language} into {count} languages",
        count,
    );
    (
        frame
            .replace("{language}", &name)
            .replace("{count}", &count.to_string()),
        true,
    )
}

fn arrow(heard: &str, typed: &str) -> String {
    format!("\u{201c}{heard}\u{201d} \u{2192} \u{201c}{typed}\u{201d}")
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
fn details(entry: &Entry) -> Vec<(String, String)> {
    let mut details = vec![(gettextrs::gettext("Download"), size(entry.download_bytes))];
    if let Some(quality) = &entry.quality {
        details.push((gettextrs::gettext("Quality"), quality_note(&quality.source)));
    }
    if !entry.license.is_empty() {
        let mut text = entry.license.clone();
        if !entry.attribution.is_empty() {
            text = format!("{}\n{}", entry.attribution, text);
        }
        if !entry.source.is_empty() {
            text = format!("{text}\n{}", entry.source);
        }
        details.push((gettextrs::gettext("License"), text));
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

/// The step a catalog row adds: its step id, the target of a translation
/// model, the desktop's script for the Chinese script step.
pub fn step_of(row: &Row, locales: &[String]) -> ChainStep {
    let step = ChainStep::new(&row.id);
    match &row.entry {
        Some(entry) if entry.step.id == TRANSLATE => step.with("to", &entry.out_lang),
        _ if row.id == SCRIPT => step.with("mode", steps::default_script(locales).nick()),
        _ => step,
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
