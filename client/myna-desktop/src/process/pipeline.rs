//! A graph of steps run on text bound for the field.
//!
//! A step that needs a model names a [`Request`], what it needs done, and
//! never a model. [`Models`] answers the request with whatever fits.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use myna_core::history::Skip;
use myna_core::ChineseScript;

pub use super::check::Check;
use super::graph::{Graph, Id, Op, Src};
use super::script;
use super::vocabulary::Vocabulary;
use super::wire::Params;
use super::{Field, Processor};
use crate::inject::fold_control_chars;
use crate::live::Live;

/// Whether a step would rather have the faster model or the better one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Prefer {
    #[default]
    Latency,
    Accuracy,
}

/// What a step asks of a model: a purpose, never a model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// What to do with the text: `text.cleanup`, `text.translate`.
    pub task: String,
    /// What the task is about: `language`, `to`, a written `instruction`.
    /// Ordered, so equal requests make equal keys.
    pub params: Params,
    pub prefer: Prefer,
    /// How long one answer may take before the step is skipped.
    pub run_budget: Duration,
    /// How long finding and loading a model may take before the next
    /// candidate is tried.
    pub bind_budget: Duration,
}

impl Request {
    /// A request built from data: the task and its parameters, as a step
    /// declares them. The run budget follows the task.
    pub fn of(task: &str, params: Params) -> Self {
        let run_budget = match task {
            "text.cleanup" => Duration::from_secs(1),
            _ => Duration::from_secs(10),
        };
        Self {
            task: task.into(),
            params,
            prefer: Prefer::default(),
            run_budget,
            bind_budget: BIND_BUDGET,
        }
    }

    pub fn cleanup(language: Option<String>) -> Self {
        let params = language
            .map(|l| ("language".to_owned(), l.into()))
            .into_iter()
            .collect();
        Self::of("text.cleanup", params)
    }

    /// A dictation typed at once is translated at once, so the budget is a
    /// long dictation's: a sentence takes a model 60-260 ms.
    pub fn translate(from: &str, to: &str) -> Self {
        let params = [("language", from), ("to", to)]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.into()))
            .collect();
        Self::of("text.translate", params)
    }

    /// What a binding is cached under: everything that picks a model and
    /// nothing that does not (the run budget).
    pub fn key(&self) -> String {
        let params = serde_json::to_string(&self.params).unwrap_or_default();
        format!("{}|{:?}|{params}", self.task, self.prefer)
    }
}

