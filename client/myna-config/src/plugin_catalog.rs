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

/// Merge by entry `id`: an installed snap's entry wins over a snapshot's, the
/// higher `entry_revision` between equal sources; hidden entries drop out.
pub fn load(snapshot_dir: &Path, snap_root: &Path) -> Catalog {
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
        .filter(|entry| entry.curated && entry.contract <= SUPPORTED_CONTRACT)
        .collect();
    Catalog { entries, skipped }
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
            "id": id, "snap": "s", "component": id, "model": "m",
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
        let catalog = load(&snapshot, &root);
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
    fn unknown_label_key_has_no_words() {
        assert_eq!(label("step.nope.name"), None);
    }
}
