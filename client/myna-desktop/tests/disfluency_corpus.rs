//! The classical rule against the vendored text corpus
//! (`server/src/myna/benchmarker/textcorpus/`, gates in `myna-bench text`).
//!
//! Set `MYNA_TEXT_DUMP` to a path to write every output as JSON, which the
//! Python harness scores through a bridge system.

use std::collections::BTreeMap;
use std::path::PathBuf;

use myna_desktop::process::disfluency::Lang;
use myna_desktop::process::rules::Rule;
use serde_json::Value;

fn rows(slice: &str) -> Vec<Value> {
    let root = std::env::var_os("MYNA_REPO_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let path = root.join(format!(
        "server/src/myna/benchmarker/textcorpus/{slice}.jsonl"
    ));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines()
        .map(|l| serde_json::from_str(l).expect("a JSON object per line"))
        .collect()
}

fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row[key]
        .as_str()
        .unwrap_or_else(|| panic!("no {key}: {row}"))
}

fn tidy(heard: &str) -> String {
    Rule::Disfluency(Lang::Any).apply(heard)
}

/// `(exact, total)` per category over `disfluent_en`.
fn by_category() -> BTreeMap<String, (usize, usize)> {
    let mut tally = BTreeMap::new();
    for row in rows("disfluent_en") {
        let entry = tally
            .entry(text(&row, "category").to_owned())
            .or_insert((0, 0));
        entry.1 += 1;
        entry.0 += usize::from(tidy(text(&row, "heard")) == text(&row, "intended"));
    }
    tally
}

#[test]
fn dump_outputs_for_the_python_harness() {
    let Some(path) = std::env::var_os("MYNA_TEXT_DUMP") else {
        return;
    };
    let mut out = BTreeMap::new();
    for slice in ["clean_en", "disfluent_en"] {
        for row in rows(slice) {
            let heard = text(&row, if slice == "clean_en" { "text" } else { "heard" });
            out.insert(heard.to_owned(), tidy(heard));
        }
    }
    std::fs::write(path, serde_json::to_string(&out).unwrap()).unwrap();
}

#[test]
fn clean_sentences_come_back_unchanged_but_for_a_natural_repeat() {
    let changed: Vec<_> = rows("clean_en")
        .iter()
        .map(|r| text(r, "text").to_owned())
        .filter(|t| tidy(t) != *t)
        .collect();
    assert!(changed.len() <= 1, "{changed:?}");
}

#[test]
fn the_rule_gets_every_pause_and_repeat_right_and_never_touches_a_trap() {
    let tally = by_category();
    for category in [
        "vocalized_pause",
        "repeat_k1",
        "repeat_k2",
        "repeat_k3",
        "repeat_k4",
        "ambiguous_keep",
        "negation_trap",
    ] {
        let (exact, total) = tally[category];
        assert_eq!(exact, total, "{category}: {tally:?}");
    }
    let (exact, total) = tally["reduplication_keep"];
    assert!(
        exact * 10 >= total * 9,
        "reduplication_keep {exact}/{total}"
    );
}

#[test]
fn a_self_repair_is_left_for_the_model() {
    for row in rows("disfluent_en") {
        if matches!(text(&row, "category"), "self_repair" | "negation_repair") {
            let heard = text(&row, "heard");
            let out = tidy(heard);
            assert!(
                out == heard
                    || out.split_whitespace().count() + 2 >= heard.split_whitespace().count(),
                "{heard} -> {out}"
            );
        }
    }
}
