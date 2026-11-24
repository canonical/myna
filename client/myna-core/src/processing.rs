//! The processing steps as stored: an ordered list of enabled step ids and
//! their parameters, the presets the Settings page offers over them, and
//! the one-shot migration from the keys that came before.
//!
//! Steps run in a fixed stage order that the user does not set; the daemon
//! compiles the list (`myna-desktop`'s `process::steps`), this module only
//! names, orders and migrates it.

use std::collections::BTreeMap;
use std::path::Path;

use gio::glib;

use crate::chains::{
    chains_from_variant, chains_variant, Chain, ChainStep, Processing, KEY_ACTIVE_CHAIN,
    KEY_CHAINS, KEY_FIT_TO_FIELD, MIGRATED_ID,
};
use crate::settings::store_path;

/// The flat step list and its parameters, before chains; never released.
const KEY_PROCESSING_STEPS: &str = "processing-steps";
const KEY_PROCESSING_PARAMS: &str = "processing-params";

/// Step parameters as stored: `<step>.<param>` to its value.
pub type Params = BTreeMap<String, String>;

/// The steps this build knows, in stage order.
pub const STEP_ORDER: [&str; 13] = [
    "fit",
    "commands",
    "tidy",
    "emoji",
    "snippets",
    "casing",
    "addresses",
    "numbers",
    "profanity",
    "translate",
    "original",
    "typography",
    "script",
];

/// The parameter key of `step`'s `param`: `translate.to`.
pub fn param_key(step: &str, param: &str) -> String {
    format!("{step}.{param}")
}

/// A named set of steps the Settings page offers as one choice. Not stored:
/// a choice writes its steps and parameters (`processing-steps`,
/// `processing-params`), and the stored list reads back as the nearest preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessingChain(&'static str);

impl ProcessingChain {
    /// Nothing: typed as heard.
    pub const EXACT: Self = Self("exact");
    /// Hesitations, false starts and repeats dropped.
    pub const TIDY: Self = Self("tidy");
    /// Tidied, then translated from English to French.
    pub const TIDY_FRENCH: Self = Self("tidy-french");
    /// Typed as heard, fitted to the field: no capital or full stop in an
    /// address, no capital that continues a sentence.
    pub const KEYWORD: Self = Self("keyword");

    /// Every preset, in schema order.
    pub const ALL: [Self; 4] = [Self::EXACT, Self::TIDY, Self::TIDY_FRENCH, Self::KEYWORD];

    /// The schema enum nick.
    pub fn nick(self) -> &'static str {
        self.0
    }

    /// The preset a nick names; `None` for one this build does not know.
    pub fn from_nick(nick: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|chain| chain.nick() == nick)
    }
}

impl Default for ProcessingChain {
    fn default() -> Self {
        Self::EXACT
    }
}

/// How Chinese text is written (`script.mode`). A speech model picks a
/// script as it likes; anything but `AsRecognised` converts what it wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChineseScript {
    /// Typed as the speech model wrote it.
    #[default]
    AsRecognised,
    Simplified,
    Traditional,
    /// Traditional, in the characters and words used in Taiwan.
    TraditionalTaiwan,
    /// Traditional, in the characters used in Hong Kong.
    TraditionalHongKong,
}

impl ChineseScript {
    pub const ALL: [Self; 5] = [
        Self::AsRecognised,
        Self::Simplified,
        Self::Traditional,
        Self::TraditionalTaiwan,
        Self::TraditionalHongKong,
    ];

    /// The schema enum nick.
    pub fn nick(self) -> &'static str {
        match self {
            Self::AsRecognised => "as-recognised",
            Self::Simplified => "simplified",
            Self::Traditional => "traditional",
            Self::TraditionalTaiwan => "traditional-taiwan",
            Self::TraditionalHongKong => "traditional-hongkong",
        }
    }

    /// The script a nick names; `None` for one this build does not know.
    pub fn from_nick(nick: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|script| script.nick() == nick)
    }
}

