//! A record of processed dictations, for checking what processing did.
//!
//! Off unless the user turns it on: it is the one place dictated text is
//! written to disk.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use myna_core::history::{append, Entry, Outcome};

use super::pipeline::Trace;
use super::{Field, Landing, Processor};
use crate::live::Live;

/// What a record says besides the text, read as each piece is recorded.
#[derive(Clone, Default)]
pub struct Context {
    /// The enabled steps, recorded as [`super::steps::label`].
    pub steps: Live<Vec<super::steps::StepSpec>>,
    /// What the steps of the piece just processed did.
    pub trace: Trace,
    /// The model the backend said it serves, once a press has asked.
    pub speech_model: Live<Option<String>>,
    pub version: Option<&'static str>,
}

/// Runs a processor and, while `on`, appends what became of each piece, as
/// the controller reports it, to `path` as one JSON line.
pub struct Recorded<P> {
    inner: P,
    on: Live<bool>,
    path: Option<PathBuf>,
    context: Context,
    /// What the steps did to the piece last processed, until it is delivered.
    steps: Option<Vec<myna_core::history::Step>>,
}

impl<P> Recorded<P> {
    pub fn new(inner: P, on: Live<bool>, path: Option<PathBuf>, context: Context) -> Self {
        Self {
            inner,
            on,
            path,
            context,
            steps: None,
        }
    }
}

#[async_trait]
impl<P: Processor> Processor for Recorded<P> {
    fn prepare(&mut self) {
        self.steps = None;
        self.inner.prepare();
    }

    async fn process(&mut self, text: String, field: &Field) -> String {
        let typed = self.inner.process(text, field).await;
        self.steps = Some(
            self.context
                .trace
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
        );
        typed
    }

