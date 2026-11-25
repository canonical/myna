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
/// A saved chain as the record names it: its id and its name.
pub type ChainName = (String, String);

#[derive(Clone, Default)]
pub struct Context {
    /// The enabled steps, recorded as [`super::steps::label`].
    pub steps: Live<Vec<super::steps::StepSpec>>,
    /// The saved chain in use: its id and name.
    pub chain: Live<Option<ChainName>>,
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
    /// The steps and chain as the dictation started: a switch mid-dictation
    /// applies from the next one, so it is not what made this text.
    started: Option<(Vec<super::steps::StepSpec>, Option<ChainName>)>,
}

impl<P> Recorded<P> {
    pub fn new(inner: P, on: Live<bool>, path: Option<PathBuf>, context: Context) -> Self {
        Self {
            inner,
            on,
            path,
            context,
            steps: None,
            started: None,
        }
    }
}

#[async_trait]
impl<P: Processor> Processor for Recorded<P> {
    fn prepare(&mut self) {
        self.steps = None;
        self.started = Some((self.context.steps.get(), self.context.chain.get()));
        self.inner.prepare();
    }

    fn fit(&mut self, text: String, field: &Field) -> String {
        self.inner.fit(text, field)
    }

    async fn process(&mut self, text: String) -> String {
        let typed = self.inner.process(text).await;
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
        let (steps_then, chain_then) = self
            .started
            .clone()
            .unwrap_or_else(|| (self.context.steps.get(), self.context.chain.get()));
        let entry = Entry {
            time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
            heard: landing.heard,
            typed: landing.typed,
            chain: Some(super::steps::label(&steps_then)),
            chain_id: chain_then.as_ref().map(|(id, _)| id.clone()),
            chain_name: chain_then.map(|(_, name)| name),
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
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Shout;

    #[async_trait]
    impl Processor for Shout {
        async fn process(&mut self, text: String) -> String {
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
        let typed = recorded.process("um hi".into()).await;
        assert_eq!(typed, "UM HI");
        recorded.landed(landing("um hi", &typed));

        let lines = lines(&path);
        assert_eq!(lines[0]["heard"], "um hi");
        assert_eq!(lines[0]["typed"], "UM HI");
        assert_eq!(lines[0]["chain"], "tidy");
        assert_eq!(lines[0]["steps"][0]["task"], "text.cleanup");
        assert_eq!(lines[0]["speech_model"], "parakeet");
        assert_eq!(lines[0]["outcome"], "typed");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the record is the user's alone");
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
}