impl ProcessingChain {
    /// The steps a preset enables, in stage order.
    pub fn steps(self) -> Vec<String> {
        let ids: &[&str] = match self {
            Self::TIDY => &["tidy"],
            Self::TIDY_FRENCH => &["tidy", "translate"],
            Self::KEYWORD => &["fit"],
            _ => &[],
        };
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    /// The parameters a preset sets.
    pub fn params(self) -> Params {
        match self {
            Self::TIDY_FRENCH => [(param_key("translate", "to"), "fr".to_owned())].into(),
            _ => Params::new(),
        }
    }
}

/// Stage order, one of each; ids this build does not know keep their order
/// after the known ones, so a newer build's step survives a round trip.
pub fn in_stage_order(ids: &[String]) -> Vec<String> {
    let rank = |id: &String| STEP_ORDER.iter().position(|known| known == id);
    let mut ordered: Vec<String> = Vec::new();
    for id in ids {
        if !ordered.contains(id) {
            ordered.push(id.clone());
        }
    }
    ordered.sort_by_key(|id| rank(id).unwrap_or(STEP_ORDER.len()));
    ordered
}

/// The preset a stored list reads as: the one that enables exactly the
/// preset steps among `ids` (the `script` step is its own setting), or, when
/// none does, the one that differs by the fewest steps, preferring the one
/// that enables more. Reading never changes the list.
pub fn chain_of(ids: &[String], params: &Params) -> ProcessingChain {
    let has = |id: &str| ids.iter().any(|have| have == id);
    let translating = has("translate");
    let french = params
        .get(&param_key("translate", "to"))
        .is_none_or(|to| to == "fr");
    let wanted: Vec<&str> = ["fit", "tidy", "translate"]
        .into_iter()
        .filter(|id| has(id))
        .collect();
    let distance = |chain: ProcessingChain| {
        let steps = chain.steps();
        let missing = wanted
            .iter()
            .filter(|id| !steps.iter().any(|s| s == *id))
            .count();
        let extra = steps
            .iter()
            .filter(|id| !wanted.contains(&id.as_str()))
            .count();
        let foreign =
            2 * usize::from(chain == ProcessingChain::TIDY_FRENCH && translating && !french);
        (missing + extra + foreign, std::cmp::Reverse(steps.len()))
    };
    [
        ProcessingChain::TIDY_FRENCH,
        ProcessingChain::TIDY,
        ProcessingChain::KEYWORD,
        ProcessingChain::EXACT,
    ]
    .into_iter()
    .min_by_key(|chain| distance(*chain))
    .unwrap_or_default()
}

/// The list and parameters after choosing `chain`: its steps replace the
/// preset steps, and the Chinese script step and any step this build does
/// not know stay as they were.
pub fn with_chain(
    ids: &[String],
    params: &Params,
    chain: ProcessingChain,
) -> (Vec<String>, Params) {
    let mut next: Vec<String> = ids
        .iter()
        .filter(|id| !["fit", "tidy", "translate"].contains(&id.as_str()))
        .cloned()
        .collect();
    next.extend(chain.steps());
    let mut params = params.clone();
    params.extend(chain.params());
    (in_stage_order(&next), params)
}

/// How Chinese is written under a stored list: as recognised unless the
/// `script` step is enabled with a known `script.mode`.
pub fn script_of(ids: &[String], params: &Params) -> ChineseScript {
    if !ids.iter().any(|id| id == "script") {
        return ChineseScript::AsRecognised;
    }
    params
        .get(&param_key("script", "mode"))
        .and_then(|nick| ChineseScript::from_nick(nick))
        .unwrap_or_default()
}

/// The list and parameters after choosing how Chinese is written: the
/// `script` step is enabled for any script but `AsRecognised`.
pub fn with_script(
    ids: &[String],
    params: &Params,
    script: ChineseScript,
) -> (Vec<String>, Params) {
    let mut next: Vec<String> = ids.iter().filter(|id| *id != "script").cloned().collect();
    let mut params = params.clone();
    let key = param_key("script", "mode");
    if script == ChineseScript::AsRecognised {
        params.remove(&key);
    } else {
        next.push("script".into());
        params.insert(key, script.nick().to_owned());
    }
    (in_stage_order(&next), params)
}

/// The keys that stood in `com.canonical.Myna.Dictation` before steps, none
/// of them in a release.
/// [`migrate_legacy`] on the settings store every process shares; the daemon
/// and Myna Settings call it before they read a setting.
pub fn migrate_legacy_store() -> bool {
    store_path().is_some_and(|path| {
        let legacy = migrate_legacy(&path);
        migrate_steps(&path) || legacy
    })
}

const LEGACY_KEYS: [&str; 6] = [
    "processing-chain",
    "processing",
    "processing-tidy",
    "processing-translate",
    "processing-sentences",
    "chinese-script",
];

/// What the legacy keys said, as plain text.
#[derive(Default)]
struct Legacy {
    chain: Option<String>,
    processing: Option<bool>,
    tidy: Option<bool>,
    translate: Option<bool>,
    script: Option<String>,
}

impl Legacy {
    /// The steps and parameters the old keys amount to. `processing-chain`
    /// is the newest of them and wins; before it, `processing` switched
    /// everything off and `processing-tidy` and `processing-translate`
    /// chose the steps. `processing-sentences` has no successor: sentence
    /// streaming follows from what the steps need.
    fn steps(&self) -> (Vec<String>, Params) {
        let (mut ids, mut params) = match self.chain.as_deref().and_then(ProcessingChain::from_nick)
        {
            Some(chain) => (chain.steps(), chain.params()),
            None if self.chain.is_some() => (Vec::new(), Params::new()),
            None if self.processing == Some(false) => (Vec::new(), Params::new()),
            None => {
                let mut ids = Vec::new();
                let mut params = Params::new();
                if self.tidy == Some(true) {
                    ids.push("tidy".to_owned());
                }
                if self.translate == Some(true) {
                    ids.push("translate".to_owned());
                    params.insert(param_key("translate", "to"), "fr".into());
                }
                (ids, params)
            }
        };
        if let Some(script) = self.script.as_deref().and_then(ChineseScript::from_nick) {
            (ids, params) = with_script(&ids, &params, script);
        }
        (ids, params)
    }
}

/// The text of a keyfile value that is a GVariant string: `'tidy'`.
fn unquote(raw: &str) -> String {
    let raw = raw.trim();
    raw.strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .unwrap_or(raw)
        .to_owned()
}

/// The GVariant text of a string list: `['tidy', 'translate']`.
fn list_text(ids: &[String]) -> String {
    let items: Vec<String> = ids.iter().map(|id| format!("'{id}'")).collect();
    format!("[{}]", items.join(", "))
}

/// The GVariant text of a parameter map: `{'translate.to': 'fr'}`.
fn map_text(params: &Params) -> String {
    let items: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("'{k}': '{v}'"))
        .collect();
    format!("{{{}}}", items.join(", "))
}