    fn landed(&mut self, landing: Landing) {
        let steps = self.steps.take().unwrap_or_default();
        // Text discarded while held never went through the steps.
        let steps = if landing.outcome == Outcome::Discarded {
            Vec::new()
        } else {
            steps
        };
        let Some(path) = self.path.as_ref().filter(|_| self.on.get()) else {
            return;
        };
        let ms = |d: std::time::Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        let entry = Entry {
            time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
            heard: landing.heard,
            typed: landing.typed,
            chain: Some(super::steps::label(&self.context.steps.get())),
            steps,
            speech_model: self.context.speech_model.get(),
            version: self.context.version.map(str::to_owned),
            outcome: Some(landing.outcome),
            asr_tail_ms: landing.timing.map(|t| ms(t.asr_tail)),
            inject_ms: landing.timing.map(|t| ms(t.inject)),
            dead_air_ms: landing.timing.map(|t| ms(t.dead_air)),
        };
        // A record that cannot be written must not cost the dictation.
        if let Err(err) = append(path, &entry) {
            myna_core::info_log!("process", "history not written: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::landing::Timing;
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    struct Shout;

    #[async_trait]
    impl Processor for Shout {
        async fn process(&mut self, text: String, _: &Field) -> String {
            text.to_uppercase()
        }
    }

    fn context() -> Context {
        let context = Context {
            steps: Live::new(crate::process::steps::preset_steps(
                myna_core::ProcessingChain::TIDY,
            )),
            speech_model: Live::new(Some("parakeet".into())),
            version: Some("1.2.3"),
            ..Context::default()
        };
        context
            .trace
            .lock()
            .unwrap()
            .push(myna_core::history::Step {
                task: "text.cleanup".into(),
                model: Some("fdt".into()),
                ms: 4,
                skipped: None,
            });
        context
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("myna-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("nested/history.jsonl")
    }

    fn landing(heard: &str, typed: &str) -> Landing {
        Landing {
            heard: heard.into(),
            typed: typed.into(),
            outcome: Outcome::Typed,
            timing: None,
        }
    }

    fn lines(path: &std::path::Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn records_what_was_heard_and_what_was_typed() {
        let path = scratch("on");
        let mut recorded = Recorded::new(Shout, Live::new(true), Some(path.clone()), context());
        let typed = recorded.process("um hi".into(), &Field::default()).await;
        assert_eq!(typed, "UM HI");
        recorded.landed(landing("um hi", &typed));
        recorded.landed(landing("bye", "BYE"));

        let lines = lines(&path);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["heard"], "um hi");
        assert_eq!(lines[0]["typed"], "UM HI");
        assert_eq!(lines[1]["heard"], "bye");
        assert!(lines[0]["time"].as_u64().unwrap() > 0);
        assert_eq!(lines[0]["chain"], "tidy");
        assert_eq!(lines[0]["steps"][0]["task"], "text.cleanup");
        assert_eq!(lines[0]["steps"][0]["model"], "fdt");
        assert_eq!(lines[0]["speech_model"], "parakeet");
        assert_eq!(lines[0]["version"], "1.2.3");
        assert_eq!(lines[0]["outcome"], "typed");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the record is the user's alone");
    }

    #[tokio::test]
    async fn a_piece_that_was_not_processed_carries_no_steps() {
        let path = scratch("bypass");
        let mut recorded = Recorded::new(Shout, Live::new(true), Some(path.clone()), context());
        recorded.landed(landing("hi", "hi"));
        let lines = lines(&path);
        assert!(lines[0].get("steps").is_none());
        assert_eq!(lines[0]["chain"], "tidy");
    }

    #[tokio::test]
    async fn text_discarded_while_held_carries_no_steps_and_leaves_none_behind() {
        let path = scratch("discarded");
        let mut recorded = Recorded::new(Shout, Live::new(true), Some(path.clone()), context());
        recorded.process("a".into(), &Field::default()).await;
        recorded.landed(Landing {
            outcome: Outcome::Discarded,
            ..landing("held", "")
        });
        recorded.landed(landing("b", "b"));
        let lines = lines(&path);
        assert_eq!(lines[0]["outcome"], "discarded");
        assert!(lines[0].get("steps").is_none());
        assert!(lines[1].get("steps").is_none());
    }

    #[tokio::test]
    async fn steps_belong_to_the_piece_they_ran_for() {
        let path = scratch("steps-once");
        let mut recorded = Recorded::new(Shout, Live::new(true), Some(path.clone()), context());
        recorded.process("a".into(), &Field::default()).await;
        recorded.landed(landing("a", "A"));
        recorded.landed(landing("b", "b"));
        let lines = lines(&path);
        assert!(lines[0].get("steps").is_some());
        assert!(lines[1].get("steps").is_none());
    }

    #[test]
    fn timings_are_kept_in_milliseconds_and_a_lost_piece_is_marked() {
        let path = scratch("timing");
        let mut recorded = Recorded::new(Shout, Live::new(true), Some(path.clone()), context());
        recorded.landed(Landing {
            timing: Some(Timing {
                asr_tail: Duration::from_millis(120),
                inject: Duration::from_millis(3),
                dead_air: Duration::from_millis(480),
            }),
            ..landing("hi", "HI")
        });
        recorded.landed(Landing {
            outcome: Outcome::FocusLost,
            ..landing("gone", "")
        });
        let lines = lines(&path);
        assert_eq!(lines[0]["asr_tail_ms"], 120);
        assert_eq!(lines[0]["inject_ms"], 3);
        assert_eq!(lines[0]["dead_air_ms"], 480);
        assert_eq!(lines[1]["outcome"], "focus-lost");
        assert_eq!(lines[1]["heard"], "gone");
        assert!(lines[1].get("dead_air_ms").is_none());
    }

    #[test]
    fn writes_nothing_while_off_and_follows_the_switch() {
        let path = scratch("off");
        let on = Live::new(false);
        let mut recorded = Recorded::new(Shout, on.clone(), Some(path.clone()), Context::default());
        recorded.landed(landing("hi", "HI"));
        assert!(!path.exists());

        on.set(true);
        recorded.landed(landing("hi", "HI"));
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
    }

    #[test]
    fn a_record_that_cannot_be_written_does_not_panic() {
        let mut recorded = Recorded::new(
            Shout,
            Live::new(true),
            Some(PathBuf::from("/proc/myna-nowhere/history.jsonl")),
            Context::default(),
        );
        recorded.landed(landing("hi", "HI"));
    }
}
