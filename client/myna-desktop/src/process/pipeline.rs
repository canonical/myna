//! A graph of steps run on text bound for the field.
//!
//! A step that needs a model names a [`Request`], what it needs done, and
//! never a model. [`Models`] answers the request with whatever fits.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use myna_core::history::Skip;
use myna_core::ChineseScript;

pub use super::check::Check;
use super::graph::{Graph, Op};
use super::script;
use super::vocabulary::Vocabulary;
use super::{Field, Processor};
use crate::live::Live;

pub use super::request::{Request, BIND_BUDGET};

/// How long past its run budget a model's own deadline may take to report.
const RUN_GRACE: Duration = Duration::from_millis(100);

/// Why a model gave no usable answer. The far side's reason is kept for
/// tests and never logged or recorded: it may quote the text.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("no model: {0}")]
    Unavailable(String),
    #[error("the model refused: {0}")]
    Refused(String),
    #[error("the model ran past its budget")]
    TimedOut,
}

impl ModelError {
    fn skip(&self) -> Skip {
        match self {
            ModelError::Unavailable(_) => Skip::NoModel,
            ModelError::Refused(_) => Skip::Refused,
            ModelError::TimedOut => Skip::Timeout,
        }
    }
}

/// A model's answer, and which model it was when the far side says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    pub model: Option<String>,
}

/// Whatever answers a [`Request`]: the place models are found and chosen.
#[async_trait]
pub trait Models: Send {
    async fn run(&mut self, request: &Request, text: &str) -> Result<Answer, ModelError>;

    /// Speech is starting: bind in the background whatever of `requests` is
    /// not bound, so a model evicted since the last text is found again.
    fn prepare(&mut self, _requests: &[Request]) {}
}

/// What each step did to the last piece of text, for the record.
pub type Trace = Arc<Mutex<Vec<myna_core::history::Step>>>;

/// The graph between the speech model and the field, read anew for each
/// piece of text, so a change in Settings applies from the next one.
pub struct Pipeline {
    graph: Live<Graph>,
    /// `graph` as the dictation started: a chain switched mid-dictation
    /// applies from the next one. `None` until a dictation prepares.
    started: Option<Graph>,
    models: Box<dyn Models>,
    written: Live<Vec<String>>,
    /// How Chinese is written once the graph is done; none to change.
    script: Live<ChineseScript>,
    /// `written` compiled, until it changes.
    vocabulary: (Vec<String>, Vocabulary),
    trace: Trace,
}

impl Pipeline {
    pub fn new(graph: impl Into<Live<Graph>>, models: impl Models + 'static) -> Self {
        Self {
            graph: graph.into(),
            started: None,
            models: Box::new(models),
            written: Live::new(Vec::new()),
            script: Live::new(ChineseScript::AsRecognised),
            vocabulary: (Vec::new(), Vocabulary::default()),
            trace: Trace::default(),
        }
    }

    /// The user's vocabulary: the spelling rule applies it, and a cleanup
    /// may write it in place of what the speech model misheard.
    #[must_use]
    pub fn written(mut self, words: Live<Vec<String>>) -> Self {
        self.written = words;
        self
    }

    /// The script Chinese is written in, applied to what the graph makes
    /// whichever graph it is, a piece at a time.
    #[must_use]
    pub fn scripted(mut self, script: Live<ChineseScript>) -> Self {
        self.script = script;
        self
    }

    /// The vocabulary compiled from the current entries.
    fn vocabulary(&mut self) -> &Vocabulary {
        let entries = self.written.get();
        if self.vocabulary.0 != entries {
            self.vocabulary = (entries.clone(), Vocabulary::new(&entries));
        }
        &self.vocabulary.1
    }

    /// Where the steps of the last piece of text are left.
    pub fn trace(&self) -> Trace {
        self.trace.clone()
    }

    /// One model step: its answer if usable, else `text` as it came.
    async fn ask(
        &mut self,
        request: &Request,
        check: Check,
        text: String,
        trace: &mut Vec<myna_core::history::Step>,
    ) -> String {
        self.try_ask(request, check, &text, trace)
            .await
            .unwrap_or(text)
    }