/// A model that takes longer to load than this is not the one to dictate
/// with: the next candidate is tried.
pub const BIND_BUDGET: Duration = Duration::from_secs(5);

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
                let cleaned = fold_control_chars(&answer.text);
                answer_chars = cleaned.chars().count();
                match check.verdict(text, &cleaned, &written) {
                    Ok(()) => {
                        typed = Some(cleaned);
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

    async fn process(&mut self, text: String, _: &Field) -> String {
        let graph = self.started.clone().unwrap_or_else(|| self.graph.get());
        self.vocabulary();
        let mut trace = Vec::new();
        let mut made: HashMap<Id, String> = HashMap::new();
        let read = |made: &HashMap<Id, String>, src: Src| match src {
            Src::Input => text.clone(),
            Src::Node(id) => made.get(&id).cloned().unwrap_or_default(),
        };
        for id in graph.plan() {
            let op = graph.node(id);
            let out = match op {
                Op::Rule { from, rule } => rule.apply_with(&read(&made, *from), &self.vocabulary.1),
                Op::Ask {
                    from,
                    request,
                    check,
                    ..
                } => {
                    let input = read(&made, *from);
                    self.ask(request, *check, input, &mut trace).await
                }
                Op::Pivot {
                    from,
                    first,
                    second,
                } => {
                    let input = read(&made, *from);
                    match self
                        .try_ask(&first.request, first.check, &input, &mut trace)
                        .await
                    {
                        Some(mid) => self
                            .try_ask(&second.request, second.check, &mid, &mut trace)
                            .await
                            .unwrap_or(input),
                        None => input,
                    }
                }
            };
            made.insert(id, out);
        }
        *self.trace.lock().unwrap_or_else(|p| p.into_inner()) = trace;
        script::convert(&read(&made, graph.out()), self.script.get())
    }
}

#[cfg(test)]
mod tests {
    use super::super::graph::Builder;
    use super::*;
    use myna_core::ProcessingChain;
    use myna_platform::text_input::FieldPurpose;

    /// Answers every request the same way, or never.
    enum Scripted {
        Says(&'static str),
        Fails,
        Refuses,
        Hangs,
        TimesOut,
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
                Scripted::Refuses => Err(ModelError::Refused("cannot: um hi".into())),
                Scripted::Hangs => std::future::pending().await,
                Scripted::TimesOut => Err(ModelError::TimedOut),
            }
        }
    }

    fn graph_of(chain: ProcessingChain) -> Graph {
        Graph::preset(chain)
    }

    fn only(task: &str) -> Graph {
        let mut g = Builder::default();
        let out = match task {
            "text.translate" => g.ask(
                Src::Input,
                Request::translate("en", "fr"),
                Check::Usable { translation: true },
            ),
            _ => g.tidy(Src::Input),
        };
        g.finish(out)
    }

    fn cleanup(models: Scripted) -> Pipeline {
        Pipeline::new(graph_of(ProcessingChain::TIDY), models)
    }

    fn here() -> Field {
        Field::default()
    }

    /// Answers by the language a request translates to, or fails for it.
    struct ByTarget(Vec<(&'static str, Option<&'static str>)>);

    #[async_trait]
    impl Models for ByTarget {
        async fn run(&mut self, request: &Request, _: &str) -> Result<Answer, ModelError> {
            let to = request.params["to"].as_str().unwrap_or_default();
            match self.0.iter().find(|(t, _)| *t == to) {
                Some((_, Some(text))) => Ok(Answer {
                    text: (*text).into(),
                    model: Some(to.into()),
                }),
                _ => Err(ModelError::Unavailable("none".into())),
            }
        }
    }

    fn pivot() -> Graph {
        let mut g = Builder::default();
        let out = g.pivot(
            Src::Input,
            Request::translate("fr", "en"),
            Request::translate("en", "de"),
            Check::Usable { translation: true },
        );
        g.finish(out)
    }

    #[tokio::test]
    async fn a_pivot_runs_the_second_leg_on_what_the_first_made() {
        let models = ByTarget(vec![
            ("en", Some("See you Monday.")),
            ("de", Some("Bis Montag.")),
        ]);
        let mut pipeline = Pipeline::new(pivot(), models);
        assert_eq!(
            pipeline.process("A lundi.".into(), &here()).await,
            "Bis Montag."
        );
        let trace = traced(&pipeline);
        assert_eq!(trace.len(), 2);
        assert!(trace.iter().all(|step| step.skipped.is_none()));
    }

    #[tokio::test]
    async fn a_pivot_with_a_failed_leg_types_what_was_heard_never_the_half() {
        let first_fails = ByTarget(vec![("de", Some("Bis Montag."))]);
        let mut pipeline = Pipeline::new(pivot(), first_fails);
        assert_eq!(
            pipeline.process("A lundi.".into(), &here()).await,
            "A lundi."
        );
        assert_eq!(traced(&pipeline).len(), 1, "the second leg never ran");

        let second_fails = ByTarget(vec![("en", Some("See you Monday."))]);
        let mut pipeline = Pipeline::new(pivot(), second_fails);
        assert_eq!(
            pipeline.process("A lundi.".into(), &here()).await,
            "A lundi."
        );
        let trace = traced(&pipeline);
        assert_eq!(trace.len(), 2);
        assert_eq!(trace[1].skipped, Some(Skip::NoModel));
    }

    #[tokio::test]
    async fn each_leg_is_checked_against_its_own_input() {
        let empty = ByTarget(vec![("en", Some("   ")), ("de", Some("Bis Montag."))]);
        let mut pipeline = Pipeline::new(pivot(), empty);
        assert_eq!(
            pipeline.process("A lundi.".into(), &here()).await,
            "A lundi."
        );
        assert_eq!(traced(&pipeline)[0].skipped, Some(Skip::Empty));

        let runaway = ByTarget(vec![
            ("en", Some("See you Monday.")),
            ("de", Some("ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja ja")),
        ]);
        let mut pipeline = Pipeline::new(pivot(), runaway);
        assert_eq!(
            pipeline.process("A lundi.".into(), &here()).await,
            "A lundi."
        );
        assert_eq!(traced(&pipeline)[1].skipped, Some(Skip::OddLength));

        // 50 chars: too long for what was heard (8), fine for the first
        // leg's 15, which is what the second leg is checked against.
        let long = "Wir sehen uns am Montag um zehn Uhr im Buero, ja.";
        let wide = ByTarget(vec![("en", Some("See you Monday.")), ("de", Some(long))]);
        let mut pipeline = Pipeline::new(pivot(), wide);
        assert_eq!(pipeline.process("A lundi.".into(), &here()).await, long);
    }

    #[tokio::test]
    async fn the_graph_changed_while_running_applies_to_the_next_text() {
        let graph = Live::new(Graph::passthrough());
        let mut pipeline = Pipeline::new(graph.clone(), Scripted::Says("Bonjour."));
        assert_eq!(pipeline.process("Hello.".into(), &here()).await, "Hello.");
        graph.set(only("text.translate"));
        assert_eq!(pipeline.process("Hello.".into(), &here()).await, "Bonjour.");
    }

    #[test]
    fn a_requests_key_is_what_picks_its_model() {
        let key = Request::translate("en", "fr").key();
        assert_eq!(key, Request::translate("en", "fr").key());
        assert_ne!(key, Request::translate("en", "de").key());
        assert_ne!(key, Request::translate("fr", "en").key());
        assert_ne!(key, Request::cleanup(Some("en".into())).key());
        let mut quick = Request::translate("en", "fr");
        quick.run_budget = Duration::from_secs(1);
        assert_eq!(key, quick.key());
        quick.prefer = Prefer::Accuracy;
        assert_ne!(key, quick.key());
        let mut written = Request::cleanup(None);
        let plain = written.key();
        written
            .params
            .insert("instruction".into(), "be brief".into());
        assert_ne!(plain, written.key());
    }

    #[test]
    fn a_translation_names_both_languages() {
        let request = Request::translate("en", "fr");
        assert_eq!(request.task, "text.translate");
        assert_eq!(request.params["language"], "en");
        assert_eq!(request.params["to"], "fr");
        assert!(Request::cleanup(None).params.is_empty());
    }

    #[test]
    fn a_request_built_from_data_is_the_one_the_constructors_build() {
        let params = |pairs: &[(&str, &str)]| -> Params {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).into()))
                .collect()
        };
        let cleanup = Request::of("text.cleanup", params(&[("language", "en")]));
        assert_eq!(cleanup, Request::cleanup(Some("en".into())));
        assert_eq!(cleanup.run_budget, Duration::from_secs(1));
        let translate = Request::of(
            "text.translate",
            params(&[("language", "en"), ("to", "fr")]),
        );
        assert_eq!(translate, Request::translate("en", "fr"));
        assert_eq!(translate.key(), Request::translate("en", "fr").key());
        let rewrite = Request::of("text.rewrite", params(&[("style", "formal")]));
        assert_eq!(rewrite.run_budget, Duration::from_secs(10));
        assert_eq!(rewrite.prefer, Prefer::Latency);
        assert_eq!(rewrite.bind_budget, BIND_BUDGET);
    }

    const HEARD: &str = "Let's meet Monday, no, Tuesday.";

    #[tokio::test]
    async fn an_answer_made_of_words_that_were_said_is_typed() {
        let mut pipeline = cleanup(Scripted::Says("Let's meet Tuesday."));
        assert_eq!(
            pipeline.process(HEARD.into(), &here()).await,
            "Let's meet Tuesday."
        );
        let trace = traced(&pipeline);
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].task, "text.cleanup");
        assert_eq!(trace[0].model.as_deref(), Some("scripted"));
        assert_eq!(trace[0].skipped, None);
    }

    #[tokio::test]
    async fn a_missing_model_still_loses_the_stutters_and_pauses() {
        let mut pipeline = cleanup(Scripted::Fails);
        assert_eq!(
            pipeline
                .process("Um, I I think we, uh, ship.".into(), &here())
                .await,
            "I think we ship."
        );
        assert_eq!(traced(&pipeline)[0].skipped, Some(Skip::NoModel));
    }

    fn traced(pipeline: &Pipeline) -> Vec<myna_core::history::Step> {
        pipeline.trace().lock().unwrap().clone()
    }

    #[tokio::test]
    async fn an_answer_with_a_new_word_is_dropped_for_what_was_heard() {
        let mut pipeline = cleanup(Scripted::Says("Sure! Let's meet Tuesday."));
        assert_eq!(pipeline.process(HEARD.into(), &here()).await, HEARD);
        let trace = traced(&pipeline);
        assert_eq!(trace[0].model.as_deref(), Some("scripted"));
        assert_eq!(trace[0].skipped, Some(Skip::NewWord));
    }

    #[tokio::test]
    async fn a_cleanup_may_write_the_users_own_words_for_what_was_misheard() {
        let written = Live::new(Vec::new());
        let mut pipeline = cleanup(Scripted::Says("Restart MySQL.")).written(written.clone());
        assert_eq!(
            pipeline.process("restart my sequel".into(), &here()).await,
            "restart my sequel"
        );
        written.set(vec!["MySQL".into()]);
        assert_eq!(
            pipeline.process("restart my sequel".into(), &here()).await,
            "Restart MySQL."
        );
    }

    #[tokio::test]
    async fn chinese_is_written_in_the_chosen_script_in_any_graph() {
        let script = Live::new(ChineseScript::AsRecognised);
        let mut pipeline =
            Pipeline::new(Graph::passthrough(), Scripted::Fails).scripted(script.clone());
        assert_eq!(
            pipeline.process("头发 NASA".into(), &here()).await,
            "头发 NASA"
        );
        script.set(ChineseScript::Traditional);
        assert_eq!(
            pipeline.process("头发 NASA".into(), &here()).await,
            "頭髮 NASA"
        );
        let mut tidy = Pipeline::new(graph_of(ProcessingChain::TIDY), Scripted::Says("我们出发"))
            .scripted(script);
        assert_eq!(tidy.process("我们出发".into(), &here()).await, "我們出發");
    }

    #[tokio::test]
    async fn the_script_step_writes_chinese_when_on_and_leaves_english_alone() {
        use super::super::steps::{compile, Context, StepSpec};
        let typed = |steps: &[&str], language: &str| {
            let steps: Vec<StepSpec> = steps.iter().map(|s| StepSpec::parse(s)).collect();
            let compiled = compile(
                &steps,
                &Context {
                    language: Some(language),
                    ..Context::default()
                },
            );
            Pipeline::new(compiled.graph, Scripted::Fails).scripted(Live::new(compiled.script))
        };
        let said = "软件 NASA";
        assert_eq!(
            typed(&["script=traditional-taiwan"], "zh")
                .process(said.into(), &here())
                .await,
            "軟體 NASA"
        );
        assert_eq!(
            typed(&[], "zh").process(said.into(), &here()).await,
            said,
            "off"
        );
        assert_eq!(
            typed(&["script=traditional-taiwan"], "en")
                .process(said.into(), &here())
                .await,
            said,
            "not applicable to English"
        );
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
            pipeline.process(said.into(), &here()).await,
            "I run MySQL",
            "the model echoing its input passes the check with the term"
        );
        assert_eq!(*seen.lock().unwrap(), "I run MySQL");
        written.set(vec!["Postgres".to_owned()]);
        assert_eq!(pipeline.process(said.into(), &here()).await, said);
    }

    /// The graph is read as a dictation starts: a chain switched while it
    /// runs applies from the next one.
    #[tokio::test]
    async fn a_chain_switched_mid_dictation_applies_from_the_next() {
        let graph = Live::new(Graph::passthrough());
        let mut pipeline = Pipeline::new(graph.clone(), Scripted::Says("As heard."));
        pipeline.prepare();
        graph.set(graph_of(ProcessingChain::TIDY));
        assert_eq!(
            pipeline.process("as heard".into(), &here()).await,
            "as heard"
        );
        pipeline.prepare();
        assert_eq!(
            pipeline.process("as heard".into(), &here()).await,
            "As heard."
        );
    }

    #[tokio::test]
    async fn a_term_keeps_its_capitals_when_fitted() {
        let mut pipeline = Pipeline::new(Graph::passthrough(), Scripted::Fails)
            .written(Live::new(vec!["my sequel => MySQL".to_owned()]));
        let mid = Field::after(FieldPurpose::Plain, Some('a'));
        assert_eq!(
            pipeline.fit("The my sequel dump.".into(), &mid),
            "the MySQL dump."
        );
        let email = Field::after(FieldPurpose::Email, None);
        assert_eq!(pipeline.fit("My sequel.".into(), &email), "my sequel");
    }

    /// Fitting is the last change: a step that recases after the words
    /// before it cannot undo it.
    #[tokio::test]
    async fn fitting_runs_after_the_graph() {
        let mut pipeline = cleanup(Scripted::Says("The end."));
        let mid = Field::after(FieldPurpose::Plain, Some('s'));
        let processed = pipeline.process("the end".into(), &mid).await;
        assert_eq!(processed, "The end.", "the cleanup recased it");
        assert_eq!(pipeline.fit(processed, &mid), "the end.");
    }

    #[tokio::test]
    async fn an_empty_answer_is_dropped_for_what_was_heard() {
        let mut pipeline = cleanup(Scripted::Says("  "));
        assert_eq!(pipeline.process(HEARD.into(), &here()).await, HEARD);
    }

    #[tokio::test]
    async fn a_missing_model_types_what_was_heard() {
        let mut pipeline = cleanup(Scripted::Fails);
        assert_eq!(pipeline.process(HEARD.into(), &here()).await, HEARD);
        let trace = traced(&pipeline);
        assert_eq!(trace[0].model, None);
        assert_eq!(trace[0].skipped, Some(Skip::NoModel));
    }

    #[tokio::test]
    async fn a_refusal_is_recorded_as_a_code_without_the_far_sides_words() {
        let mut pipeline = cleanup(Scripted::Refuses);
        assert_eq!(pipeline.process(HEARD.into(), &here()).await, HEARD);
        assert_eq!(traced(&pipeline)[0].skipped, Some(Skip::Refused));
    }

    #[tokio::test]
    async fn a_model_that_reports_its_own_timeout_is_recorded_as_one() {
        let mut pipeline = cleanup(Scripted::TimesOut);
        assert_eq!(pipeline.process(HEARD.into(), &here()).await, HEARD);
        assert_eq!(traced(&pipeline)[0].skipped, Some(Skip::Timeout));
    }

    #[tokio::test(start_paused = true)]
    async fn a_model_over_its_budget_types_what_was_heard() {
        let mut pipeline = cleanup(Scripted::Hangs);
        assert_eq!(pipeline.process(HEARD.into(), &here()).await, HEARD);
        assert_eq!(traced(&pipeline)[0].skipped, Some(Skip::Timeout));
        // The trace is of the last text alone.
        assert_eq!(pipeline.process(HEARD.into(), &here()).await, HEARD);
        assert_eq!(traced(&pipeline).len(), 1);
    }

    #[tokio::test]
    async fn model_answers_fold_control_characters_into_single_spaces() {
        let mut translate = Pipeline::new(
            only("text.translate"),
            Scripted::Says("rm -rf /tmp/x\nsudo reboot"),
        );
        assert_eq!(
            translate.process("delete x".into(), &here()).await,
            "rm -rf /tmp/x sudo reboot"
        );

        let mut tidy = cleanup(Scripted::Says("Let's meet\r\n  Tuesday.\x1b\n"));
        assert_eq!(
            tidy.process(HEARD.into(), &here()).await,
            "Let's meet Tuesday."
        );

        let mut control_only = cleanup(Scripted::Says("\n\r\t\x1b"));
        assert_eq!(control_only.process(HEARD.into(), &here()).await, HEARD);
    }

    #[tokio::test]
    async fn a_three_step_list_still_folds_and_checks_every_answer() {
        use crate::process::steps::{compile, Context, StepSpec};
        let steps = ["fit", "tidy", "translate=fr"].map(StepSpec::parse);
        let graph = || compile(&steps, &Context::default()).graph;

        let mut folded = Pipeline::new(graph(), Scripted::Says("rm x\nsudo y"));
        assert_eq!(folded.process(HEARD.into(), &here()).await, "rm x sudo y");

        let mut refused = Pipeline::new(graph(), Scripted::Refuses);
        let typed = refused.process("Um, let's meet.".into(), &here()).await;
        assert_eq!(
            typed, "Let's meet.",
            "the rules ran, the models failed open"
        );
        assert_eq!(traced(&refused).len(), 2);

        let mut chatty = Pipeline::new(graph(), Scripted::Says("\n\r"));
        assert!(!chatty.process(HEARD.into(), &here()).await.is_empty());
    }

    #[tokio::test]
    async fn fitting_types_what_the_field_calls_for_and_asks_no_model() {
        let mut pipeline = Pipeline::new(Graph::passthrough(), Scripted::Fails);
        let email = Field::after(FieldPurpose::Email, None);
        assert_eq!(pipeline.fit("Maria.".into(), &email), "maria");
        let go_on = Field::after(FieldPurpose::Plain, Some('s'));
        assert_eq!(pipeline.fit("The end.".into(), &go_on), "the end.");
        let fresh = Field::after(FieldPurpose::Plain, Some('.'));
        assert_eq!(pipeline.fit("The end.".into(), &fresh), "The end.");
        assert!(traced(&pipeline).is_empty());
    }
}
