//! Steps run in order on text bound for the field.
//!
//! A step that needs a model names a [`Request`], what it needs done, and
//! never a model. [`Models`] answers the request with whatever fits.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::Processor;
use crate::live::Live;

/// What a step asks of a model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Request {
    /// What to do with the text: `text.cleanup`, `text.translate`.
    pub task: String,
    /// The language of the text, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// The language to answer in, for a translation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// How long the answer may take before the step is skipped.
    #[serde(skip)]
    pub budget: Duration,
}

impl Request {
    pub fn cleanup(language: Option<String>) -> Self {
        Self {
            task: "text.cleanup".into(),
            language,
            to: None,
            budget: Duration::from_secs(1),
        }
    }

    /// A dictation typed at once is translated at once, so the budget is a
    /// long dictation's: a sentence takes a model 60-260 ms.
    pub fn translate(from: &str, to: &str) -> Self {
        Self {
            task: "text.translate".into(),
            language: Some(from.into()),
            to: Some(to.into()),
            budget: Duration::from_secs(10),
        }
    }
}

/// Why a model gave no usable answer. Carries no text.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("no model: {0}")]
    Unavailable(String),
    #[error("the model refused: {0}")]
    Refused(String),
}

/// Whatever answers a [`Request`]: the place models are found and chosen.
#[async_trait]
pub trait Models: Send {
    async fn run(&mut self, request: &Request, text: &str) -> Result<String, ModelError>;
}

/// What an answer must satisfy to be typed in place of what was heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// Not empty.
    Present,
    /// Only words that were said: a cleanup may drop, recase and
    /// repunctuate, never add.
    NoNewWords,
}

impl Check {
    fn passes(self, heard: &str, answer: &str) -> bool {
        if answer.trim().is_empty() {
            return false;
        }
        match self {
            Check::Present => true,
            Check::NoNewWords => {
                let said: HashSet<String> = words(heard).collect();
                words(answer).all(|word| said.contains(&word))
            }
        }
    }
}

/// The words of `text`, lowercased and stripped of punctuation.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
}

#[derive(Clone)]
pub enum Step {
    /// A change that needs no model.
    Rule(fn(&str) -> String),
    /// A change a model makes, typed only if it passes `check`.
    Ask { request: Request, check: Check },
}

/// A chain of steps the user turns on or off as one. They run in the order
/// listed here, whatever order they were turned on in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chain {
    /// Drop hesitations, false starts and repeats.
    Tidy,
    /// Type in French what was said in English.
    French,
}

impl Chain {
    pub fn steps(self) -> Vec<Step> {
        match self {
            Chain::Tidy => vec![Step::Ask {
                request: Request::cleanup(None),
                check: Check::NoNewWords,
            }],
            Chain::French => vec![Step::Ask {
                request: Request::translate("en", "fr"),
                check: Check::Present,
            }],
        }
    }
}

/// The steps of `chains`, in the order they run.
pub fn steps_of(chains: &[Chain]) -> Vec<Step> {
    [Chain::Tidy, Chain::French]
        .into_iter()
        .filter(|chain| chains.contains(chain))
        .flat_map(Chain::steps)
        .collect()
}

/// The steps between the speech model and the field, read anew for each
/// piece of text, so a change in Settings applies from the next one.
pub struct Pipeline {
    steps: Live<Vec<Step>>,
    models: Box<dyn Models>,
}

impl Pipeline {
    pub fn new(steps: impl Into<Live<Vec<Step>>>, models: impl Models + 'static) -> Self {
        Self {
            steps: steps.into(),
            models: Box::new(models),
        }
    }
}

#[async_trait]
impl Processor for Pipeline {
    async fn process(&mut self, mut text: String) -> String {
        for step in &self.steps.get() {
            match step {
                Step::Rule(rule) => text = rule(&text),
                Step::Ask { request, check } => {
                    let answer =
                        tokio::time::timeout(request.budget, self.models.run(request, &text)).await;
                    match answer {
                        Ok(Ok(answer)) if check.passes(&text, &answer) => text = answer,
                        Ok(Ok(_)) => {
                            myna_core::info_log!(
                                "process",
                                "{} skipped: the answer failed {check:?}",
                                request.task
                            );
                        }
                        Ok(Err(err)) => {
                            myna_core::info_log!("process", "{} skipped: {err}", request.task);
                        }
                        Err(_) => {
                            myna_core::info_log!(
                                "process",
                                "{} skipped: no answer within {:?}",
                                request.task,
                                request.budget
                            );
                        }
                    }
                }
            }
        }
        text
    }
}

/// Models behind a Unix socket: one JSON line out (the request and the
/// text), one back (`text` or `error`). The far side chooses the model.
pub struct SocketModels {
    path: PathBuf,
}

impl SocketModels {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[derive(Serialize)]
struct Ask<'a> {
    #[serde(flatten)]
    request: &'a Request,
    text: &'a str,
}

#[derive(Deserialize)]
struct Answer {
    text: Option<String>,
    error: Option<String>,
}

