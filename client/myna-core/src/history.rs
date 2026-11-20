//! The record of processed dictations: where it is kept and what a line of
//! it holds. The daemon appends to it; Myna Settings reads and clears it.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One dictation, or one piece of one: what was heard and what was typed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Seconds since the epoch.
    pub time: u64,
    pub heard: String,
    pub typed: String,
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
