//! The lab's session: speech is recorded once, then every graph replays it.

use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use myna_core::ChineseScript;
use myna_desktop::draft::CommitPolicy;
use myna_desktop::inject::FieldPurpose;
use myna_desktop::live::Live;
use myna_desktop::process::graph::Needs;
use myna_desktop::process::pipeline::{Pipeline, Request};
use myna_desktop::process::resolve::Resolver;
use myna_orchestrator::fsm::OrchestratorEvent;
use myna_orchestrator::{AudioSource, CaptureStream, TextSink};
use serde_json::{json, Value};

use crate::lab::{replay, FieldSpec, Run, Speech};
use crate::report::{self, Summary};
use crate::spec::Spec;

/// What a lab run is told by the command line.
pub struct Config {
    pub specs: Vec<Spec>,
    pub purpose: FieldPurpose,
    pub mid_sentence: bool,
    pub vocabulary: Vec<String>,
    pub language: Option<String>,
    pub share_dirs: Vec<PathBuf>,
    pub json: Option<PathBuf>,
}

struct Entry {
    label: String,
    pipeline: Pipeline,
    policy: CommitPolicy,
}

pub struct Lab {
    entries: Vec<Entry>,
    resolver: Resolver,
    requests: Vec<Request>,
    field: FieldSpec,
    summary: Summary,
    json: Vec<Value>,
    warm: Vec<Value>,
    json_path: Option<PathBuf>,
    share_dirs: Vec<PathBuf>,
    color: bool,
    total: usize,
    seen: usize,
}

impl Lab {
    pub fn new(config: Config, total: usize) -> Self {
        let resolver = Resolver::new(config.share_dirs.clone());
        let script_applies =
            |script| myna_desktop::process::script::applies(script, config.language.as_deref());
        let mut requests: Vec<Request> = Vec::new();
        let entries = config
            .specs
            .iter()
            .map(|spec| {
                let script = if script_applies(spec.script) {
                    spec.script
                } else {
                    ChineseScript::AsRecognised
                };
                // As the daemon: conversion reads a sentence at a time.
                let needs = spec
                    .graph
                    .needs()
                    .max((script != ChineseScript::AsRecognised).then_some(Needs::Sentence));
                for request in spec.graph.requests() {
                    if !requests.iter().any(|r| r.key() == request.key()) {
                        requests.push(request);
                    }
                }
                Entry {
                    label: spec.label.clone(),
                    pipeline: Pipeline::new(spec.graph.clone(), resolver.clone())
                        .written(Live::new(config.vocabulary.clone()))
                        .scripted(Live::new(script)),
                    policy: CommitPolicy::for_needs(needs),
                }
            })
            .collect();
        Self {
            entries,
            resolver,
            requests,
            field: FieldSpec {
                purpose: config.purpose,
                mid_sentence: config.mid_sentence,
            },
            summary: Summary::default(),
            json: Vec::new(),
            warm: Vec::new(),
            json_path: config.json,
            share_dirs: config.share_dirs,
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
            total,
            seen: 0,
        }
    }