/// Move a settings file written before steps to `processing-steps` and
/// `processing-params`, once: it reads the legacy keys from the keyfile
/// store, writes the steps they amount to unless the file already holds an
/// explicit value, and removes every legacy key it read. A file with no
/// legacy key is not touched. Returns whether the file changed.
///
/// None of the legacy keys is in a release tag (v0.3.3's schema has none),
/// so only development keyfiles carry them. Delete this function, its
/// callers and `LEGACY_KEYS` in the first release after it ships.
pub fn migrate_legacy(path: &Path) -> bool {
    let file = glib::KeyFile::new();
    if file.load_from_file(path, glib::KeyFileFlags::NONE).is_err() {
        return false;
    }
    let group = "com/canonical/myna/dictation";
    let raw = |key: &str| file.value(group, key).ok().map(|v| v.to_string());
    let present: Vec<&str> = LEGACY_KEYS
        .into_iter()
        .filter(|key| raw(key).is_some())
        .collect();
    if present.is_empty() {
        return false;
    }
    let flag = |key: &str| raw(key).map(|v| v.trim() == "true");
    let legacy = Legacy {
        chain: raw("processing-chain").map(|v| unquote(&v)),
        processing: flag("processing"),
        tidy: flag("processing-tidy"),
        translate: flag("processing-translate"),
        script: raw("chinese-script").map(|v| unquote(&v)),
    };
    let (ids, params) = legacy.steps();
    if raw(KEY_PROCESSING_STEPS).is_none() && !ids.is_empty() {
        file.set_value(group, KEY_PROCESSING_STEPS, &list_text(&ids));
    }
    if raw(KEY_PROCESSING_PARAMS).is_none() && !params.is_empty() {
        file.set_value(group, KEY_PROCESSING_PARAMS, &map_text(&params));
    }
    for key in present {
        let _ = file.remove_key(group, key);
    }
    file.save_to_file(path).is_ok()
}

