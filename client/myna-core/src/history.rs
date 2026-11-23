//! The record of processed dictations: where it is kept and what a line of
//! it holds. The daemon appends to it; Myna Settings reads and clears it.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One dictation, or one piece of one: what was heard, what was typed, and
/// what turned the one into the other. Everything past the text is absent
/// from a record written before it was kept.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Seconds since the epoch.
    pub time: u64,
    pub heard: String,
    pub typed: String,
    /// The steps that ran, as a short label (a preset's nick, else the ids
    /// joined by `+`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<String>,
    /// The saved chain in use, by its stable id, and its name then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_name: Option<String>,
    /// What each step of the chain did, in the order they ran.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    /// The model that heard the speech, as the backend names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_model: Option<String>,
    /// The Myna build that typed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// How it ended; a record written before this was kept typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// Stop edge to the text being ready to process, in milliseconds. The
    /// three timings are monotonic and only set for text that landed after
    /// the stop edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asr_tail_ms: Option<u64>,
    /// How long the field took to take the text, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inject_ms: Option<u64>,
    /// Stop edge to the text being in the field, in milliseconds: what the
    /// user waited through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_air_ms: Option<u64>,
}

/// How a dictation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// The text reached the field.
    Typed,
    /// Focus left the field first, so `typed` is what would have been typed
    /// and `heard` is what can be recovered.
    FocusLost,
    /// Processing left nothing to type, so `typed` is empty.
    Empty,
    /// The field would not take it for a reason other than focus.
    Refused,
    /// The dictation was cancelled while the text was still held back, so
    /// nothing was typed and `heard` is what can be recovered.
    Discarded,
}

/// One step of a chain, as it went for one piece of text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// What was asked: `text.cleanup`, `text.translate`.
    pub task: String,
    /// The model that answered, when it said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// How long the step took, in milliseconds.
    pub ms: u64,
    /// Why the step left the text as it was; `None` when its answer was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<Skip>,
}

/// Why a step left the text as it was. A code, never words from the
/// dictation or from a model, so neither the record nor a log can leak
/// what was said through it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Skip {
    /// Nothing that answers the request was reachable.
    NoModel,
    /// The model said it could not answer.
    Refused,
    /// No answer within the step's budget.
    Timeout,
    /// The answer held no words.
    Empty,
    /// The answer held a word that was not said, or moved one.
    NewWord,
    /// The answer dropped more of what was said than a cleanup may.
    TooManyDropped,
    /// The answer dropped too many words in a row.
    LongDrop,
    /// The answer dropped a lone "not", "never" or "n't".
    DroppedNegation,
    /// The answer opened with a chat reply such as "Sure!".
    Chatty,
    /// The answer was far shorter or longer than what was said.
    OddLength,
    /// A reason this build does not know, from a newer or older record.
    #[serde(other)]
    Other,
}

/// Where the record is kept: beside the settings in the snap's own data, as
/// [`crate::settings::store_path`] finds them, or the user's state directory
/// for an unpackaged daemon.
pub fn path() -> Option<PathBuf> {
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let unpackaged =
        std::env::var_os("GSETTINGS_BACKEND").as_deref() == Some(std::ffi::OsStr::new("keyfile"));
    path_from(
        env("SNAP_USER_COMMON"),
        unpackaged,
        env("XDG_STATE_HOME"),
        env("HOME"),
    )
}

fn path_from(
    snap_common: Option<PathBuf>,
    unpackaged: bool,
    state: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    let dir = match snap_common {
        Some(common) => common,
        None if unpackaged => state
            .or_else(|| home.map(|home| home.join(".local/state")))?
            .join("myna"),
        None => home?.join("snap/myna/common"),
    };
    Some(dir.join("processing-history.jsonl"))
}

/// Add `entry` to the record at `path`, readable by the user alone.
pub fn append(path: &Path, entry: &Entry) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_string(entry)?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?
        .write_all(line.as_bytes())
}

/// The last `limit` entries of the record at `path`, newest first. A record
/// that is missing reads as empty, and a line that is not an entry is skipped.
pub fn recent(path: &Path, limit: usize) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .rev()
        .filter_map(|line| serde_json::from_str(line).ok())
        .take(limit)
        .collect()
}

