//! A record of processed dictations, for checking what processing did.
//!
//! Off unless the user turns it on: it is the one place dictated text is
//! written to disk.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use myna_core::history::{append, Entry};

use super::Processor;
use crate::live::Live;

/// Runs a processor and, while `on`, appends what went in and what came out
/// to `path` as one JSON line.
pub struct Recorded<P> {
    inner: P,
    on: Live<bool>,
    path: Option<PathBuf>,
}

impl<P> Recorded<P> {
    pub fn new(inner: P, on: Live<bool>, path: Option<PathBuf>) -> Self {
        Self { inner, on, path }
    }
}

#[async_trait]
impl<P: Processor> Processor for Recorded<P> {
    async fn process(&mut self, text: String) -> String {
        let Some(path) = self.path.as_ref().filter(|_| self.on.get()) else {
            return self.inner.process(text).await;
        };
        let heard = text.clone();
        let typed = self.inner.process(text).await;
        let entry = Entry {
            time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
            heard,
            typed: typed.clone(),
        };
        // A record that cannot be written must not cost the dictation.
        if let Err(err) = append(path, &entry) {
            myna_core::info_log!("process", "history not written: {err}");
        }
        typed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Shout;

    #[async_trait]
    impl Processor for Shout {
        async fn process(&mut self, text: String) -> String {
            text.to_uppercase()
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("myna-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("nested/history.jsonl")
    }

    #[tokio::test]
    async fn records_what_was_heard_and_what_was_typed() {
        let path = scratch("on");
        let mut recorded = Recorded::new(Shout, Live::new(true), Some(path.clone()));
        assert_eq!(recorded.process("um hi".into()).await, "UM HI");
        assert_eq!(recorded.process("bye".into()).await, "BYE");

        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["heard"], "um hi");
        assert_eq!(lines[0]["typed"], "UM HI");
        assert_eq!(lines[1]["heard"], "bye");
        assert!(lines[0]["time"].as_u64().unwrap() > 0);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the record is the user's alone");
    }

    #[tokio::test]
    async fn writes_nothing_while_off_and_follows_the_switch() {
        let path = scratch("off");
        let on = Live::new(false);
        let mut recorded = Recorded::new(Shout, on.clone(), Some(path.clone()));
        assert_eq!(recorded.process("hi".into()).await, "HI");
        assert!(!path.exists());

        on.set(true);
        recorded.process("hi".into()).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
    }

    #[tokio::test]
    async fn a_record_that_cannot_be_written_still_types_the_text() {
        let mut recorded = Recorded::new(
            Shout,
            Live::new(true),
            Some(PathBuf::from("/proc/myna-nowhere/history.jsonl")),
        );
        assert_eq!(recorded.process("hi".into()).await, "HI");
    }
}
