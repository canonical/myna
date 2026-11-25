//! Replays one utterance of speech through a graph the way the daemon feeds
//! it, and times where the wait after the user stops goes.
//!
//! Speech runs once and is recorded as events with their times relative to
//! the stop edge (negative: before it). Each graph then replays those events
//! through the daemon's own pieces: [`CommitPolicy`] decides which text is
//! settled at each event, [`Pipeline`] processes it, and the field is
//! derived from the last typed character as the controller does. Processing
//! is serial on a virtual clock, so a piece that is still running when the
//! next event arrives delays it exactly as it would live.

use std::time::Instant;

use myna_desktop::draft::{append, sentence_ends, CommitPolicy};
use myna_desktop::inject::{fold_control_chars, FieldPurpose};
use myna_desktop::process::pipeline::Pipeline;
use myna_desktop::process::{Field, Processor};
use myna_orchestrator::fsm::OrchestratorEvent;

/// How far apart `--text` sentences are taken to have been spoken.
const SENTENCE_GAP_MS: f64 = 3000.0;

#[derive(Clone, Debug, PartialEq)]
pub enum Ev {
    /// Committed text.
    Final(String),
    /// Any other event: the controller flushes what is settled at each.
    Boundary,
}

/// One utterance as the speech model delivered it.
#[derive(Clone, Debug, Default)]
pub struct Speech {
    pub id: String,
    pub reference: Option<String>,
    /// `(ms relative to the stop edge, event)`.
    pub events: Vec<(f64, Ev)>,
}

impl Speech {
    /// What was heard, as the controller would hold it.
    pub fn heard(&self) -> String {
        let mut held = String::new();
        for (_, ev) in &self.events {
            if let Ev::Final(text) = ev {
                append(&mut held, text);
            }
        }
        held
    }

    /// Events recorded off a live session. `stop` is when audio ended; when
    /// it never did, the last event stands in.
    pub fn recorded(
        id: String,
        reference: Option<String>,
        recorded: &[(Instant, OrchestratorEvent)],
        stop: Option<Instant>,
    ) -> Self {
        let events = recorded;
        let base = stop.or_else(|| events.last().map(|e| e.0));
        let ms = |at: Instant| match base {
            Some(base) if at >= base => at.duration_since(base).as_secs_f64() * 1e3,
            Some(base) => -(base.duration_since(at).as_secs_f64() * 1e3),
            None => 0.0,
        };
        let mut events: Vec<(f64, Ev)> = events
            .iter()
            .map(|(at, event)| {
                let ev = match event {
                    OrchestratorEvent::Final(text) => Ev::Final(text.clone()),
                    _ => Ev::Boundary,
                };
                (ms(*at), ev)
            })
            .collect();
        // A batch backend may say everything in `Done` alone.
        if !events.iter().any(|(_, ev)| matches!(ev, Ev::Final(_))) {
            let said = done_text(recorded);
            if let (Some(text), Some(last)) = (said, events.last().map(|e| e.0)) {
                events.insert(events.len() - 1, (last, Ev::Final(text)));
            }
        }
        Self {
            id,
            reference,
            events,
        }
    }

    /// Text typed instead of spoken: a sentence at a time, the last at the
    /// stop edge, so nothing waits on speech.
    pub fn typed(id: String, text: &str) -> Self {
        let mut sentences = Vec::new();
        let mut from = 0;
        for end in sentence_ends(text) {
            sentences.push(text[from..end].trim());
            from = end;
        }
        sentences.push(text[from..].trim());
        sentences.retain(|s| !s.is_empty());
        let last = sentences.len().saturating_sub(1);
        let mut events = Vec::new();
        for (i, sentence) in sentences.iter().enumerate() {
            let at = -((last - i) as f64) * SENTENCE_GAP_MS;
            events.push((at, Ev::Final((*sentence).to_owned())));
            events.push((at, Ev::Boundary));
        }
        events.push((0.0, Ev::Boundary));
        Self {
            id,
            reference: None,
            events,
        }
    }

    /// When the last event came, which is when the speech model finished.
    fn end_ms(&self) -> f64 {
        self.events.last().map_or(0.0, |e| e.0)
    }
}

