//! A graph of steps run on text bound for the field.
//!
//! A step that needs a model names a [`Request`], what it needs done, and
//! never a model. [`Models`] answers the request with whatever fits.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use myna_core::history::Skip;

pub use super::check::Check;
use super::graph::{Graph, Id, Joins, Op, Src};
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
    fn new(task: &str, params: Params, run_budget: Duration) -> Self {
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
        Self::new("text.cleanup", params, Duration::from_secs(1))
    }

    /// A dictation typed at once is translated at once, so the budget is a
    /// long dictation's: a sentence takes a model 60-260 ms.
    pub fn translate(from: &str, to: &str) -> Self {
        let params = [("language", from), ("to", to)]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.into()))
            .collect();
        Self::new("text.translate", params, Duration::from_secs(10))
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

/// Why a model gave no usable answer. The far side's reason is kept for
/// tests and never logged or recorded: it may quote the text.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("no model: {0}")]
    Unavailable(String),
    #[error("the model refused: {0}")]
    Refused(String),
}

impl ModelError {
    fn skip(&self) -> Skip {
        match self {
            ModelError::Unavailable(_) => Skip::NoModel,
            ModelError::Refused(_) => Skip::Refused,
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
    models: Box<dyn Models>,
    written: Live<Vec<String>>,
    /// `written` compiled, until it changes.
    vocabulary: (Vec<String>, Vocabulary),
    trace: Trace,
    joins: Joins,
}

impl Pipeline {
    pub fn new(graph: impl Into<Live<Graph>>, models: impl Models + 'static) -> Self {
        Self {
            graph: graph.into(),
            models: Box::new(models),
            written: Live::new(Vec::new()),
            vocabulary: (Vec::new(), Vocabulary::default()),
            trace: Trace::default(),
            joins: Joins::default(),
        }
    }

    /// The user's vocabulary: the spelling rule applies it, and a cleanup
    /// may write it in place of what the speech model misheard.
    #[must_use]
    pub fn written(mut self, words: Live<Vec<String>>) -> Self {
        self.written = words;
        self
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
        let written = self.written.get();
        let started = Instant::now();
        let answer =
            tokio::time::timeout(request.run_budget, self.models.run(request, &text)).await;
        let mut model = None;
        let mut answer_chars = 0;
        let mut typed = None;
        let skipped = match answer {
            Ok(Ok(answer)) => {
                model = answer.model;
                let cleaned = fold_control_chars(&answer.text);
                answer_chars = cleaned.chars().count();
                match check.verdict(&text, &cleaned, &written) {
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
        typed.unwrap_or(text)
    }
}

#[async_trait]
impl Processor for Pipeline {
    fn prepare(&mut self) {
        self.models.prepare(&self.graph.get().requests());
    }

    async fn process(&mut self, text: String, field: &Field) -> String {
        let graph = self.graph.get();
        let entries = self.written.get();
        if self.vocabulary.0 != entries {
            self.vocabulary = (entries.clone(), Vocabulary::new(&entries));
        }
        let seq = self.joins.begin();
        let mut trace = Vec::new();
        let mut made: HashMap<Id, String> = HashMap::new();
        let read = |made: &HashMap<Id, String>, src: Src| match src {
            Src::Input => text.clone(),
            Src::Node(id) => made.get(&id).cloned().unwrap_or_default(),
        };
        for id in graph.plan(field) {
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
                Op::Route { .. } => op
                    .select(field)
                    .map(|src| read(&made, src))
                    .unwrap_or_default(),
                Op::Join { parts, sep } => {
                    let mut whole = None;
                    for part in parts {
                        let arrived = read(&made, *part);
                        whole = self.joins.arrive(seq, id, parts, *part, arrived);
                    }
                    whole.map(|all| all.join(sep)).unwrap_or_default()
                }
            };
            made.insert(id, out);
        }
        self.joins.forget(seq);
        *self.trace.lock().unwrap_or_else(|p| p.into_inner()) = trace;
        read(&made, graph.out())
    }
}

#[cfg(test)]
mod tests {
    use super::super::graph::{Builder, Needs};
    use super::*;
    use crate::process::rules::Rule;
    use myna_core::ProcessingChain;
    use myna_platform::text_input::FieldPurpose;

    /// Answers every request the same way, or never.
    enum Scripted {
        Says(&'static str),
        Fails,
        Refuses,
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
                Scripted::Refuses => Err(ModelError::Refused("cannot: um hi".into())),
                Scripted::Hangs => std::future::pending().await,
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
                Needs::Sentence,
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
        let written = Live::new(vec!["my sequel:MySQL".to_owned()]);
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

    #[tokio::test]
    async fn a_term_keeps_its_capitals_in_the_keyword_preset() {
        let mut pipeline = Pipeline::new(graph_of(ProcessingChain::KEYWORD), Scripted::Fails)
            .written(Live::new(vec!["my sequel:MySQL".to_owned()]));
        let mid = Field::after(FieldPurpose::Plain, Some('a'));
        assert_eq!(
            pipeline.process("The my sequel dump.".into(), &mid).await,
            "the MySQL dump."
        );
        let email = Field::after(FieldPurpose::Email, None);
        assert_eq!(
            pipeline.process("My sequel.".into(), &email).await,
            "my sequel"
        );
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
    async fn rules_and_models_run_in_order() {
        let mut g = Builder::default();
        let asked = g.ask(
            Src::Input,
            Request::cleanup(None),
            Check::Present,
            Needs::Sentence,
        );
        let out = g.rule(asked, Rule::Address);
        let mut pipeline = Pipeline::new(g.finish(out), Scripted::Says("Done."));
        assert_eq!(pipeline.process(HEARD.into(), &here()).await, "done");
    }

    #[tokio::test]
    async fn a_route_types_what_the_field_calls_for_and_asks_no_model_it_does_not_need() {
        let mut pipeline = Pipeline::new(graph_of(ProcessingChain::KEYWORD), Scripted::Fails);
        let email = Field::after(FieldPurpose::Email, None);
        assert_eq!(pipeline.process("Maria.".into(), &email).await, "maria");
        let go_on = Field::after(FieldPurpose::Plain, Some('s'));
        assert_eq!(
            pipeline.process("The end.".into(), &go_on).await,
            "the end."
        );
        let fresh = Field::after(FieldPurpose::Plain, Some('.'));
        assert_eq!(
            pipeline.process("The end.".into(), &fresh).await,
            "The end."
        );
        assert!(traced(&pipeline).is_empty());
    }

    /// The input beside its translation: one source feeding two nodes.
    fn side_by_side() -> Graph {
        let mut g = Builder::default();
        let french = g.ask(
            Src::Input,
            Request::translate("en", "fr"),
            Check::Present,
            Needs::Sentence,
        );
        let out = g.join(vec![Src::Input, french], " / ");
        g.finish(out)
    }

    #[tokio::test]
    async fn a_join_types_each_piece_beside_its_own_translation() {
        let mut pipeline = Pipeline::new(side_by_side(), Scripted::Says("Bonjour."));
        for piece in ["Hello.", "Goodbye."] {
            assert_eq!(
                pipeline.process(piece.into(), &here()).await,
                format!("{piece} / Bonjour.")
            );
        }
        assert_eq!(traced(&pipeline).len(), 1);
    }

    #[tokio::test]
    async fn a_failed_branch_of_a_join_hands_on_what_it_was_given() {
        let mut pipeline = Pipeline::new(side_by_side(), Scripted::Fails);
        assert_eq!(
            pipeline.process("Hello.".into(), &here()).await,
            "Hello. / Hello."
        );
        assert_eq!(traced(&pipeline)[0].skipped, Some(Skip::NoModel));
    }
}
