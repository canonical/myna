//! The plugin catalogs Settings lists steps from: the deb's snapshot of each
//! text snap's catalog, under the catalog each installed snap ships itself.
//! Format: `.kb/text-plugin-contract.md`, "One source per plugin".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The newest `contract` this Settings understands; later entries are hidden.
pub const SUPPORTED_CONTRACT: u32 = 2;

/// The snaps whose catalogs are read.
pub const SNAPS: [&str; 2] = ["myna-text", "myna-translation"];

pub const SNAPSHOT_DIR: &str = "/usr/share/myna-config/catalog";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Step {
    pub id: String,
    pub stage: String,
    pub label_key: String,
    pub description_key: String,
    pub example_key: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Quality {
    pub value: f64,
    #[serde(default)]
    pub source: String,
}

impl Eq for Quality {}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Entry {
    pub id: String,
    pub snap: String,
    pub component: String,
    pub model: String,
    #[serde(default = "one")]
    pub contract: u32,
    #[serde(default)]
    pub entry_revision: u64,
    pub task: String,
    pub step: Step,
    #[serde(default)]
    pub in_lang: String,
    #[serde(default)]
    pub out_lang: String,
    /// Every language a model reads, when it is more than `in_lang`.
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub download_bytes: u64,
    #[serde(default)]
    pub installed_bytes: u64,
    #[serde(default)]
    pub latency_class: String,
    #[serde(default)]
    pub locales: Vec<String>,
    #[serde(default = "yes")]
    pub curated: bool,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub attribution: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub quality: Option<Quality>,
}

fn one() -> u32 {
    1
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct File {
    plugins: Vec<serde_json::Value>,
}

/// Where an entry came from; an installed snap's entry outranks a snapshot's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Snapshot,
    Installed,
}

/// Entries read from one file, plus one line per file or entry skipped.
fn parse(path: &Path, text: &str, skipped: &mut Vec<String>) -> Vec<Entry> {
    let file: File = match serde_json::from_str(text) {
        Ok(file) => file,
        Err(err) => {
            skipped.push(format!("{}: {err}", path.display()));
            return Vec::new();
        }
    };
    file.plugins
        .into_iter()
        .filter_map(|value| match serde_json::from_value::<Entry>(value) {
            Ok(entry) => Some(entry),
            Err(err) => {
                skipped.push(format!("{}: entry skipped: {err}", path.display()));
                None
            }
        })
        .collect()
}

fn read(path: &Path, skipped: &mut Vec<String>) -> Vec<Entry> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(path, &text, skipped),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            skipped.push(format!("{}: {err}", path.display()));
            Vec::new()
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Catalog {
    pub entries: Vec<Entry>,
    /// One line per unreadable file or entry, naming the file.
    pub skipped: Vec<String>,
}

/// Which entries a catalog lists besides those of a known contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Listing {
    /// Only curated entries: what users see.
    Curated,
    /// Uncurated entries too, for development and E2E.
    All,
}

impl Listing {
    /// `All` when `MYNA_LIST_UNCURATED` is `1`.
    pub fn from_env(value: Option<&str>) -> Self {
        if value == Some("1") {
            Self::All
        } else {
            Self::Curated
        }
    }
}

/// [`load_listing`] of the curated entries.
pub fn load(snapshot_dir: &Path, snap_root: &Path) -> Catalog {
    load_listing(snapshot_dir, snap_root, Listing::Curated)
}

/// Merge by entry `id`: an installed snap's entry wins over a snapshot's, the
/// higher `entry_revision` between equal sources; hidden entries drop out.
pub fn load_listing(snapshot_dir: &Path, snap_root: &Path, listing: Listing) -> Catalog {
    let mut skipped = Vec::new();
    let mut best: BTreeMap<String, (Source, Entry)> = BTreeMap::new();
    for snap in SNAPS {
        let snapshot = snapshot_dir.join(format!("{snap}.json"));
        let installed: PathBuf = snap_root
            .join(snap)
            .join("current/share")
            .join(snap)
            .join("catalog.json");
        for (source, path) in [(Source::Snapshot, snapshot), (Source::Installed, installed)] {
            for entry in read(&path, &mut skipped) {
                let wins = best.get(&entry.id).is_none_or(|(held, kept)| {
                    (source, entry.entry_revision) > (*held, kept.entry_revision)
                });
                if wins {
                    best.insert(entry.id.clone(), (source, entry));
                }
            }
        }
    }
    let entries = best
        .into_values()
        .map(|(_, entry)| entry)
        .filter(|entry| {
            (entry.curated || listing == Listing::All) && entry.contract <= SUPPORTED_CONTRACT
        })
        .filter(|entry| {
            let listed = entry.listable();
            if !listed {
                skipped.push(format!(
                    "{}: entry skipped: not an allowed snap or component",
                    entry.id
                ));
            }
            listed
        })
        .collect();
    Catalog { entries, skipped }
}