/// Forget everything recorded at `path`. A record that is not there is
/// already forgotten.
pub fn clear(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("myna-history-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir.join("nested/history.jsonl")
    }

    fn entry(time: u64, typed: &str) -> Entry {
        Entry {
            time,
            heard: format!("um {typed}"),
            typed: typed.to_owned(),
            ..Entry::default()
        }
    }

    #[test]
    fn the_record_sits_beside_the_settings() {
        let dir = |path: &str| Some(PathBuf::from(path));
        let file = |path: &str| Some(PathBuf::from(path).join("processing-history.jsonl"));
        // In the snap, and for Myna Settings outside it.
        assert_eq!(
            path_from(dir("/c"), false, dir("/s"), dir("/h")),
            file("/c")
        );
        assert_eq!(
            path_from(None, false, dir("/s"), dir("/h")),
            file("/h/snap/myna/common")
        );
        // An unpackaged daemon and the settings app launched beside it.
        assert_eq!(path_from(None, true, dir("/s"), dir("/h")), file("/s/myna"));
        assert_eq!(
            path_from(None, true, None, dir("/h")),
            file("/h/.local/state/myna")
        );
        assert_eq!(path_from(None, true, None, None), None);
        assert_eq!(path_from(None, false, dir("/s"), None), None);
        assert!(path().is_some());
    }

    #[test]
    fn entries_come_back_newest_first_and_private() {
        let path = scratch("recent");
        assert!(recent(&path, 10).is_empty());
        for (time, typed) in [(1, "one"), (2, "two"), (3, "three")] {
            append(&path, &entry(time, typed)).unwrap();
        }
        assert_eq!(recent(&path, 2), [entry(3, "three"), entry(2, "two")]);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_skip_reads_back_as_a_code_and_an_unknown_one_as_other() {
        let step = |skipped: &str| -> Step {
            serde_json::from_str(&format!(
                "{{\"task\":\"text.cleanup\",\"ms\":1,\"skipped\":{skipped}}}"
            ))
            .unwrap()
        };
        assert_eq!(
            step("\"dropped-negation\"").skipped,
            Some(Skip::DroppedNegation)
        );
        assert_eq!(
            step("\"the answer failed NoNewWords\"").skipped,
            Some(Skip::Other)
        );
        assert_eq!(
            serde_json::to_string(&Skip::TooManyDropped).unwrap(),
            "\"too-many-dropped\""
        );
    }

    #[test]
    fn what_did_the_work_is_kept_and_an_older_line_still_reads() {
        let path = scratch("details");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"time\":1,\"heard\":\"um one\",\"typed\":\"one\"}\n",
        )
        .unwrap();
        let full = Entry {
            chain: Some("tidy".into()),
            steps: vec![
                Step {
                    task: "text.cleanup".into(),
                    model: Some("fdt".into()),
                    ms: 4,
                    skipped: None,
                },
                Step {
                    task: "text.translate".into(),
                    model: None,
                    ms: 1000,
                    skipped: Some(Skip::Timeout),
                },
            ],
            speech_model: Some("parakeet".into()),
            version: Some("0.3.3".into()),
            ..entry(2, "two")
        };
        append(&path, &full).unwrap();
        assert_eq!(recent(&path, 10), [full, entry(1, "one")]);
    }

    #[test]
    fn timings_and_outcome_are_kept_and_absent_ones_stay_out_of_the_line() {
        let path = scratch("timings");
        let timed = Entry {
            outcome: Some(Outcome::FocusLost),
            asr_tail_ms: Some(120),
            inject_ms: Some(3),
            dead_air_ms: Some(480),
            ..entry(2, "two")
        };
        append(&path, &timed).unwrap();
        append(&path, &entry(1, "one")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("\"outcome\":\"focus-lost\""));
        assert!(lines[0].contains("\"dead_air_ms\":480"));
        for key in ["outcome", "asr_tail_ms", "inject_ms", "dead_air_ms"] {
            assert!(!lines[1].contains(key), "{key} written when unknown");
        }
        assert_eq!(recent(&path, 10), [entry(1, "one"), timed]);
    }

    #[test]
    fn a_line_that_is_not_an_entry_is_skipped() {
        let path = scratch("damaged");
        append(&path, &entry(1, "one")).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"time\": \n")
            .unwrap();
        assert_eq!(recent(&path, 10), [entry(1, "one")]);
    }

    #[test]
    fn clearing_forgets_the_record_and_can_be_repeated() {
        let path = scratch("clear");
        append(&path, &entry(1, "one")).unwrap();
        clear(&path).unwrap();
        assert!(recent(&path, 10).is_empty());
        clear(&path).unwrap();
        assert!(clear(path.parent().unwrap()).is_err());
    }
}
