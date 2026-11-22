//! Finding the text model for a request, and keeping it bound.
//!
//! Connected text shares are enumerated, those whose declared coverage
//! excludes the request are dropped, the rest are ranked, and only the
//! winner is asked to `warm`; the runner-up is tried only if the winner
//! fails or is too slow. A bound request runs on its model with no
//! further probing. Binding happens ahead of speech ([`Resolver::settle`]),
//! and a request found unbound at speech start binds then instead of
//! never.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use myna_orchestrator::backend::share::{resolve_text_shares, TextProvider, TextShareKind};
use tokio::sync::watch;

use super::openai;
use super::pipeline::{Answer, ModelError, Models, Prefer, Request};
use super::wire::{self, Capability, WireError};

/// How a share is spoken to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// `myna-text-v1`.
    Myna,
    /// OpenAI chat over the socket, prompted by a compiled recipe.
    Chat,
}

/// A model of a connected share that might serve a request.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub via: Via,
    pub socket: PathBuf,
    pub share: String,
    pub capability: Capability,
}

/// The candidates that cover `request` and fit its run budget, best first.
/// Total and deterministic: ties end on the share's name, then the model id.
pub fn rank(request: &Request, candidates: Vec<Candidate>) -> Vec<Candidate> {
    let budget_ms = u64::try_from(request.run_budget.as_millis()).unwrap_or(u64::MAX);
    let mut fits: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| c.capability.covers(&request.task, &request.params))
        .filter(|c| c.capability.latency_ms.is_none_or(|ms| ms <= budget_ms))
        .collect();
    let quality = |c: &Candidate| (c.capability.quality.unwrap_or(0.0) * 1000.0) as i64;
    let latency = |c: &Candidate| c.capability.latency_ms.unwrap_or(u64::MAX);
    fits.sort_by(|a, b| {
        match request.prefer {
            Prefer::Latency => {
                (latency(a), Reverse(quality(a))).cmp(&(latency(b), Reverse(quality(b))))
            }
            Prefer::Accuracy => {
                (Reverse(quality(a)), latency(a)).cmp(&(Reverse(quality(b)), latency(b)))
            }
        }
        .then_with(|| (&a.share, &a.capability.id).cmp(&(&b.share, &b.capability.id)))
    });
    fits
}

/// Whether the chosen graph can do everything it asks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ChainStatus {
    #[default]
    Ready,
    /// Some step has no model: the steps are skipped, the text is typed.
    Degraded {
        /// Tasks no connected share covers, sorted.
        missing_tasks: Vec<String>,
        /// A share was connected after this process started, so only a
        /// fresh daemon can see it.
        needs_restart: bool,
    },
}

/// What a chat model is warmed with: a short dictation like the real ones.
const WARM_TEXT: &str = "Um, so I I think we should meet on Tuesday, no, Wednesday.";

async fn chat(
    socket: &std::path::Path,
    model: &str,
    request: &Request,
    text: &str,
    within: Duration,
) -> Result<String, WireError> {
    let system = openai::system_prompt(&request.task, &request.params)
        .ok_or_else(|| WireError::Refused("no recipe".into()))?;
    openai::complete(socket, model, &system, text, within).await
}

#[derive(Clone)]
struct Binding {
    via: Via,
    socket: PathBuf,
    share: String,
    model: String,
}

impl Binding {
    fn name(&self) -> String {
        format!("{}/{}", self.share, self.model)
    }
}

struct Inner {
    /// Directories of shares, searched afresh at every bind: a share comes
    /// and goes under a `snap connect` or a refresh.
    dirs: Vec<PathBuf>,
    bound: Mutex<HashMap<String, Binding>>,
    /// One bind at a time, so a press and the background binder cannot both
    /// load the same model.
    binding: tokio::sync::Mutex<()>,
    status: watch::Sender<ChainStatus>,
}

/// Resolves requests against connected text shares. Cloning shares the
/// bindings.
#[derive(Clone)]
pub struct Resolver(Arc<Inner>);