impl Entry {
    /// Only the two text snaps' own components are ever installed, whatever
    /// a catalog file claims.
    fn listable(&self) -> bool {
        SNAPS.contains(&self.snap.as_str())
            && crate::adapters::snapd_client::is_valid_component_name(&self.component)
    }
}

/// What one snapd request changes: named components of one snap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentRequest {
    pub snap: String,
    pub components: Vec<String>,
    /// The model ids the components provide, for waiting on the daemon.
    pub models: Vec<String>,
    pub expected_bytes: u64,
}

/// The requests that install (or remove) `entries`: one per snap, each
/// component once, in a stable order.
pub fn requests(entries: &[&Entry]) -> Vec<ComponentRequest> {
    let mut by_snap: BTreeMap<&str, BTreeMap<&str, &Entry>> = BTreeMap::new();
    for entry in entries {
        by_snap
            .entry(&entry.snap)
            .or_default()
            .insert(&entry.component, entry);
    }
    by_snap
        .into_iter()
        .map(|(snap, components)| ComponentRequest {
            snap: snap.to_owned(),
            components: components.keys().map(|c| (*c).to_owned()).collect(),
            models: components.values().map(|e| e.model.clone()).collect(),
            expected_bytes: components.values().map(|e| e.download_bytes).sum(),
        })
        .collect()
}

/// Entries whose model the daemon does not offer yet.
pub fn not_offered<'a>(entries: &'a [Entry], offered: &[String]) -> Vec<&'a Entry> {
    entries
        .iter()
        .filter(|entry| !offered.contains(&entry.model))
        .collect()
}

/// How far down the user's locales a thing useful for `useful_for` (bare
/// language codes, else `fallback`) comes; `locales.len()` when not at all.
pub fn relevance(useful_for: &[String], fallback: &str, locales: &[String]) -> usize {
    locales
        .iter()
        .position(|locale| {
            let language = locale.split(['_', '-', '.']).next().unwrap_or_default();
            useful_for.iter().any(|l| l == language)
                || (useful_for.is_empty() && fallback == language)
        })
        .unwrap_or(locales.len())
}

/// Stable sort putting entries relevant to the user's locales first, in the
/// order the locales are given, then the rest by id.
pub fn locale_order(entries: &mut [&Entry], locales: &[String]) {
    let rank = |entry: &Entry| relevance(&entry.locales, &entry.in_lang, locales);
    entries.sort_by(|a, b| (rank(a), &a.id).cmp(&(rank(b), &b.id)));
}