    /// Binds every text model the graphs ask for, before the first
    /// utterance, and says what it cost apart from the utterances.
    pub async fn warm(&mut self) {
        if self.requests.is_empty() {
            return;
        }
        println!("warm (text models bound before the first utterance)");
        for request in &self.requests {
            let started = Instant::now();
            self.resolver.settle(std::slice::from_ref(request)).await;
            let ms = started.elapsed().as_millis();
            let bound = self.resolver.bound_to(request);
            let what = describe(request);
            match &bound {
                Some(model) => println!("   {what:<34} {model}  {ms} ms"),
                None => {
                    println!("   {what:<34} NOT BOUND: no connected text share covers it ({ms} ms)")
                }
            }
            self.warm
                .push(json!({"request": what, "model": bound, "ms": ms}));
        }
        if self.warm.iter().any(|w| w["model"].is_null()) {
            println!(
                "   (text shares searched: {}; add one with --text-share <dir>)",
                self.share_dirs
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    /// Runs every graph over `speech` and prints the block.
    pub async fn analyse(&mut self, speech: Speech) {
        self.seen += 1;
        let mut runs: Vec<(String, Run)> = Vec::new();
        for entry in &mut self.entries {
            let run = replay(&mut entry.pipeline, entry.policy, self.field, &speech).await;
            runs.push((entry.label.clone(), run));
        }
        println!(
            "{}",
            report::utterance(self.seen, self.total, &speech, &runs, self.color)
        );
        self.summary.add(&runs);
        self.json.push(report::utterance_json(&speech, &runs));
    }

    /// The summary table, and `--json`.
    pub fn finish(&self) -> Result<(), String> {
        if self.summary.is_empty() {
            return Ok(());
        }
        print!("{}", self.summary.table());
        if let Some(path) = &self.json_path {
            let doc = json!({
                "warm": self.warm,
                "utterances": self.json,
                "summary": self.summary.json(),
            });
            let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
            std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
            println!("wrote {}", path.display());
        }
        Ok(())
    }
}

fn describe(request: &Request) -> String {
    let params: Vec<String> = request
        .params
        .iter()
        .map(|(k, v)| {
            format!(
                "{k}={}",
                v.as_str().map_or_else(|| v.to_string(), str::to_owned)
            )
        })
        .collect();
    format!("{} {}", request.task, params.join(" "))
        .trim_end()
        .to_owned()
}

/// Prints what a lab run must not hide, and nothing else.
pub struct Quiet;

#[async_trait]
impl TextSink for Quiet {
    async fn emit(&mut self, event: OrchestratorEvent) {
        match event {
            OrchestratorEvent::Warning { notice, .. } => {
                eprintln!("⚠ {}: {}", notice.headline, notice.detail);
            }
            OrchestratorEvent::Error { code, failure } => {
                eprintln!("✗ {} [{code}]: {}", failure.headline, failure.detail);
            }
            _ => {}
        }
    }
}

/// Keeps every event with the moment it arrived, then passes it on.
pub struct Recording<T> {
    pub inner: T,
    pub events: Vec<(Instant, OrchestratorEvent)>,
}

impl<T> Recording<T> {
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            events: Vec::new(),
        }
    }
}

#[async_trait]
impl<T: TextSink> TextSink for Recording<T> {
    async fn emit(&mut self, event: OrchestratorEvent) {
        self.events.push((Instant::now(), event.clone()));
        self.inner.emit(event).await;
    }
}

/// When audio ended: the stop edge the wait is measured from.
#[derive(Clone, Default)]
pub struct Stop(Arc<Mutex<Option<Instant>>>);

impl Stop {
    pub fn at(&self) -> Option<Instant> {
        *self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A source that notes when its stream ends.
pub struct Stamped<S> {
    inner: S,
    stop: Stop,
}

impl<S> Stamped<S> {
    pub fn new(inner: S, stop: Stop) -> Self {
        Self { inner, stop }
    }
}

impl<S: AudioSource> AudioSource for Stamped<S> {
    fn format(&self) -> myna_core::AudioFormat {
        self.inner.format()
    }

    fn health(&self) -> myna_core::CaptureHealthStream {
        self.inner.health()
    }

    fn capture(self: Box<Self>) -> CaptureStream {
        let Stamped { inner, stop } = *self;
        let stamp = futures_util::stream::once(async move {
            *stop.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
        })
        .filter_map(|()| async { None });
        Box::pin(Box::new(inner).capture().chain(stamp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use myna_orchestrator::WavFileSource;

    #[test]
    fn a_request_is_described_by_its_task_and_params() {
        assert_eq!(
            describe(&Request::translate("en", "fr")),
            "text.translate language=en to=fr"
        );
        assert_eq!(describe(&Request::cleanup(None)), "text.cleanup");
    }

    #[tokio::test]
    async fn the_stop_edge_is_when_the_audio_ends() {
        let stop = Stop::default();
        assert!(stop.at().is_none());
        let pcm = bytes::Bytes::from(vec![0u8; 3200]);
        let wav = WavFileSource::from_pcm(myna_core::AudioFormat::default(), pcm);
        let source = Stamped::new(wav, stop.clone());
        let chunks: Vec<_> = Box::new(source).capture().collect().await;
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(Result::is_ok));
        assert!(stop.at().is_some());
    }

    #[tokio::test]
    async fn events_are_kept_with_their_arrival() {
        let mut sink = Recording::new(Quiet);
        sink.emit(OrchestratorEvent::Final("a".into())).await;
        sink.emit(OrchestratorEvent::Done("a".into())).await;
        assert_eq!(sink.events.len(), 2);
        assert!(sink.events[0].0 <= sink.events[1].0);
    }

    #[tokio::test]
    async fn a_lab_replays_every_graph_and_reports_the_summary() {
        let config = Config {
            specs: ["exact", "disfluency"]
                .map(|s| crate::spec::parse(s).unwrap())
                .to_vec(),
            purpose: FieldPurpose::Plain,
            mid_sentence: false,
            vocabulary: Vec::new(),
            language: None,
            share_dirs: Vec::new(),
            json: Some(std::env::temp_dir().join(format!("lab-{}.json", std::process::id()))),
        };
        let path = config.json.clone().unwrap();
        let mut lab = Lab::new(config, 1);
        lab.warm().await;
        lab.analyse(Speech::typed("t".into(), "Um, I I think so."))
            .await;
        lab.finish().unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(doc["utterances"][0]["graphs"][0]["graph"], "exact");
        assert_eq!(
            doc["utterances"][0]["graphs"][0]["text"],
            "Um, I I think so."
        );
        assert_eq!(doc["utterances"][0]["graphs"][1]["text"], "I think so.");
        assert_eq!(doc["summary"]["graphs"].as_array().unwrap().len(), 2);
    }
}