    /// One model step: its answer if usable.
    async fn try_ask(
        &mut self,
        request: &Request,
        check: Check,
        text: &str,
        trace: &mut Vec<myna_core::history::Step>,
    ) -> Option<String> {
        let written = self.written.get();
        let started = Instant::now();
        // The far side holds itself to the run budget (it is on the wire as
        // the deadline), so its own timeout is the one that is reported; this
        // one, a little later, only catches a `Models` that does not.
        let answer = tokio::time::timeout(
            request.run_budget + RUN_GRACE,
            self.models.run(request, text),
        )
        .await;
        let mut model = None;
        let mut answer_chars = 0;
        let mut typed = None;
        let skipped = match answer {
            Ok(Ok(answer)) => {
                model = answer.model;
                answer_chars = answer.text.chars().count();
                match check.verdict(text, &answer.text, &written) {
                    Ok(()) => {
                        typed = Some(answer.text);
                        None
                    }
                    Err(why) => Some(why),
                }
            }
            Ok(Err(err)) => Some(err.skip()),
            Err(_) => Some(Skip::Timeout),
        };
        if let Some(why) = skipped {
            // Counts only: no word of the text or the answer.
            myna_core::info_log!(
                "process",
                "{} skipped: {why:?} ({} chars in, {answer_chars} out)",
                request.task,
                text.chars().count()
            );
        }
        trace.push(myna_core::history::Step {
            task: request.task.clone(),
            model,
            ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            skipped,
        });
        typed
    }
}

#[async_trait]
impl Processor for Pipeline {
    fn prepare(&mut self) {
        let graph = self.graph.get();
        self.models.prepare(&graph.requests());
        self.started = Some(graph);
    }

    fn fit(&mut self, text: String, field: &Field) -> String {
        super::fit::fit(&text, field, self.vocabulary())
    }

