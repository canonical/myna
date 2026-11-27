//! Fast fixture consumer for the vendored text evaluation corpus.
//!
//! The Python suite (`myna-bench text`) owns the sha256 pins and the gates;
//! this file only parses the same JSONL so deterministic `Rule` and `Check`
//! tests can iterate the pairs without any model, and fails if the slices
//! drift from the shape those tests rely on. Copy `load_pairs` and friends
//! into a consumer's test file rather than sharing a crate: it is thirty
//! lines and keeps the corpus out of every build graph.

use std::path::PathBuf;

use serde_json::Value;

fn corpus_dir() -> PathBuf {
    let root = std::env::var_os("MYNA_REPO_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    root.join("server/src/myna/benchmarker/textcorpus")
}

fn rows(slice: &str) -> Vec<Value> {
    let path = corpus_dir().join(format!("{slice}.jsonl"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines()
        .map(|line| serde_json::from_str(line).expect("each corpus line is a JSON object"))
        .collect()
}

fn field<'a>(row: &'a Value, key: &str) -> &'a str {
    row[key]
        .as_str()
        .unwrap_or_else(|| panic!("row without string {key}: {row}"))
}

/// `(id, heard, intended, category)` of every `disfluent_en` pair.
fn load_pairs() -> Vec<(String, String, String, String)> {
    rows("disfluent_en")
        .iter()
        .map(|r| {
            (
                field(r, "id").to_owned(),
                field(r, "heard").to_owned(),
                field(r, "intended").to_owned(),
                field(r, "category").to_owned(),
            )
        })
        .collect()
}

#[test]
fn clean_en_is_two_hundred_punctuated_sentences() {
    let rows = rows("clean_en");
    assert_eq!(rows.len(), 200);
    for row in &rows {
        let text = field(row, "text");
        assert!(text.chars().next().unwrap().is_uppercase(), "{text}");
        assert!(text.ends_with(['.', '?', '!']), "{text}");
    }
}

#[test]
fn translate_pairs_are_two_hundred_with_both_sides() {
    let rows = rows("translate_en_fr");
    assert_eq!(rows.len(), 200);
    for row in &rows {
        assert!(!field(row, "en").is_empty() && !field(row, "fr").is_empty());
        assert!(field(row, "id").starts_with("fleurs-"));
    }
}

#[test]
fn disfluent_pairs_cover_every_trap_category() {
    let pairs = load_pairs();
    assert!(pairs.len() >= 150, "{} pairs", pairs.len());
    for category in [
        "vocalized_pause",
        "repeat_k1",
        "repeat_k2",
        "repeat_k3",
        "repeat_k4",
        "reduplication_keep",
        "ambiguous_keep",
        "self_repair",
        "negation_trap",
        "negation_repair",
    ] {
        assert!(pairs.iter().any(|p| p.3 == category), "no {category} pair");
    }
}

#[test]
fn keep_categories_are_untouched_by_construction() {
    for (id, heard, intended, category) in load_pairs() {
        let keep = matches!(
            category.as_str(),
            "reduplication_keep" | "ambiguous_keep" | "negation_trap"
        );
        assert_eq!(keep, heard == intended, "{id} ({category})");
    }
}