/// Move a flat `processing-steps` list to a chain, once: its steps but
/// `fit`, in stage order with their parameters, become the chain "My steps",
/// made active; whether `fit` was among them becomes `fit-to-field`. Values
/// already set in the new keys win. Both old keys are removed. Returns
/// whether the file changed.
///
/// Like [`migrate_legacy`], only development keyfiles carry the old keys
/// (no tag has them). Delete this in the first release after it ships.
pub fn migrate_steps(path: &Path) -> bool {
    let file = glib::KeyFile::new();
    if file.load_from_file(path, glib::KeyFileFlags::NONE).is_err() {
        return false;
    }
    let group = "com/canonical/myna/dictation";
    let raw = |key: &str| file.value(group, key).ok().map(|v| v.to_string());
    let parse = |key: &str, ty: &glib::VariantTy| {
        raw(key).and_then(|text| glib::Variant::parse(Some(ty), &text).ok())
    };
    let Some(steps) = raw(KEY_PROCESSING_STEPS) else {
        if raw(KEY_PROCESSING_PARAMS).is_none() {
            return false;
        }
        let _ = file.remove_key(group, KEY_PROCESSING_PARAMS);
        return file.save_to_file(path).is_ok();
    };
    let ids: Vec<String> = glib::Variant::parse(Some(glib::VariantTy::STRING_ARRAY), &steps)
        .ok()
        .and_then(|value| value.get())
        .unwrap_or_default();
    let params: Params = parse(
        KEY_PROCESSING_PARAMS,
        glib::VariantTy::new("a{ss}").unwrap(),
    )
    .and_then(|value| value.get())
    .unwrap_or_default();
    if raw(KEY_FIT_TO_FIELD).is_none() {
        let fit = ids.iter().any(|id| id == "fit");
        file.set_value(group, KEY_FIT_TO_FIELD, if fit { "true" } else { "false" });
    }
    let kept: Vec<String> = ids
        .iter()
        .filter(|id| !id.is_empty() && *id != "fit")
        .cloned()
        .collect();
    let kept = in_stage_order(&kept);
    if !kept.is_empty() {
        let mut chains = parse(KEY_CHAINS, chains_variant(&[]).type_())
            .and_then(|value| chains_from_variant(&value))
            .unwrap_or_else(|| Processing::default().chains);
        let steps = kept
            .iter()
            .map(|id| {
                let prefix = format!("{id}.");
                params
                    .iter()
                    .filter_map(|(key, value)| Some((key.strip_prefix(&prefix)?, value)))
                    .fold(ChainStep::new(id), |step, (name, value)| {
                        step.with(name, value)
                    })
            })
            .collect();
        let id = (1..)
            .map(|n| match n {
                1 => MIGRATED_ID.to_owned(),
                n => format!("{MIGRATED_ID}-{n}"),
            })
            .find(|id| chains.iter().all(|chain| &chain.id != id))
            .expect("an unbounded range has a free id");
        chains.push(Chain {
            id: id.clone(),
            name: "My steps".into(),
            steps,
        });
        file.set_value(group, KEY_CHAINS, &chains_variant(&chains).print(false));
        if raw(KEY_ACTIVE_CHAIN).is_none() {
            file.set_value(group, KEY_ACTIVE_CHAIN, &format!("'{id}'"));
        }
    }
    let _ = file.remove_key(group, KEY_PROCESSING_STEPS);
    let _ = file.remove_key(group, KEY_PROCESSING_PARAMS);
    file.save_to_file(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn params(list: &[(&str, &str)]) -> Params {
        list.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    const GROUP: &str = "[com/canonical/myna/dictation]\n";

    /// Migrate `before` in a scratch keyfile; the file's lines afterwards,
    /// and whether it changed.
    fn migrate(before: &str) -> (bool, Vec<String>) {
        let dir = tempdir();
        let path = dir.join("keyfile");
        std::fs::write(&path, format!("{GROUP}{before}")).unwrap();
        let changed = migrate_legacy(&path);
        let after = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = after
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('['))
            .map(str::to_owned)
            .collect();
        lines.sort();
        (changed, lines)
    }

    fn tempdir() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "myna-migrate-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Every legacy shape, as the file holds it, and what it becomes.
    #[test]
    fn each_legacy_shape_becomes_its_steps() {
        let steps = "processing-steps=['tidy']";
        let french = "processing-steps=['tidy', 'translate']";
        let table: [(&str, &[&str]); 15] = [
            ("processing-chain='exact'\n", &[]),
            ("processing-chain='tidy'\n", &[steps]),
            (
                "processing-chain='tidy-french'\n",
                &[french, "processing-params={'translate.to': 'fr'}"],
            ),
            (
                "processing-chain='keyword'\n",
                &["processing-steps=['fit']"],
            ),
            ("processing-chain='summary'\n", &[]),
            // A chain this build does not know still outranks the older keys.
            ("processing-chain='summary'\nprocessing-tidy=true\n", &[]),
            ("processing=false\nprocessing-tidy=true\n", &[]),
            ("processing=true\nprocessing-tidy=true\n", &[steps]),
            (
                "processing-tidy=true\nprocessing-translate=true\n",
                &[french, "processing-params={'translate.to': 'fr'}"],
            ),
            (
                "processing-translate=true\n",
                &[
                    "processing-steps=['translate']",
                    "processing-params={'translate.to': 'fr'}",
                ],
            ),
            ("processing-sentences=true\n", &[]),
            // The newest key wins over the ones before it.
            (
                "processing=true\nprocessing-tidy=true\nprocessing-chain='exact'\n",
                &[],
            ),
            (
                "chinese-script='traditional-taiwan'\n",
                &[
                    "processing-steps=['script']",
                    "processing-params={'script.mode': 'traditional-taiwan'}",
                ],
            ),
            ("chinese-script='as-recognised'\n", &[]),
            (
                "processing-chain='tidy'\nchinese-script='simplified'\n",
                &[
                    "processing-steps=['tidy', 'script']",
                    "processing-params={'script.mode': 'simplified'}",
                ],
            ),
        ];
        for (before, expected) in table {
            let (changed, lines) = migrate(before);
            assert!(changed, "{before:?} holds legacy keys");
            let mut expected: Vec<String> = expected.iter().map(|s| (*s).to_owned()).collect();
            expected.sort();
            assert_eq!(lines, expected, "from {before:?}");
        }
    }

    /// Charles' keyfile of 2026-11-22, with the keys around the legacy ones
    /// untouched.
    #[test]
    fn a_development_keyfile_migrates_and_keeps_its_other_keys() {
        let (changed, lines) = migrate(
            "processing-history=true\nprocessing=true\nsound-set='marimba'\n\
             hud-style='bar'\nvocabulary=['Myna', 'IBus']\nprocessing-translate=false\n\
             processing-sentences=true\nprocessing-tidy=true\nprocessing-chain='tidy'\n",
        );
        assert!(changed);
        assert_eq!(
            lines,
            [
                "hud-style='bar'",
                "processing-history=true",
                "processing-steps=['tidy']",
                "sound-set='marimba'",
                "vocabulary=['Myna', 'IBus']",
            ]
        );
    }

    #[test]
    fn migration_is_idempotent_and_leaves_a_clean_file_alone() {
        let dir = tempdir();
        let path = dir.join("keyfile");
        std::fs::write(&path, format!("{GROUP}processing-chain='tidy'\n")).unwrap();
        assert!(migrate_legacy(&path));
        let once = std::fs::read(&path).unwrap();
        assert!(!migrate_legacy(&path));
        assert_eq!(std::fs::read(&path).unwrap(), once);
        assert!(!migrate_legacy(&dir.join("absent")));
    }

    #[test]
    fn migration_never_overwrites_an_explicit_value() {
        let (changed, lines) = migrate(
            "processing-steps=['fit']\nprocessing-chain='tidy-french'\n\
             chinese-script='simplified'\n",
        );
        assert!(changed);
        assert_eq!(
            lines,
            [
                "processing-params={'script.mode': 'simplified', 'translate.to': 'fr'}",
                "processing-steps=['fit']",
            ]
        );
        let (_, lines) = migrate(
            "processing-steps=@as []\nprocessing-params=@a{ss} {}\nprocessing-chain='tidy'\n",
        );
        assert_eq!(
            lines,
            ["processing-params=@a{ss} {}", "processing-steps=@as []"]
        );
    }

    #[test]
    fn a_preset_reads_back_from_what_it_writes() {
        for chain in ProcessingChain::ALL {
            let (steps, params) = with_chain(&[], &Params::new(), chain);
            assert_eq!(chain_of(&steps, &params), chain);
        }
    }

    #[test]
    fn a_list_that_matches_no_preset_reads_as_the_nearest() {
        let none = Params::new();
        let table = [
            (ids(&["tidy", "fit"]), ProcessingChain::TIDY),
            (ids(&["translate"]), ProcessingChain::TIDY_FRENCH),
            (ids(&["fit", "translate"]), ProcessingChain::KEYWORD),
            (ids(&["script"]), ProcessingChain::EXACT),
            (ids(&["nonsense"]), ProcessingChain::EXACT),
        ];
        for (list, expected) in table {
            assert_eq!(chain_of(&list, &none), expected, "{list:?}");
        }
        let german = params(&[("translate.to", "de")]);
        assert_eq!(
            chain_of(&ids(&["tidy", "translate"]), &german),
            ProcessingChain::TIDY
        );
    }

    #[test]
    fn choosing_a_preset_keeps_the_script_and_unknown_steps() {
        let (list, p) = with_chain(
            &ids(&["tidy", "script", "future"]),
            &params(&[("script.mode", "simplified")]),
            ProcessingChain::TIDY_FRENCH,
        );
        assert_eq!(list, ids(&["tidy", "translate", "script", "future"]));
        assert_eq!(p["script.mode"], "simplified");
        assert_eq!(p["translate.to"], "fr");
        let (list, _) = with_chain(&list, &p, ProcessingChain::EXACT);
        assert_eq!(list, ids(&["script", "future"]));
    }

    #[test]
    fn the_script_reads_and_writes_through_its_step() {
        let (list, p) = with_script(&ids(&["tidy"]), &Params::new(), ChineseScript::Traditional);
        assert_eq!(list, ids(&["tidy", "script"]));
        assert_eq!(script_of(&list, &p), ChineseScript::Traditional);
        assert_eq!(script_of(&ids(&["tidy"]), &p), ChineseScript::AsRecognised);
        assert_eq!(
            script_of(&ids(&["script"]), &Params::new()),
            ChineseScript::AsRecognised
        );
        let (list, p) = with_script(&list, &p, ChineseScript::AsRecognised);
        assert_eq!(list, ids(&["tidy"]));
        assert!(p.is_empty());
    }

    #[test]
    fn steps_are_ordered_by_stage_once_each() {
        assert_eq!(
            in_stage_order(&ids(&[
                "script",
                "x",
                "translate",
                "fit",
                "translate",
                "tidy"
            ])),
            ids(&["fit", "tidy", "translate", "script", "x"])
        );
    }

    #[test]
    fn the_schema_and_the_nicks_agree() {
        for chain in ProcessingChain::ALL {
            assert_eq!(ProcessingChain::from_nick(chain.nick()), Some(chain));
        }
        for script in ChineseScript::ALL {
            assert_eq!(ChineseScript::from_nick(script.nick()), Some(script));
        }
        assert_eq!(ProcessingChain::from_nick("summary"), None);
        assert_eq!(ChineseScript::from_nick("pinyin"), None);
    }

    /// Migrate `before` through both steps, as `migrate_legacy_store` does.
    fn migrate_both(before: &str) -> (bool, Vec<String>) {
        let dir = tempdir();
        let path = dir.join("keyfile");
        std::fs::write(&path, format!("{GROUP}{before}")).unwrap();
        let legacy = migrate_legacy(&path);
        let changed = migrate_steps(&path) || legacy;
        let after = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = after
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('['))
            .map(str::to_owned)
            .collect();
        lines.sort();
        (changed, lines)
    }

    const EXAMPLE: &str = "('example', 'Tidy and translate', [('tidy', true, {}), \
                           ('translate', true, {'to': 'fr'})])";

    #[test]
    fn a_step_list_becomes_the_active_chain_and_the_fit_switch() {
        let (changed, lines) = migrate_both(
            "processing-steps=['translate', 'fit', 'tidy', 'future']\n\
             processing-params={'translate.to': 'de', 'future.x': '1', 'gone.y': '2'}\n\
             sounds=false\n",
        );
        assert!(changed);
        assert_eq!(
            lines,
            [
                "active-chain='my-steps'".to_owned(),
                format!(
                    "chains=[{EXAMPLE}, ('my-steps', 'My steps', [('tidy', true, {{}}), \
                     ('translate', true, {{'to': 'de'}}), ('future', true, {{'x': '1'}})])]"
                ),
                "fit-to-field=true".to_owned(),
                "sounds=false".to_owned(),
            ]
        );
    }

    #[test]
    fn a_list_of_fit_alone_or_nothing_makes_no_chain() {
        assert_eq!(
            migrate_both("processing-steps=['fit']\n"),
            (true, vec!["fit-to-field=true".to_owned()])
        );
        assert_eq!(
            migrate_both("processing-steps=@as []\nprocessing-params=@a{ss} {}\n"),
            (true, vec!["fit-to-field=false".to_owned()])
        );
        assert_eq!(
            migrate_both("processing-params={'translate.to': 'de'}\n"),
            (true, Vec::new())
        );
        assert_eq!(
            migrate_both("sounds=false\n"),
            (false, vec!["sounds=false".to_owned()])
        );
    }

    /// Values already in the new keys win, and an id already taken is not
    /// reused.
    #[test]
    fn the_step_migration_never_overwrites_the_new_keys() {
        let (_, lines) = migrate_both(
            "processing-steps=['tidy']\nactive-chain='mine'\nfit-to-field=true\n\
             chains=[('my-steps', 'Mine', @a(sba{ss}) [])]\n",
        );
        assert_eq!(
            lines,
            [
                "active-chain='mine'",
                "chains=[('my-steps', 'Mine', []), ('my-steps-2', 'My steps', [('tidy', true, {})])]",
                "fit-to-field=true",
            ]
        );
    }

    #[test]
    fn a_legacy_keyfile_migrates_all_the_way_once() {
        let (changed, lines) = migrate_both("processing-chain='tidy-french'\n");
        assert!(changed);
        assert_eq!(
            lines,
            [
                "active-chain='my-steps'".to_owned(),
                format!(
                    "chains=[{EXAMPLE}, ('my-steps', 'My steps', [('tidy', true, {{}}), \
                     ('translate', true, {{'to': 'fr'}})])]"
                ),
                "fit-to-field=false".to_owned(),
            ]
        );
        let dir = tempdir();
        let path = dir.join("keyfile");
        std::fs::write(&path, format!("{GROUP}processing-chain='tidy'\n")).unwrap();
        assert!(migrate_legacy(&path));
        assert!(migrate_steps(&path));
        let once = std::fs::read(&path).unwrap();
        assert!(!migrate_steps(&path));
        assert!(!migrate_legacy(&path));
        assert_eq!(std::fs::read(&path).unwrap(), once);
        assert!(!migrate_steps(&dir.join("absent")));
    }
}