impl Resolver {
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self(Arc::new(Inner {
            dirs,
            bound: Mutex::default(),
            binding: tokio::sync::Mutex::new(()),
            status: watch::channel(ChainStatus::Ready).0,
        }))
    }

    pub fn status(&self) -> watch::Receiver<ChainStatus> {
        self.0.status.subscribe()
    }

    fn bound(&self, key: &str) -> Option<Binding> {
        self.0
            .bound
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .cloned()
    }

    fn evict(&self, key: &str) {
        self.0
            .bound
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(key);
    }

    /// Every candidate of every connected Myna text share, and whether a
    /// share is connected that this process cannot see yet.
    async fn candidates(&self, within: Duration) -> (Vec<Candidate>, bool) {
        let mut providers: Vec<TextProvider> = Vec::new();
        let mut needs_restart = false;
        for dir in &self.0.dirs {
            let shares = resolve_text_shares(dir);
            needs_restart |= shares.needs_restart();
            providers.extend(shares.providers);
        }
        let mut out = Vec::new();
        for provider in providers {
            let share = provider.snap_name.clone();
            let found = match provider.kind {
                TextShareKind::MynaText => wire::capabilities(&provider.socket, within)
                    .await
                    .map(|caps| (Via::Myna, caps.models)),
                // Unlabelled: whatever it is, ask it as a chat server.
                TextShareKind::Unlabelled => {
                    openai::models(&provider.socket, within).await.map(|ids| {
                        (
                            Via::Chat,
                            ids.iter().flat_map(|id| openai::capabilities(id)).collect(),
                        )
                    })
                }
            };
            match found {
                Ok((via, models)) => out.extend(models.into_iter().map(|capability| Candidate {
                    via,
                    socket: provider.socket.clone(),
                    share: share.clone(),
                    capability,
                })),
                Err(e) => myna_core::info_log!("process", "{share}: no models ({e:?})"),
            }
        }
        (out, needs_restart)
    }

    /// Bind `request`: warm the best-ranked model, and the next one only if
    /// that fails or runs past the bind budget. `None` when nothing covers it
    /// or nothing warms.
    async fn bind(&self, request: &Request) -> (Option<Binding>, bool) {
        let _one = self.0.binding.lock().await;
        let key = request.key();
        if let Some(done) = self.bound(&key) {
            return (Some(done), false);
        }
        let (candidates, needs_restart) = self.candidates(request.bind_budget).await;
        for candidate in rank(request, candidates).into_iter().take(2) {
            let warmed = match candidate.via {
                Via::Myna => {
                    wire::warm(
                        &candidate.socket,
                        &candidate.capability.id,
                        &request.task,
                        &request.params,
                        request.bind_budget,
                    )
                    .await
                }
                Via::Chat => chat(
                    &candidate.socket,
                    &candidate.capability.id,
                    request,
                    WARM_TEXT,
                    request.bind_budget,
                )
                .await
                .map(|_| ()),
            };
            match warmed {
                Ok(()) => {
                    let binding = Binding {
                        via: candidate.via,
                        socket: candidate.socket,
                        share: candidate.share,
                        model: candidate.capability.id,
                    };
                    myna_core::info_log!("process", "{} bound to {}", request.task, binding.name());
                    self.0
                        .bound
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(key, binding.clone());
                    return (Some(binding), needs_restart);
                }
                Err(e) => myna_core::info_log!(
                    "process",
                    "{}: {}/{} did not warm ({})",
                    request.task,
                    candidate.share,
                    candidate.capability.id,
                    match e {
                        WireError::Unavailable(_) => "unavailable",
                        WireError::Refused(_) => "refused",
                    }
                ),
            }
        }
        (None, needs_restart)
    }

    /// Bind whatever of `requests` is unbound and publish whether all of
    /// them are. Called at daemon start, on a chain change, and when a share
    /// may have appeared.
    pub async fn settle(&self, requests: &[Request]) {
        let mut missing = BTreeSet::new();
        let mut needs_restart = false;
        for request in requests {
            let (binding, restart) = self.bind(request).await;
            needs_restart |= restart;
            if binding.is_none() {
                missing.insert(request.task.clone());
            }
        }
        let status = if missing.is_empty() {
            ChainStatus::Ready
        } else {
            ChainStatus::Degraded {
                missing_tasks: missing.into_iter().collect(),
                needs_restart,
            }
        };
        self.0.status.send_if_modified(|now| {
            let changed = *now != status;
            *now = status;
            changed
        });
    }
}

