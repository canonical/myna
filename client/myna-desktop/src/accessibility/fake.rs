//! An in-memory fake `AccessibilityAnnouncer` for hermetic tests
//! (contracts/announcer.md).

use std::sync::{Arc, Mutex};

use super::{AccessibilityAnnouncer, AnnounceError, AnnouncementText, Severity};
use async_trait::async_trait;

/// One recorded `announce()` or `set_state()` call, for test assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    Announce {
        text: String,
        severity: Option<Severity>,
    },
    SetState {
        name: String,
        description: String,
    },
}

/// Records every `announce()`/`set_state()` call; never touches a real bus.
/// Contract A1/A2/A3/A5 tests exercise this directly (contracts/announcer.md).
/// Clone the log handle (`.log()`) before moving the fake into a controller,
/// the same convention `MockIndicator`/`MockInjector` already use.
#[derive(Debug, Default)]
pub struct FakeAnnouncer {
    calls: Arc<Mutex<Vec<Recorded>>>,
    /// When `Some`, the next `announce()` call returns this error instead of
    /// recording (contract A5: announcement-failure handling).
    pub fail_next: Option<AnnounceError>,
}

impl FakeAnnouncer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A shared handle to the recorded call sequence.
    pub fn log(&self) -> Arc<Mutex<Vec<Recorded>>> {
        self.calls.clone()
    }
}

#[async_trait]
impl AccessibilityAnnouncer for FakeAnnouncer {
    async fn announce(
        &mut self,
        text: AnnouncementText,
        severity: Option<Severity>,
    ) -> Result<(), AnnounceError> {
        if let Some(err) = self.fail_next.take() {
            return Err(err);
        }
        self.calls.lock().unwrap().push(Recorded::Announce {
            text: text.as_str().to_string(),
            severity,
        });
        Ok(())
    }

    async fn set_state(&mut self, name: AnnouncementText, description: AnnouncementText) {
        self.calls.lock().unwrap().push(Recorded::SetState {
            name: name.as_str().to_string(),
            description: description.as_str().to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T009: FakeAnnouncer records announce()/set_state() calls ────────────

    #[tokio::test]
    async fn records_announce_calls_with_text_and_severity() {
        let mut fake = FakeAnnouncer::new();
        let log = fake.log();
        fake.announce(AnnouncementText::new("listening"), None)
            .await
            .unwrap();
        fake.announce(AnnouncementText::new("error"), Some(Severity::Critical))
            .await
            .unwrap();

        assert_eq!(
            *log.lock().unwrap(),
            vec![
                Recorded::Announce {
                    text: "listening".to_string(),
                    severity: None,
                },
                Recorded::Announce {
                    text: "error".to_string(),
                    severity: Some(Severity::Critical),
                },
            ]
        );
    }

    #[tokio::test]
    async fn records_set_state_calls_separately_from_announce() {
        let mut fake = FakeAnnouncer::new();
        let log = fake.log();
        fake.set_state(
            AnnouncementText::new("Dictation: listening"),
            AnnouncementText::new("Recording your speech"),
        )
        .await;

        assert_eq!(
            *log.lock().unwrap(),
            vec![Recorded::SetState {
                name: "Dictation: listening".to_string(),
                description: "Recording your speech".to_string(),
            }]
        );
    }
}