fn done_text(events: &[(Instant, OrchestratorEvent)]) -> Option<String> {
    events.iter().rev().find_map(|(_, e)| match e {
        OrchestratorEvent::Done(text) if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    })
}

/// The field the text is going to.
#[derive(Clone, Copy, Debug)]
pub struct FieldSpec {
    pub purpose: FieldPurpose,
    pub mid_sentence: bool,
}

/// What one step did, summed over the pieces of an utterance.
#[derive(Clone, Debug, PartialEq)]
pub struct StepSum {
    pub task: String,
    pub model: Option<String>,
    pub ms: u64,
    pub runs: usize,
    /// Why runs were skipped, a code each.
    pub skipped: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Piece {
    pub heard: String,
    pub typed: String,
    /// When processing started, relative to the stop edge.
    pub start_ms: f64,
    pub ms: f64,
}

/// One graph's run over one utterance.
#[derive(Clone, Debug)]
pub struct Run {
    pub typed: String,
    pub pieces: Vec<Piece>,
    pub steps: Vec<StepSum>,
    /// Stop edge to the speech model finishing.
    pub asr_tail_ms: f64,
    /// The rest of the wait: the graph's work still queued or running.
    pub graph_ms: f64,
    /// Stop edge to the text being ready to type.
    pub dead_air_ms: f64,
}

fn skip_code(skip: myna_core::history::Skip) -> String {
    serde_json::to_value(skip)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "other".into())
}

fn sum(steps: &mut Vec<StepSum>, step: &myna_core::history::Step) {
    let found = steps.iter().position(|s| s.task == step.task);
    let at = found.unwrap_or_else(|| {
        steps.push(StepSum {
            task: step.task.clone(),
            model: None,
            ms: 0,
            runs: 0,
            skipped: Vec::new(),
        });
        steps.len() - 1
    });
    let sum = &mut steps[at];
    sum.ms += step.ms;
    sum.runs += 1;
    if step.model.is_some() {
        sum.model.clone_from(&step.model);
    }
    if let Some(skip) = step.skipped {
        sum.skipped.push(skip_code(skip));
    }
}