#[async_trait]
impl Models for Resolver {
    async fn run(&mut self, request: &Request, text: &str) -> Result<Answer, ModelError> {
        let key = request.key();
        let binding = match self.bound(&key) {
            Some(binding) => binding,
            None => self
                .bind(request)
                .await
                .0
                .ok_or_else(|| ModelError::Unavailable("no model covers the request".into()))?,
        };
        let run = match binding.via {
            Via::Myna => {
                wire::run(
                    &binding.socket,
                    &binding.model,
                    &request.task,
                    &request.params,
                    text,
                    request.run_budget,
                )
                .await
            }
            Via::Chat => {
                chat(
                    &binding.socket,
                    &binding.model,
                    request,
                    text,
                    request.run_budget,
                )
                .await
            }
        };
        match run {
            Ok(text) => Ok(Answer {
                text,
                model: Some(binding.name()),
            }),
            Err(WireError::Refused(why)) => Err(ModelError::Refused(why)),
            Err(WireError::Unavailable(why)) => {
                // The share went away or restarted: bind afresh next time.
                self.evict(&key);
                Err(ModelError::Unavailable(why))
            }
        }
    }

    fn prepare(&mut self, requests: &[Request]) {
        let unbound: Vec<Request> = requests
            .iter()
            .filter(|r| self.bound(&r.key()).is_none())
            .cloned()
            .collect();
        if unbound.is_empty() {
            return;
        }
        let resolver = self.clone();
        tokio::spawn(async move {
            for request in &unbound {
                resolver.bind(request).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    fn cap(id: &str, task: &str, latency: Option<u64>, quality: Option<f64>) -> Candidate {
        Candidate {
            via: Via::Myna,
            socket: "/s".into(),
            share: "share".into(),
            capability: serde_json::from_value(json!({
                "id": id, "task": task, "covers": {"language": ["en"]},
                "latency_ms": latency, "quality": quality,
            }))
            .unwrap(),
        }
    }

    fn ids(ranked: &[Candidate]) -> Vec<&str> {
        ranked.iter().map(|c| c.capability.id.as_str()).collect()
    }

    #[test]
    fn ranking_drops_what_cannot_serve_and_orders_the_rest_by_preference() {
        let mut request = Request::cleanup(Some("en".into()));
        let all = || {
            vec![
                cap("slow-good", "text.cleanup", Some(900), Some(0.9)),
                cap("fast", "text.cleanup", Some(5), Some(0.5)),
                cap("too-slow", "text.cleanup", Some(5000), Some(1.0)),
                cap("other-task", "text.translate", Some(1), Some(1.0)),
                cap("unknown", "text.cleanup", None, None),
            ]
        };
        assert_eq!(
            ids(&rank(&request, all())),
            ["fast", "slow-good", "unknown"]
        );
        request.prefer = Prefer::Accuracy;
        assert_eq!(
            ids(&rank(&request, all())),
            ["slow-good", "fast", "unknown"]
        );
        request.params.insert("language".into(), "fr".into());
        assert!(rank(&request, all()).is_empty());
    }

    #[test]
    fn a_tie_is_broken_by_share_then_model_not_by_arrival() {
        let request = Request::cleanup(None);
        let mut a = cap("m", "text.cleanup", Some(5), Some(0.5));
        a.share = "b-share".into();
        let mut b = cap("m", "text.cleanup", Some(5), Some(0.5));
        b.share = "a-share".into();
        let c = cap("a", "text.cleanup", Some(5), Some(0.5));
        let forward = rank(&request, vec![a.clone(), b.clone(), c.clone()]);
        let backward = rank(&request, vec![c, b, a]);
        assert_eq!(forward, backward);
        assert_eq!(forward[0].share, "a-share");
    }

    /// A provider share on disk: `provider.env` and a socket it serves.
    struct Share {
        dir: PathBuf,
        warms: Arc<AtomicUsize>,
        runs: Arc<AtomicUsize>,
    }

    #[derive(Clone, Copy)]
    enum Mode {
        Works,
        WarmFails,
        WarmHangs,
        RunRefuses,
    }

    fn root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("myna-resolve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn share(root: &Path, name: &str, models: Value, mode: Mode) -> Share {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("provider.env"),
            format!("SNAP_NAME={name}\nMYNA_ROLE=text\nUNIX_SOCKET=text.sock\n"),
        )
        .unwrap();
        let listener = UnixListener::bind(dir.join("text.sock")).unwrap();
        let (warms, runs) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (w, r) = (warms.clone(), runs.clone());
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (models, w, r) = (models.clone(), w.clone(), r.clone());
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut line = String::new();
                    BufReader::new(read).read_line(&mut line).await.unwrap();
                    let call: Value = serde_json::from_str(&line).unwrap();
                    let reply = match call["op"].as_str().unwrap() {
                        "capabilities" => {
                            json!({"protocol": "myna-text-v1", "models": models})
                        }
                        "warm" => {
                            w.fetch_add(1, Ordering::SeqCst);
                            match mode {
                                Mode::Works | Mode::RunRefuses => json!({"ok": true}),
                                Mode::WarmFails => json!({"error": "no memory"}),
                                Mode::WarmHangs => std::future::pending().await,
                            }
                        }
                        _ => {
                            r.fetch_add(1, Ordering::SeqCst);
                            if matches!(mode, Mode::RunRefuses) {
                                json!({"error": "cannot"})
                            } else {
                                json!({"text": format!("{}!", call["text"].as_str().unwrap())})
                            }
                        }
                    };
                    write
                        .write_all(format!("{reply}\n").as_bytes())
                        .await
                        .unwrap();
                });
            }
        });
        Share { dir, warms, runs }
    }

    fn model(id: &str, task: &str, latency: u64) -> Value {
        json!({"id": id, "task": task, "covers": {"language": ["en"], "to": ["fr"]},
               "latency_ms": latency})
    }

    fn bound(resolver: &Resolver, request: &Request) -> Option<String> {
        resolver.bound(&request.key()).map(|b| b.name())
    }

    #[tokio::test]
    async fn only_the_best_ranked_model_is_warmed() {
        let dir = root("winner");
        let fast = share(
            &dir,
            "fast",
            json!([model("a", "text.cleanup", 5)]),
            Mode::Works,
        );
        let slow = share(
            &dir,
            "slow",
            json!([model("b", "text.cleanup", 50)]),
            Mode::Works,
        );
        let resolver = Resolver::new(vec![dir.clone()]);
        let request = Request::cleanup(Some("en".into()));
        resolver.settle(std::slice::from_ref(&request)).await;
        assert_eq!(bound(&resolver, &request).as_deref(), Some("fast/a"));
        assert_eq!(fast.warms.load(Ordering::SeqCst), 1);
        assert_eq!(slow.warms.load(Ordering::SeqCst), 0);
        assert_eq!(*resolver.status().borrow(), ChainStatus::Ready);
        // Bound: settling again probes and warms nothing.
        resolver.settle(std::slice::from_ref(&request)).await;
        assert_eq!(fast.warms.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_winner_that_fails_to_warm_hands_over_to_the_runner_up() {
        let dir = root("failover");
        let a = share(
            &dir,
            "a",
            json!([model("a", "text.cleanup", 5)]),
            Mode::WarmFails,
        );
        let b = share(
            &dir,
            "b",
            json!([model("b", "text.cleanup", 50)]),
            Mode::Works,
        );
        let resolver = Resolver::new(vec![dir.clone()]);
        let request = Request::cleanup(None);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert_eq!(bound(&resolver, &request).as_deref(), Some("b/b"));
        assert_eq!(
            (
                a.warms.load(Ordering::SeqCst),
                b.warms.load(Ordering::SeqCst)
            ),
            (1, 1)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_winner_past_the_bind_budget_hands_over_too() {
        let dir = root("slowbind");
        share(
            &dir,
            "a",
            json!([model("a", "text.cleanup", 5)]),
            Mode::WarmHangs,
        );
        share(
            &dir,
            "b",
            json!([model("b", "text.cleanup", 50)]),
            Mode::Works,
        );
        let resolver = Resolver::new(vec![dir.clone()]);
        let mut request = Request::cleanup(None);
        request.bind_budget = Duration::from_millis(200);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert_eq!(bound(&resolver, &request).as_deref(), Some("b/b"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_third_candidate_is_never_tried() {
        let dir = root("two");
        let c = [("a", 5), ("b", 6), ("c", 7)].map(|(n, ms)| {
            let mode = if n == "c" {
                Mode::Works
            } else {
                Mode::WarmFails
            };
            share(&dir, n, json!([model(n, "text.cleanup", ms)]), mode)
        });
        let resolver = Resolver::new(vec![dir.clone()]);
        let request = Request::cleanup(None);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert!(bound(&resolver, &request).is_none());
        assert_eq!(c[2].warms.load(Ordering::SeqCst), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn an_uncovered_task_is_reported_missing_until_a_share_covers_it() {
        let dir = root("missing");
        share(
            &dir,
            "tagger",
            json!([model("a", "text.cleanup", 5)]),
            Mode::Works,
        );
        let resolver = Resolver::new(vec![dir.clone()]);
        let requests = [Request::cleanup(None), Request::translate("en", "fr")];
        resolver.settle(&requests).await;
        assert_eq!(
            *resolver.status().borrow(),
            ChainStatus::Degraded {
                missing_tasks: vec!["text.translate".into()],
                needs_restart: false
            }
        );
        // The same answer is not published again, a different one is.
        let mut seen = resolver.status();
        seen.borrow_and_update();
        resolver.settle(&requests).await;
        assert!(!seen.has_changed().unwrap());
        share(
            &dir,
            "marian",
            json!([model("m", "text.translate", 90)]),
            Mode::Works,
        );
        resolver.settle(&requests).await;
        assert!(seen.has_changed().unwrap());
        assert_eq!(*resolver.status().borrow(), ChainStatus::Ready);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_share_connected_after_start_asks_for_a_restart() {
        let dir = root("restart");
        std::fs::create_dir_all(dir.join("late")).unwrap();
        let resolver = Resolver::new(vec![dir.clone()]);
        resolver.settle(&[Request::cleanup(None)]).await;
        assert_eq!(
            *resolver.status().borrow(),
            ChainStatus::Degraded {
                missing_tasks: vec!["text.cleanup".into()],
                needs_restart: true
            }
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn an_unbound_request_binds_at_run_and_runs_with_the_models_name() {
        let dir = root("lazy");
        let s = share(
            &dir,
            "tagger",
            json!([model("a", "text.cleanup", 5)]),
            Mode::Works,
        );
        let mut resolver = Resolver::new(vec![dir.clone()]);
        let request = Request::cleanup(None);
        let answer = resolver.run(&request, "hi").await.unwrap();
        assert_eq!(answer.text, "hi!");
        assert_eq!(answer.model.as_deref(), Some("tagger/a"));
        resolver.run(&request, "hi").await.unwrap();
        assert_eq!(
            (
                s.warms.load(Ordering::SeqCst),
                s.runs.load(Ordering::SeqCst)
            ),
            (1, 2)
        );
        let none = Request::translate("en", "de");
        assert!(matches!(
            resolver.run(&none, "hi").await,
            Err(ModelError::Unavailable(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_share_that_went_away_is_evicted_and_found_again_by_prepare() {
        let dir = root("evict");
        let first = share(
            &dir,
            "tagger",
            json!([model("a", "text.cleanup", 5)]),
            Mode::Works,
        );
        let mut resolver = Resolver::new(vec![dir.clone()]);
        let request = Request::cleanup(None);
        resolver.settle(std::slice::from_ref(&request)).await;
        std::fs::remove_file(first.dir.join("text.sock")).unwrap();
        assert!(matches!(
            resolver.run(&request, "hi").await,
            Err(ModelError::Unavailable(_))
        ));
        assert!(bound(&resolver, &request).is_none());
        let second = share(
            &dir,
            "tagger",
            json!([model("a", "text.cleanup", 5)]),
            Mode::Works,
        );
        // Nothing to do while it is bound; once evicted, prepare binds.
        resolver.prepare(std::slice::from_ref(&request));
        for _ in 0..100 {
            if bound(&resolver, &request).is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(bound(&resolver, &request).as_deref(), Some("tagger/a"));
        assert_eq!(second.warms.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_refusal_keeps_the_binding() {
        let dir = root("refuse");
        share(
            &dir,
            "tagger",
            json!([model("a", "text.cleanup", 5)]),
            Mode::RunRefuses,
        );
        let mut resolver = Resolver::new(vec![dir.clone()]);
        let request = Request::cleanup(None);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert!(matches!(
            resolver.run(&request, "hi").await,
            Err(ModelError::Refused(why)) if why == "cannot"
        ));
        assert_eq!(bound(&resolver, &request).as_deref(), Some("tagger/a"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An unlabelled share serving OpenAI chat: every request is answered
    /// with a completion that upper-cases the user message.
    fn chat_share(root: &Path, name: &str, model: &'static str) -> Arc<AtomicUsize> {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("provider.env"),
            format!("SNAP_NAME={name}\nUNIX_SOCKET=chat.sock\n"),
        )
        .unwrap();
        let listener = UnixListener::bind(dir.join("chat.sock")).unwrap();
        let completions = Arc::new(AtomicUsize::new(0));
        let counted = completions.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let counted = counted.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0; 16384];
                    let n = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
                        .await
                        .unwrap();
                    let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let body = if request.starts_with("GET /v1/models") {
                        json!({"data": [{"id": model}]}).to_string()
                    } else {
                        counted.fetch_add(1, Ordering::SeqCst);
                        let json = &request[request.find("\r\n\r\n").unwrap() + 4..];
                        let sent: Value = serde_json::from_str(json).unwrap();
                        let user = sent["messages"][1]["content"]
                            .as_str()
                            .unwrap()
                            .to_uppercase();
                        json!({"choices": [{"message": {"content": user}}]}).to_string()
                    };
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(reply.as_bytes()).await.unwrap();
                });
            }
        });
        completions
    }

    #[tokio::test]
    async fn a_chat_share_serves_what_its_recipes_cover_and_loses_to_a_faster_model() {
        let dir = root("chat");
        let completions = chat_share(&dir, "gemma4", "gemma4-e4b");
        let mut resolver = Resolver::new(vec![dir.clone()]);
        let translate = Request::translate("en", "fr");
        let answer = resolver.run(&translate, "hello").await.unwrap();
        assert_eq!(answer.text, "HELLO");
        assert_eq!(answer.model.as_deref(), Some("gemma4/gemma4-e4b"));
        // One completion to warm, one to run.
        assert_eq!(completions.load(Ordering::SeqCst), 2);
        // Cleanup has a 1 s budget and a chat model takes longer than that.
        let cleanup = Request::cleanup(None);
        assert!(resolver.run(&cleanup, "um hello").await.is_err());
        share(
            &dir,
            "marian",
            json!([model("m", "text.translate", 90)]),
            Mode::Works,
        );
        let faster = Request::translate("en", "fr");
        resolver.evict(&faster.key());
        let answer = resolver.run(&faster, "hello").await.unwrap();
        assert_eq!(answer.model.as_deref(), Some("marian/m"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
