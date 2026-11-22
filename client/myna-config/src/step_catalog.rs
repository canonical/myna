//! What the Add step page lists, GTK-free: every step the merged catalogs
//! offer, plus the ones the client carries, each with what it is, what it
//! costs and what can be done with it. Entries come from the catalog schema
//! only; nothing here knows a plugin by name.

use myna_core::processing::{param_key, Params};

use crate::plugin_catalog::{self, Entry};
use crate::processing_steps::{
    language_name, size, Offered, Row, Source, Standing, SCRIPT, TRANSLATE, TRANSLATE_TO,
};

const FIT: &str = "fit";

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
}

pub struct Inputs<'a> {
    pub catalog: &'a [Entry],
    pub enabled: &'a [String],
    pub params: &'a Params,
    pub source: &'a Source,
    pub daemon: Option<&'a Offered>,
    /// Installs running now, by item id, with snapd's percentage.
    pub installing: &'a std::collections::BTreeMap<String, Option<u8>>,
    /// Item ids installed whose models the daemon has not offered yet.
    pub preparing: &'a std::collections::BTreeSet<String>,
    pub locales: &'a [String],
    /// The Steps rows as the page shows them, for what cannot apply.
    pub rows: &'a [Row],
}

/// The items in the order the page lists them: what suits the user's
/// locales first, then stage order.
pub fn items(inputs: &Inputs) -> Vec<Item> {
    let mut found: Vec<(usize, usize, Item)> = Vec::new();
    for id in [FIT, SCRIPT] {
        let rank = if id == SCRIPT {
            plugin_catalog::relevance(&CHINESE.map(str::to_owned), "", inputs.locales)
        } else {
            inputs.locales.len()
        };
        found.push((rank, stage_of(id), built_in(id, inputs)));
    }
    for entry in inputs.catalog {
        let rank = plugin_catalog::relevance(&entry.locales, &entry.in_lang, inputs.locales);
        found.push((rank, stage_of(&entry.step.stage), from_entry(entry, inputs)));
    }
    found.sort_by(|a, b| (a.0, a.1, &a.2.id).cmp(&(b.0, b.1, &b.2.id)));
    found.into_iter().map(|(_, _, item)| item).collect()
}

fn stage_of(stage: &str) -> usize {
    match stage {
        FIT | "field-fit" => 0,
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
    let (title, description, example, license) = if id == SCRIPT {
        (
            gettextrs::gettext("Chinese script"),
            gettextrs::gettext(
                "Types Chinese in the characters you choose. Other text is left alone.",
            ),
            Some(("\u{8f6f}\u{4ef6}".to_owned(), "\u{8edf}\u{9ad4}".to_owned())),
            // TRANSLATORS: credits the libraries and word lists behind Chinese script conversion.
            gettextrs::gettext("opencc-jieba-rs (MIT); OpenCC conversion lists (Apache-2.0)"),
        )
    } else {
        (
            gettextrs::gettext("Fitted to the field"),
            gettextrs::gettext(
                "No capital or full stop in an address, and no capital where you carry on a sentence.",
            ),
            None,
            String::new(),
        )
    };
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
        && matches!(inputs.source, Source::One(from) if *from != entry.out_lang))
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
    }
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
    if added(entry, inputs) {
        return Offer::Added;
    }
    if let Some(percent) = inputs.installing.get(&entry.id) {
        return Offer::Installing(*percent);
    }
    let Some(daemon) = inputs.daemon else {
        return Offer::Unknown;
    };
    if !daemon.offers(&entry.model) {
        return if inputs.preparing.contains(&entry.id) {
            Offer::Preparing
        } else {
            Offer::Install(Box::new(entry.clone()))
        };
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

/// Why a translation model cannot act on the dictation language: neither it
/// nor a pivot through English reaches its target.
fn translate_mismatch(entry: &Entry, inputs: &Inputs) -> Option<String> {
    let Source::One(from) = inputs.source else {
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
    use std::collections::{BTreeMap, BTreeSet};

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
        daemon: Option<Offered>,
        installing: BTreeMap<String, Option<u8>>,
        preparing: BTreeSet<String>,
        locales: Vec<String>,
    }

    fn case() -> Case {
        Case {
            enabled: Vec::new(),
            params: Params::new(),
            language: "en",
            daemon: Offered::parse(BOTH),
            installing: BTreeMap::new(),
            preparing: BTreeSet::new(),
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
        items(&Inputs {
            catalog: &catalog,
            enabled: &case.enabled,
            params: &case.params,
            source: &source,
            daemon: case.daemon.as_ref(),
            installing: &case.installing,
            preparing: &case.preparing,
            locales: &case.locales,
            rows: &step_rows,
        })
    }

    fn find<'a>(items: &'a [Item], id: &str) -> &'a Item {
        items.iter().find(|i| i.id == id).unwrap()
    }

    #[test]
    fn built_ins_and_every_entry_are_listed_with_their_facts() {
        let all = listed(&case());
        let ids: Vec<_> = all.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids.len(), 5, "{ids:?}");
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
        assert!(find(&all, "fit").example.is_none());
        assert_eq!(find(&all, "fit").details.len(), 1);
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
            daemon: None,
            installing: &BTreeMap::new(),
            preparing: &BTreeSet::new(),
            locales: &[],
            rows: &[],
        });
        assert_eq!(all.len(), 2);
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
        assert_eq!(ids, ["tidy", "en-fr", "fit", "fr-en", "script"], "{ids:?}");
    }

    #[test]
    fn an_installed_item_can_be_added_and_a_missing_one_installed() {
        let all = listed(&case());
        assert_eq!(find(&all, "tidy").offer, Offer::Add);
        assert_eq!(find(&all, "en-fr").offer, Offer::Add);
        assert_eq!(find(&all, "fit").offer, Offer::Add);
        let Offer::Install(entry) = &find(&all, "fr-en").offer else {
            panic!("{:?}", find(&all, "fr-en"));
        };
        assert_eq!(entry.id, "fr-en");
    }

    #[test]
    fn a_step_already_on_is_added() {
        let mut on = case();
        on.enabled = vec!["tidy".into(), "fit".into(), "translate".into()];
        on.params
            .insert(param_key(TRANSLATE, TRANSLATE_TO), "fr".into());
        let all = listed(&on);
        assert_eq!(find(&all, "tidy").offer, Offer::Added);
        assert_eq!(find(&all, "fit").offer, Offer::Added);
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
    fn with_no_daemon_nothing_is_known_about_the_models() {
        let mut none = case();
        none.daemon = None;
        let all = listed(&none);
        assert_eq!(find(&all, "tidy").offer, Offer::Unknown);
        assert_eq!(find(&all, "fit").offer, Offer::Add);
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
            daemon: daemon.as_ref(),
            installing: &BTreeMap::new(),
            preparing: &BTreeSet::new(),
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
            &["fit".to_owned()],
            &Params::new(),
            find(&all, "en-fr"),
            &[],
        );
        assert_eq!(ids, ["fit", "translate"]);
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
            daemon: None,
            installing: &BTreeMap::new(),
            preparing: &BTreeSet::new(),
            locales: &["en".to_owned()],
            rows: &[],
        });
        let ids: Vec<_> = all.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            ["m-clean", "z-translate", "a-late", "fit", "script"],
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