    async fn process(&mut self, text: String) -> String {
        let graph = self.started.clone().unwrap_or_else(|| self.graph.get());
        self.vocabulary();
        let mut trace = Vec::new();
        let mut text = text;
        for op in graph.ops() {
            text = match op {
                Op::Rule { rule } => rule.apply_with(&text, &self.vocabulary.1),
                Op::Ask { request, check } => self.ask(request, *check, text, &mut trace).await,
                Op::Pivot {
                    first,
                    second,
                    check,
                } => match self.try_ask(first, *check, &text, &mut trace).await {
                    Some(mid) => self
                        .try_ask(second, *check, &mid, &mut trace)
                        .await
                        .unwrap_or(text),
                    None => text,
                },
            };
        }
        *self.trace.lock().unwrap_or_else(|p| p.into_inner()) = trace;
        script::convert(&text, self.script.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use myna_core::ProcessingChain;
    use myna_platform::text_input::FieldPurpose;

    /// Answers every request the same way, or never.
    enum Scripted {
        Says(&'static str),
        Fails,
        Hangs,
    }

    #[async_trait]
    impl Models for Scripted {
        async fn run(&mut self, _: &Request, _: &str) -> Result<Answer, ModelError> {
            match self {
                Scripted::Says(answer) => Ok(Answer {
                    text: answer.to_string(),
                    model: Some("scripted".into()),
                }),
                Scripted::Fails => Err(ModelError::Unavailable("none installed".into())),
                Scripted::Hangs => std::future::pending().await,
            }
        }
    }

    fn graph_of(chain: ProcessingChain) -> Graph {
        super::super::steps::preset_graph(chain)
    }

    fn cleanup(models: Scripted) -> Pipeline {
        Pipeline::new(graph_of(ProcessingChain::TIDY), models)
    }

    fn traced(pipeline: &Pipeline) -> Vec<myna_core::history::Step> {
        pipeline.trace().lock().unwrap().clone()
    }

    const HEARD: &str = "Let's meet Monday, no, Tuesday.";

    #[tokio::test]
    async fn an_answer_made_of_words_that_were_said_is_typed() {
        let mut pipeline = cleanup(Scripted::Says("Let's meet Tuesday."));
        assert_eq!(pipeline.process(HEARD.into()).await, "Let's meet Tuesday.");
        let trace = traced(&pipeline);
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].task, "text.cleanup");
        assert_eq!(trace[0].model.as_deref(), Some("scripted"));
        assert_eq!(trace[0].skipped, None);
    }

    #[tokio::test]
    async fn a_missing_model_types_what_was_heard() {
        let mut pipeline = cleanup(Scripted::Fails);
        assert_eq!(pipeline.process(HEARD.into()).await, HEARD);
        let trace = traced(&pipeline);
        assert_eq!(trace[0].model, None);
        assert_eq!(trace[0].skipped, Some(Skip::NoModel));
    }

    #[tokio::test(start_paused = true)]
    async fn a_model_over_its_budget_types_what_was_heard() {
        let mut pipeline = cleanup(Scripted::Hangs);
        assert_eq!(pipeline.process(HEARD.into()).await, HEARD);
        assert_eq!(traced(&pipeline)[0].skipped, Some(Skip::Timeout));
        // The trace is of the last text alone.
        assert_eq!(pipeline.process(HEARD.into()).await, HEARD);
        assert_eq!(traced(&pipeline).len(), 1);
    }

    #[tokio::test]
    async fn the_users_vocabulary_is_spelled_before_the_model_sees_it() {
        struct Echo(std::sync::Arc<Mutex<String>>);
        #[async_trait]
        impl Models for Echo {
            async fn run(&mut self, _: &Request, text: &str) -> Result<Answer, ModelError> {
                *self.0.lock().unwrap() = text.to_owned();
                Ok(Answer {
                    text: text.to_owned(),
                    model: None,
                })
            }
        }
        let seen = Arc::new(Mutex::new(String::new()));
        let written = Live::new(vec!["my sequel => MySQL".to_owned()]);
        let mut pipeline = Pipeline::new(graph_of(ProcessingChain::TIDY), Echo(seen.clone()))
            .written(written.clone());
        let said = "I run my sequel";
        assert_eq!(
            pipeline.process(said.into()).await,
            "I run MySQL",
            "the model echoing its input passes the check with the term"
        );
        assert_eq!(*seen.lock().unwrap(), "I run MySQL");
        written.set(vec!["Postgres".to_owned()]);
        assert_eq!(pipeline.process(said.into()).await, said);
    }

    #[tokio::test]
    async fn chinese_is_written_in_the_chosen_script_in_any_graph() {
        let script = Live::new(ChineseScript::AsRecognised);
        let mut pipeline =
            Pipeline::new(Graph::passthrough(), Scripted::Fails).scripted(script.clone());
        assert_eq!(pipeline.process("头发 NASA".into()).await, "头发 NASA");
        script.set(ChineseScript::Traditional);
        assert_eq!(pipeline.process("头发 NASA".into()).await, "頭髮 NASA");
        let mut tidy = Pipeline::new(graph_of(ProcessingChain::TIDY), Scripted::Says("我们出发"))
            .scripted(script);
        assert_eq!(tidy.process("我们出发".into()).await, "我們出發");
    }

    /// The graph is read as a dictation starts: a chain switched while it
    /// runs applies from the next one.
    #[tokio::test]
    async fn a_chain_switched_mid_dictation_applies_from_the_next() {
        let shared = Live::new(Graph::passthrough());
        let mut pipeline = Pipeline::new(shared.clone(), Scripted::Says("As heard."));
        pipeline.prepare();
        shared.set(graph_of(ProcessingChain::TIDY));
        assert_eq!(pipeline.process("as heard".into()).await, "as heard");
        pipeline.prepare();
        assert_eq!(pipeline.process("as heard".into()).await, "As heard.");
    }

    /// Fitting is the last change: a step that recases after the words
    /// before it cannot undo it.
    #[tokio::test]
    async fn fitting_runs_after_the_graph() {
        let mut pipeline = cleanup(Scripted::Says("The end."));
        let mid = Field::after(FieldPurpose::Plain, Some('s'));
        let processed = pipeline.process("the end".into()).await;
        assert_eq!(processed, "The end.", "the cleanup recased it");
        assert_eq!(pipeline.fit(processed, &mid), "the end.");
    }
}