#[async_trait]
impl Models for SocketModels {
    async fn run(&mut self, request: &Request, text: &str) -> Result<String, ModelError> {
        let unavailable = |e: std::io::Error| ModelError::Unavailable(e.to_string());
        let stream = UnixStream::connect(&self.path).await.map_err(unavailable)?;
        let (read, mut write) = stream.into_split();
        let mut line = serde_json::to_string(&Ask { request, text })
            .map_err(|e| ModelError::Unavailable(e.to_string()))?;
        line.push('\n');
        write
            .write_all(line.as_bytes())
            .await
            .map_err(unavailable)?;
        let mut reply = String::new();
        BufReader::new(read)
            .read_line(&mut reply)
            .await
            .map_err(unavailable)?;
        let answer: Answer = serde_json::from_str(&reply)
            .map_err(|_| ModelError::Refused("unreadable reply".into()))?;
        match (answer.text, answer.error) {
            (Some(text), _) => Ok(text),
            (None, error) => Err(ModelError::Refused(error.unwrap_or_default())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    /// Answers every request the same way, or never.
    enum Scripted {
        Says(&'static str),
        Fails,
        Hangs,
    }

    #[async_trait]
    impl Models for Scripted {
        async fn run(&mut self, _: &Request, _: &str) -> Result<String, ModelError> {
            match self {
                Scripted::Says(answer) => Ok(answer.to_string()),
                Scripted::Fails => Err(ModelError::Unavailable("none installed".into())),
                Scripted::Hangs => std::future::pending().await,
            }
        }
    }

    fn cleanup(models: Scripted) -> Pipeline {
        Pipeline::new(steps_of(&[Chain::Tidy]), models)
    }

    fn tasks(steps: &[Step]) -> Vec<&str> {
        steps
            .iter()
            .map(|step| match step {
                Step::Rule(_) => "rule",
                Step::Ask { request, .. } => request.task.as_str(),
            })
            .collect()
    }

    #[test]
    fn chains_run_in_their_own_order_not_the_order_given() {
        let steps = steps_of(&[Chain::French, Chain::Tidy]);
        assert_eq!(tasks(&steps), ["text.cleanup", "text.translate"]);
        assert_eq!(tasks(&steps_of(&[Chain::French])), ["text.translate"]);
        assert!(steps_of(&[]).is_empty());
    }

    #[tokio::test]
    async fn steps_changed_while_running_apply_to_the_next_text() {
        let steps = Live::new(Vec::new());
        let mut pipeline = Pipeline::new(steps.clone(), Scripted::Says("Bonjour."));
        assert_eq!(pipeline.process("Hello.".into()).await, "Hello.");
        steps.set(steps_of(&[Chain::French]));
        assert_eq!(pipeline.process("Hello.".into()).await, "Bonjour.");
    }

    #[test]
    fn a_translation_names_both_languages() {
        let request = Request::translate("en", "fr");
        let sent = serde_json::to_value(Ask {
            request: &request,
            text: "Hello.",
        })
        .unwrap();
        assert_eq!(
            sent,
            serde_json::json!({
                "task": "text.translate", "language": "en", "to": "fr", "text": "Hello."
            })
        );
    }

    const HEARD: &str = "Um, let's meet Monday, no, Tuesday.";

    #[tokio::test]
    async fn an_answer_made_of_words_that_were_said_is_typed() {
        let mut pipeline = cleanup(Scripted::Says("Let's meet Tuesday."));
        assert_eq!(pipeline.process(HEARD.into()).await, "Let's meet Tuesday.");
    }

    #[tokio::test]
    async fn an_answer_with_a_new_word_is_dropped_for_what_was_heard() {
        let mut pipeline = cleanup(Scripted::Says("Sure! Let's meet Tuesday."));
        assert_eq!(pipeline.process(HEARD.into()).await, HEARD);
    }

    #[tokio::test]
    async fn an_empty_answer_is_dropped_for_what_was_heard() {
        let mut pipeline = cleanup(Scripted::Says("  "));
        assert_eq!(pipeline.process(HEARD.into()).await, HEARD);
    }

    #[tokio::test]
    async fn a_missing_model_types_what_was_heard() {
        let mut pipeline = cleanup(Scripted::Fails);
        assert_eq!(pipeline.process(HEARD.into()).await, HEARD);
    }

    #[tokio::test(start_paused = true)]
    async fn a_model_over_its_budget_types_what_was_heard() {
        let mut pipeline = cleanup(Scripted::Hangs);
        assert_eq!(pipeline.process(HEARD.into()).await, HEARD);
    }

    #[tokio::test]
    async fn rules_and_models_run_in_order() {
        let mut pipeline = Pipeline::new(
            vec![
                Step::Rule(|text| text.replace("Monday, no, ", "")),
                Step::Ask {
                    request: Request::cleanup(None),
                    check: Check::Present,
                },
                Step::Rule(|text| text.to_uppercase()),
            ],
            Scripted::Says("done"),
        );
        assert_eq!(pipeline.process(HEARD.into()).await, "DONE");
    }

    #[tokio::test]
    async fn socket_models_send_the_request_and_read_the_answer() {
        let dir = std::env::temp_dir().join(format!("myna-models-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let mut seen = Vec::new();
            for reply in ["{\"text\":\"Hi.\"}\n", "{\"error\":\"no model fits\"}\n"] {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                BufReader::new(read).read_line(&mut line).await.unwrap();
                seen.push(line);
                write.write_all(reply.as_bytes()).await.unwrap();
            }
            seen
        });

        let mut models = SocketModels::new(&path);
        let request = Request::cleanup(Some("en".into()));
        assert_eq!(models.run(&request, "um hi").await.unwrap(), "Hi.");
        assert!(matches!(
            models.run(&request, "um hi").await,
            Err(ModelError::Refused(reason)) if reason == "no model fits"
        ));
        let seen = server.await.unwrap();
        let sent: serde_json::Value = serde_json::from_str(&seen[0]).unwrap();
        assert_eq!(
            sent,
            serde_json::json!({"task": "text.cleanup", "language": "en", "text": "um hi"})
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn socket_models_with_nothing_listening_are_unavailable() {
        let mut models = SocketModels::new("/nonexistent/myna-models");
        assert!(matches!(
            models.run(&Request::cleanup(None), "hi").await,
            Err(ModelError::Unavailable(_))
        ));
    }
}