/// Replays `speech` through `pipeline`, held back by `policy`.
#[allow(unused_assignments)] // the last flush leaves its bookkeeping unread
pub async fn replay(
    pipeline: &mut Pipeline,
    policy: CommitPolicy,
    field: FieldSpec,
    speech: &Speech,
) -> Run {
    let trace = pipeline.trace();
    let mut pending = String::new();
    let mut typed = String::new();
    let mut last: Option<char> = None;
    let mut any = false;
    let mut clock = f64::NEG_INFINITY;
    let mut pieces = Vec::new();
    let mut steps = Vec::new();

    // `ready` bytes of `pending` are settled.
    macro_rules! flush {
        ($ready:expr) => {{
            let ready: usize = $ready;
            if ready > 0 && !pending.is_empty() {
                let rest = pending.split_off(ready);
                let heard = std::mem::replace(&mut pending, rest);
                let before = if any {
                    last
                } else {
                    field.mid_sentence.then_some('a')
                };
                let started = Instant::now();
                let start_ms = clock;
                let at = Field::after(field.purpose, before);
                let out = pipeline.process(heard.clone()).await;
                let mut out = pipeline.fit(out, &at);
                let ms = started.elapsed().as_secs_f64() * 1e3;
                if field.purpose == FieldPurpose::Terminal {
                    out = fold_control_chars(&out);
                }
                clock += ms;
                for step in trace.lock().unwrap_or_else(|p| p.into_inner()).iter() {
                    sum(&mut steps, step);
                }
                if !out.is_empty() {
                    if !typed.is_empty() && !out.starts_with(char::is_whitespace) {
                        typed.push(' ');
                    }
                    typed.push_str(&out);
                    last = out.trim_end().chars().last().or(last);
                    any = true;
                }
                pieces.push(Piece {
                    heard,
                    typed: out,
                    start_ms,
                    ms,
                });
            }
        }};
    }

    for (at, ev) in &speech.events {
        clock = clock.max(*at);
        match ev {
            Ev::Final(text) => append(&mut pending, text),
            Ev::Boundary => flush!(policy.ready(&pending, false)),
        }
    }
    clock = clock.max(speech.end_ms());
    flush!(pending.len());

    let asr_tail_ms = speech.end_ms().max(0.0);
    let dead_air_ms = clock.max(0.0).max(asr_tail_ms);
    Run {
        typed,
        pieces,
        steps,
        asr_tail_ms,
        graph_ms: dead_air_ms - asr_tail_ms,
        dead_air_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use myna_desktop::process::graph::Graph;
    use myna_desktop::process::pipeline::{Answer, ModelError, Models, Request};
    use std::time::Duration;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Answers every request after `delay`, with the text it was given.
    struct Slow(Duration);

    #[async_trait]
    impl Models for Slow {
        async fn run(&mut self, _: &Request, text: &str) -> Result<Answer, ModelError> {
            tokio::time::sleep(self.0).await;
            Ok(Answer {
                text: text.to_owned(),
                model: Some("slow/echo".into()),
            })
        }
    }

    fn plain() -> FieldSpec {
        FieldSpec {
            purpose: FieldPurpose::Plain,
            mid_sentence: false,
        }
    }

    fn tidy(delay: u64) -> (Pipeline, CommitPolicy) {
        let spec = crate::spec::parse("tidy").unwrap();
        let policy = CommitPolicy::for_chain(spec.graph.touches());
        (Pipeline::new(spec.graph, Slow(ms(delay))), policy)
    }

    #[test]
    fn typed_text_is_a_sentence_at_a_time_the_last_at_the_stop() {
        let speech = Speech::typed("t".into(), "One. Two. Three");
        let finals: Vec<_> = speech
            .events
            .iter()
            .filter_map(|(at, ev)| match ev {
                Ev::Final(t) => Some((*at, t.as_str())),
                Ev::Boundary => None,
            })
            .collect();
        assert_eq!(
            finals,
            [(-6000.0, "One."), (-3000.0, "Two."), (0.0, "Three")]
        );
        assert_eq!(speech.heard(), "One. Two. Three");
        assert!(Speech::typed("t".into(), "  ").events.len() == 1);
    }

    #[test]
    fn recorded_events_are_timed_from_the_stop_edge() {
        let base = Instant::now();
        let at = |n: u64| base + ms(n);
        let events = [
            (at(100), OrchestratorEvent::Final("a".into())),
            (at(500), OrchestratorEvent::Transcribing),
            (at(900), OrchestratorEvent::Done("a".into())),
        ];
        let speech = Speech::recorded("r".into(), None, &events, Some(at(600)));
        let times: Vec<f64> = speech.events.iter().map(|e| e.0.round()).collect();
        assert_eq!(times, [-500.0, -100.0, 300.0]);
        assert_eq!(speech.events[1].1, Ev::Boundary);
        let unstamped = Speech::recorded("r".into(), None, &events, None);
        assert_eq!(unstamped.end_ms(), 0.0);
    }

    #[test]
    fn a_backend_that_only_says_done_is_heard_as_one_piece() {
        let base = Instant::now();
        let events = [
            (base, OrchestratorEvent::Transcribing),
            (base + ms(300), OrchestratorEvent::Done("all of it".into())),
        ];
        let speech = Speech::recorded("r".into(), None, &events, Some(base));
        assert_eq!(speech.heard(), "all of it");
        let none = [(base, OrchestratorEvent::Done("  ".into()))];
        assert_eq!(Speech::recorded("r".into(), None, &none, None).heard(), "");
    }

    #[tokio::test]
    async fn exact_waits_only_for_the_speech_model() {
        let mut exact = Pipeline::new(Graph::passthrough(), Slow(ms(0)));
        let mut speech = Speech::typed("t".into(), "One. Two.");
        speech.events.last_mut().unwrap().0 = 400.0;
        let run = replay(&mut exact, CommitPolicy::Streaming, plain(), &speech).await;
        assert_eq!(run.typed, "One. Two.");
        assert_eq!(run.asr_tail_ms, 400.0);
        assert!(run.graph_ms < 50.0, "{}", run.graph_ms);
        assert_eq!(run.dead_air_ms, run.asr_tail_ms + run.graph_ms);
    }

    #[tokio::test]
    async fn a_held_sentence_is_processed_before_the_stop_and_only_the_last_waits() {
        let (mut pipeline, policy) = tidy(60);
        let speech = Speech::typed("t".into(), "One. Two. Three. Four");
        let run = replay(&mut pipeline, policy, plain(), &speech).await;
        assert_eq!(run.typed, "One. Two. Three. Four");
        // Horizon of one: the early sentences ran while speaking, so the
        // wait is what is left, not all four.
        assert_eq!(run.pieces.iter().filter(|p| p.start_ms < 0.0).count(), 1);
        assert!(run.graph_ms >= 60.0, "{}", run.graph_ms);
        assert!(run.graph_ms < 4.0 * 60.0 + 100.0, "{}", run.graph_ms);
        assert_eq!(run.steps.len(), 1);
        assert_eq!(run.steps[0].task, "text.cleanup");
        assert_eq!(run.steps[0].runs, run.pieces.len());
        assert_eq!(run.steps[0].model.as_deref(), Some("slow/echo"));
    }

    #[tokio::test]
    async fn work_still_queued_at_the_stop_is_dead_air() {
        let (mut pipeline, policy) = tidy(80);
        // Everything arrives at the stop edge: nothing could be done earlier.
        let mut speech = Speech::typed("t".into(), "One. Two. Three. Four");
        for event in &mut speech.events {
            event.0 = 0.0;
        }
        let run = replay(&mut pipeline, policy, plain(), &speech).await;
        assert!(run.graph_ms >= 2.0 * 80.0, "{}", run.graph_ms);
        assert_eq!(run.asr_tail_ms, 0.0);
    }

    #[tokio::test]
    async fn the_field_follows_what_was_typed() {
        let spec = crate::spec::parse("exact").unwrap();
        let policy = CommitPolicy::for_chain(spec.graph.touches());
        let mut pipeline = Pipeline::new(spec.graph, Slow(ms(0)));
        let speech = Speech::typed("t".into(), "The end.");
        let mid = FieldSpec {
            mid_sentence: true,
            ..plain()
        };
        assert_eq!(
            replay(&mut pipeline, policy, mid, &speech).await.typed,
            "the end."
        );
        assert_eq!(
            replay(&mut pipeline, policy, plain(), &speech).await.typed,
            "The end."
        );
        let email = FieldSpec {
            purpose: FieldPurpose::Email,
            ..plain()
        };
        assert_eq!(
            replay(&mut pipeline, policy, email, &speech).await.typed,
            "the end"
        );
    }

    #[tokio::test]
    async fn a_terminal_gets_no_control_characters() {
        struct Lines;
        #[async_trait]
        impl Models for Lines {
            async fn run(&mut self, _: &Request, _: &str) -> Result<Answer, ModelError> {
                Ok(Answer {
                    text: "a\nb".into(),
                    model: None,
                })
            }
        }
        let spec = crate::spec::parse("translate=fr").unwrap();
        let mut pipeline = Pipeline::new(spec.graph, Lines);
        let terminal = FieldSpec {
            purpose: FieldPurpose::Terminal,
            ..plain()
        };
        let run = replay(
            &mut pipeline,
            CommitPolicy::Streaming,
            terminal,
            &Speech::typed("t".into(), "ab"),
        )
        .await;
        assert_eq!(run.typed, "a b");
    }

    #[tokio::test]
    async fn a_skipped_step_is_counted_with_its_code() {
        struct Chatty;
        #[async_trait]
        impl Models for Chatty {
            async fn run(&mut self, _: &Request, _: &str) -> Result<Answer, ModelError> {
                Ok(Answer {
                    text: "Sure! something else".into(),
                    model: None,
                })
            }
        }
        let spec = crate::spec::parse("cleanup").unwrap();
        let mut pipeline = Pipeline::new(spec.graph, Chatty);
        let run = replay(
            &mut pipeline,
            CommitPolicy::Streaming,
            plain(),
            &Speech::typed("t".into(), "hello there"),
        )
        .await;
        assert_eq!(run.typed, "hello there");
        assert_eq!(run.steps[0].skipped, ["new-word"]);
    }
}