/// The words a derived label key stands for, translated; `None` for a key
/// this Settings has no words for.
pub fn label(key: &str) -> Option<String> {
    use gettextrs::gettext;
    Some(match key {
        "step.tidy.name" => gettext("Tidy up speech"),
        "step.tidy.description" => {
            gettext("Removes filler words and false starts, and fixes case and punctuation.")
        }
        "step.tidy.example" => gettext("um so I, I think we should go"),
        "step.translate.name" => gettext("Translate"),
        "step.translate.description" => gettext("Types your dictation in another language."),
        "step.translate.example" => gettext("I think we should go"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn entry(id: &str, revision: u64) -> serde_json::Value {
        serde_json::json!({
            "id": id, "snap": "myna-text", "component": format!("model-{id}"), "model": format!("m-{id}"),
            "contract": 2, "entry_revision": revision, "task": "text.cleanup",
            "step": {
                "id": "tidy", "stage": "clean-up",
                "label_key": "step.tidy.name",
                "description_key": "step.tidy.description",
                "example_key": "step.tidy.example",
            },
        })
    }

    fn with(
        mut entry: serde_json::Value,
        key: &str,
        value: serde_json::Value,
    ) -> serde_json::Value {
        entry[key] = value;
        entry
    }

    fn write(dir: &Path, rel: &str, entries: &[serde_json::Value]) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            serde_json::json!({"catalog": 1, "plugins": entries}).to_string(),
        )
        .unwrap();
    }

    fn rig() -> (tempfile_dir::Dir, PathBuf, PathBuf) {
        let dir = tempfile_dir::Dir::new();
        let snapshot = dir.path().join("snapshot");
        let root = dir.path().join("snap");
        (dir, snapshot, root)
    }

    mod tempfile_dir {
        use std::path::{Path, PathBuf};
        pub struct Dir(PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "myna-catalog-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                ));
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn installed(root: &Path, snap: &str, entries: &[serde_json::Value]) {
        write(
            root,
            &format!("{snap}/current/share/{snap}/catalog.json"),
            entries,
        );
    }

    #[test]
    fn missing_directories_give_an_empty_quiet_catalog() {
        let (_dir, snapshot, root) = rig();
        assert_eq!(load(&snapshot, &root), Catalog::default());
    }

    #[test]
    fn snapshot_alone_is_listed() {
        let (_dir, snapshot, root) = rig();
        write(&snapshot, "myna-text.json", &[entry("a", 1)]);
        let catalog = load(&snapshot, &root);
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].id, "a");
    }

    #[test]
    fn installed_snap_wins_even_with_a_lower_revision() {
        let (_dir, snapshot, root) = rig();
        write(&snapshot, "myna-text.json", &[entry("a", 5)]);
        installed(&root, "myna-text", &[entry("a", 2)]);
        let catalog = load(&snapshot, &root);
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].entry_revision, 2);
    }

    #[test]
    fn higher_revision_wins_between_equal_sources() {
        let (_dir, snapshot, root) = rig();
        write(&snapshot, "myna-text.json", &[entry("a", 1)]);
        write(&snapshot, "myna-translation.json", &[entry("a", 3)]);
        let catalog = load(&snapshot, &root);
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].entry_revision, 3);
    }

    #[test]
    fn bad_json_is_skipped_and_named() {
        let (_dir, snapshot, root) = rig();
        fs::create_dir_all(&snapshot).unwrap();
        fs::write(snapshot.join("myna-text.json"), "{nope").unwrap();
        write(&snapshot, "myna-translation.json", &[entry("b", 1)]);
        let catalog = load(&snapshot, &root);
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.skipped.len(), 1);
        assert!(catalog.skipped[0].contains("myna-text.json"));
    }

    #[test]
    fn unreadable_file_is_skipped_and_named() {
        let (_dir, snapshot, root) = rig();
        fs::create_dir_all(snapshot.join("myna-text.json")).unwrap();
        let catalog = load(&snapshot, &root);
        assert!(catalog.entries.is_empty());
        assert!(catalog.skipped[0].contains("myna-text.json"));
    }

    #[test]
    fn one_malformed_entry_does_not_hide_its_neighbours() {
        let (_dir, snapshot, root) = rig();
        write(
            &snapshot,
            "myna-text.json",
            &[serde_json::json!({"id": "broken"}), entry("ok", 1)],
        );
        let catalog = load(&snapshot, &root);
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].id, "ok");
        assert!(catalog.skipped[0].contains("myna-text.json"));
    }

    #[test]
    fn unknown_contract_and_uncurated_entries_are_hidden() {
        let (_dir, snapshot, root) = rig();
        write(
            &snapshot,
            "myna-text.json",
            &[
                with(entry("new", 1), "contract", 3.into()),
                with(entry("hidden", 1), "curated", false.into()),
                with(entry("shown", 1), "curated", true.into()),
            ],
        );
        let ids: Vec<_> = load(&snapshot, &root)
            .entries
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(ids, ["shown"]);
    }

    #[test]
    fn listing_all_shows_uncurated_entries_but_not_unknown_contracts() {
        let (_dir, snapshot, root) = rig();
        write(
            &snapshot,
            "myna-text.json",
            &[
                with(entry("new", 1), "contract", 3.into()),
                with(entry("uncurated", 1), "curated", false.into()),
                with(entry("curated", 1), "curated", true.into()),
            ],
        );
        let ids: Vec<_> = load_listing(&snapshot, &root, Listing::All)
            .entries
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(ids, ["curated", "uncurated"]);
    }

    #[test]
    fn only_a_one_lists_uncurated_entries() {
        assert_eq!(Listing::from_env(Some("1")), Listing::All);
        for value in [None, Some(""), Some("0"), Some("yes")] {
            assert_eq!(Listing::from_env(value), Listing::Curated, "{value:?}");
        }
    }

    #[test]
    fn hidden_installed_entry_still_shadows_the_snapshot() {
        let (_dir, snapshot, root) = rig();
        write(&snapshot, "myna-text.json", &[entry("a", 1)]);
        installed(
            &root,
            "myna-text",
            &[with(entry("a", 2), "curated", false.into())],
        );
        assert!(load(&snapshot, &root).entries.is_empty());
    }

    #[test]
    fn the_shipped_catalogs_load() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if !repo.join("text-snap/catalog.json").exists() {
            return;
        }
        let (_dir, snapshot, root) = rig();
        fs::create_dir_all(&snapshot).unwrap();
        for snap in SNAPS {
            let dir = snap.trim_start_matches("myna-");
            fs::copy(
                repo.join(format!("{dir}-snap/catalog.json")),
                snapshot.join(format!("{snap}.json")),
            )
            .unwrap();
        }
        let catalog = load_listing(&snapshot, &root, Listing::All);
        assert!(catalog.skipped.is_empty(), "{:?}", catalog.skipped);
        let ids: Vec<_> = catalog.entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"model-disfluency-en"));
        assert!(ids.contains(&"model-translate-en-fr"));
        for entry in &catalog.entries {
            for key in [
                &entry.step.label_key,
                &entry.step.description_key,
                &entry.step.example_key,
            ] {
                assert!(label(key).is_some(), "no words for {key}");
            }
        }
    }

    #[test]
    fn a_tie_keeps_the_first_entry_read() {
        let (_dir, snapshot, root) = rig();
        write(&snapshot, "myna-text.json", &[entry("a", 1)]);
        write(
            &snapshot,
            "myna-translation.json",
            &[with(entry("a", 1), "model", "other".into())],
        );
        assert_eq!(load(&snapshot, &root).entries[0].model, "m-a");
    }

    #[test]
    fn a_missing_contract_means_the_first() {
        let (_dir, snapshot, root) = rig();
        let mut old = entry("a", 1);
        old.as_object_mut().unwrap().remove("contract");
        write(&snapshot, "myna-text.json", &[old]);
        assert_eq!(load(&snapshot, &root).entries[0].contract, 1);
    }

    #[test]
    fn every_step_label_has_words() {
        for step in ["tidy", "translate"] {
            for part in ["name", "description", "example"] {
                let words = label(&format!("step.{step}.{part}")).unwrap();
                assert!(!words.is_empty());
            }
        }
        assert_ne!(label("step.tidy.name"), label("step.tidy.description"));
    }

    #[test]
    fn unknown_label_key_has_no_words() {
        assert_eq!(label("step.nope.name"), None);
    }

    fn pair(from: &str, to: &str, snap: &str, bytes: u64) -> serde_json::Value {
        let mut e = entry(&format!("translate-{from}-{to}"), 1);
        e["snap"] = snap.into();
        e["in_lang"] = from.into();
        e["out_lang"] = to.into();
        e["download_bytes"] = bytes.into();
        e["step"]["id"] = "translate".into();
        e["locales"] = serde_json::json!([from]);
        e
    }

    fn loaded(entries: &[serde_json::Value]) -> Vec<Entry> {
        let (_dir, snapshot, root) = rig();
        write(&snapshot, "myna-translation.json", entries);
        load(&snapshot, &root).entries
    }

    #[test]
    fn an_entry_outside_the_two_text_snaps_or_with_a_bad_component_is_dropped() {
        let (_dir, snapshot, root) = rig();
        write(
            &snapshot,
            "myna-text.json",
            &[
                with(entry("a", 1), "snap", "gemma4".into()),
                with(entry("b", 1), "component", "../etc".into()),
                with(entry("c", 1), "component", "model-".into()),
                with(entry("d", 1), "component", "model-X".into()),
                entry("ok", 1),
            ],
        );
        let catalog = load(&snapshot, &root);
        let ids: Vec<_> = catalog.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["ok"]);
        assert_eq!(catalog.skipped.len(), 4);
    }

    #[test]
    fn requests_group_components_by_snap_once_each_with_their_sizes() {
        let entries = loaded(&[
            pair("de", "en", "myna-translation", 10),
            pair("en", "fr", "myna-translation", 5),
        ]);
        let mut doubled = entries.iter().collect::<Vec<_>>();
        doubled.push(&entries[0]);
        let mut other = entry("tidy", 1);
        other["download_bytes"] = 7.into();
        let text = loaded_text(&[other]);
        doubled.push(&text[0]);
        let requests = requests(&doubled);
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].snap, "myna-text");
        assert_eq!(requests[0].components, ["model-tidy"]);
        assert_eq!(requests[0].models, ["m-tidy"]);
        assert_eq!(requests[0].expected_bytes, 7);
        assert_eq!(requests[1].snap, "myna-translation");
        assert_eq!(
            requests[1].components,
            ["model-translate-de-en", "model-translate-en-fr"]
        );
        assert_eq!(requests[1].expected_bytes, 15);
    }

    fn loaded_text(entries: &[serde_json::Value]) -> Vec<Entry> {
        let (_dir, snapshot, root) = rig();
        write(&snapshot, "myna-text.json", entries);
        load(&snapshot, &root).entries
    }

    #[test]
    fn what_the_daemon_offers_is_subtracted() {
        let entries = loaded(&[
            pair("en", "fr", "myna-translation", 1),
            pair("en", "de", "myna-translation", 1),
        ]);
        let left = not_offered(&entries, &["m-translate-en-fr".to_owned()]);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].out_lang, "de");
    }

    #[test]
    fn entries_for_the_users_locales_come_first() {
        let entries = loaded(&[
            pair("de", "en", "myna-translation", 1),
            pair("en", "fr", "myna-translation", 1),
            pair("fr", "en", "myna-translation", 1),
        ]);
        let mut sorted: Vec<&Entry> = entries.iter().collect();
        locale_order(&mut sorted, &["fr_FR.UTF-8".to_owned(), "de".to_owned()]);
        let ids: Vec<_> = sorted.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            ["translate-fr-en", "translate-de-en", "translate-en-fr"]
        );
        locale_order(&mut sorted, &[]);
        assert_eq!(sorted[0].id, "translate-de-en");
    }

    #[test]
    fn locales_win_over_the_input_language_and_a_mismatch_ranks_last() {
        let mut declared = pair("de", "en", "myna-translation", 1);
        declared["locales"] = serde_json::json!(["fr"]);
        let mut right = pair("zz", "en", "myna-translation", 1);
        right["locales"] = serde_json::json!(["de"]);
        let mut bare_match = pair("de", "fr", "myna-translation", 1);
        bare_match["id"] = "translate-zzz".into();
        bare_match["locales"] = serde_json::json!([]);
        let mut bare_other = pair("en", "fr", "myna-translation", 1);
        bare_other["locales"] = serde_json::json!([]);
        let entries = loaded(&[declared, right, bare_match, bare_other]);
        let mut sorted: Vec<&Entry> = entries.iter().collect();
        locale_order(&mut sorted, &["de".to_owned()]);
        let ids: Vec<_> = sorted.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "translate-zz-en",
                "translate-zzz",
                "translate-de-en",
                "translate-en-fr"
            ]
        );
    }

    #[test]
    fn an_entry_without_locales_follows_its_input_language() {
        let mut e = pair("de", "en", "myna-translation", 1);
        e["locales"] = serde_json::json!([]);
        let entries = loaded(&[e, pair("en", "fr", "myna-translation", 1)]);
        let mut sorted: Vec<&Entry> = entries.iter().collect();
        locale_order(&mut sorted, &["de-AT".to_owned()]);
        assert_eq!(sorted[0].id, "translate-de-en");
    }

    #[test]
    fn an_entry_carries_its_credit_and_measured_quality() {
        let (dir, snapshot, root) = rig();
        let full = with(
            with(
                with(entry("a", 1), "attribution", "Mozilla".into()),
                "source",
                "https://x".into(),
            ),
            "quality",
            serde_json::json!({"value": 0.7, "source": "chrF 0.700"}),
        );
        write(dir.path(), "snap/a.json", &[full]);
        let _ = (&snapshot, &root);
        let text = fs::read_to_string(dir.path().join("snap/a.json")).unwrap();
        let mut skipped = Vec::new();
        let got = parse(Path::new("a.json"), &text, &mut skipped);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(got[0].attribution, "Mozilla");
        assert_eq!(got[0].source, "https://x");
        assert_eq!(got[0].quality.as_ref().unwrap().source, "chrF 0.700");
    }
}
