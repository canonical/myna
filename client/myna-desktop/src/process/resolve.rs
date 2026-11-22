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
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use myna_orchestrator::backend::share::{resolve_text_shares, TextProvider, TextShareKind};
use tokio::sync::watch;

use super::openai;
use super::pipeline::{Answer, ModelError, Models, Prefer, Request, BIND_BUDGET};
use super::step;
use super::steps::{compile, Compiled, Context, StepReport, StepSpec};
use super::wire::{self, Capability, Params, WireError};

/// Where text model shares are searched: the backend directory, and the
/// colon separated directories `extra` adds (`MYNA_TEXT_SHARES`, for a
/// provider run by hand). The daemon and the testbed search alike.
pub fn share_dirs(
    backend: Option<&std::path::Path>,
    extra: Option<std::ffi::OsString>,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = backend.map(PathBuf::from).into_iter().collect();
    if let Some(extra) = extra {
        dirs.extend(std::env::split_paths(&extra).filter(|p| !p.as_os_str().is_empty()));
    }
    dirs
}

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
        /// Requests no connected share covers, in key order.
        missing: Vec<Missing>,
        /// A share was connected after this process started, so only a
        /// fresh daemon can see it.
        needs_restart: bool,
    },
}

/// A request no connected model covers: what to install, never any text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    pub task: String,
    pub params: Params,
}

impl Missing {
    pub fn of(request: &Request) -> Self {
        Self {
            task: request.task.clone(),
            params: request.params.clone(),
        }
    }
}

impl ChainStatus {
    /// The tasks with no model, sorted and without repeats.
    pub fn missing_tasks(&self) -> Vec<String> {
        match self {
            Self::Ready => Vec::new(),
            Self::Degraded { missing, .. } => missing
                .iter()
                .map(|m| m.task.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }

    /// Whether no model covers `request`.
    pub fn lacks(&self, request: &Request) -> bool {
        matches!(self, Self::Degraded { missing, .. }
            if missing.iter().any(|m| m.task == request.task && m.params == request.params))
    }
}

/// What the daemon publishes for Settings besides the status: every model
/// of every connected share, and what became of each step.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    pub models: Vec<Candidate>,
    pub steps: Vec<StepReport>,
}

/// How often the binder looks for a changed share, and how often it retries
/// a request that is still unbound.
#[derive(Clone, Copy, Debug)]
pub struct Cadence {
    pub poll: Duration,
    pub retry: Duration,
}

impl Default for Cadence {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(2),
            retry: Duration::from_secs(5),
        }
    }
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

/// How long after a failed bind a piece of text leaves the request alone,
/// typing as heard. The binder retries every few seconds meanwhile.
const INLINE_RETRY: Duration = Duration::from_secs(30);

struct Inner {
    /// Directories of shares, searched afresh at every bind: a share comes
    /// and goes under a `snap connect` or a refresh.
    dirs: Vec<PathBuf>,
    /// Directories whose shares are bound whatever snap provides them
    /// (`MYNA_TEXT_SHARES`).
    open_dirs: Vec<PathBuf>,
    /// Snaps already logged as not on the allow-list.
    ignored: Mutex<BTreeSet<String>>,
    bound: Mutex<HashMap<String, Binding>>,
    /// One bind at a time, so a press and the background binder cannot both
    /// load the same model.
    binding: tokio::sync::Mutex<()>,
    /// When each request last failed to bind, so a piece of text does not
    /// retry a bind that just failed; the background binder is not held to it.
    failed: Mutex<HashMap<String, Instant>>,
    status: watch::Sender<ChainStatus>,
    catalog: watch::Sender<Catalog>,
}

/// Resolves requests against connected text shares. Cloning shares the
/// bindings.
#[derive(Clone)]
pub struct Resolver(Arc<Inner>);

impl Resolver {
    /// Binds only the shares of curated snaps found under `dirs`.
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self::with_open(dirs, Vec::new())
    }

    /// Binds every share found under `dirs`, whatever snap provides it.
    pub fn open(dirs: Vec<PathBuf>) -> Self {
        Self::with_open(Vec::new(), dirs)
    }

    /// The resolver the daemon runs: the backend directory (curated) and the
    /// colon separated `extra` directories (`MYNA_TEXT_SHARES`, open).
    pub fn from_env(backend: Option<&std::path::Path>, extra: Option<std::ffi::OsString>) -> Self {
        Self::with_open(share_dirs(backend, None), share_dirs(None, extra))
    }

    /// [`Resolver::new`] for `dirs`, and every share under `open_dirs`.
    pub fn with_open(dirs: Vec<PathBuf>, open_dirs: Vec<PathBuf>) -> Self {
        Self(Arc::new(Inner {
            dirs,
            open_dirs,
            ignored: Mutex::default(),
            bound: Mutex::default(),
            binding: tokio::sync::Mutex::new(()),
            failed: Mutex::default(),
            status: watch::channel(ChainStatus::Ready).0,
            catalog: watch::channel(Catalog::default()).0,
        }))
    }

    pub fn status(&self) -> watch::Receiver<ChainStatus> {
        self.0.status.subscribe()
    }

    pub fn catalog(&self) -> watch::Receiver<Catalog> {
        self.0.catalog.subscribe()
    }

    /// Publish what became of each step; the daemon compiles them.
    pub fn set_steps(&self, steps: Vec<StepReport>) {
        self.0.catalog.send_if_modified(|now| {
            let changed = now.steps != steps;
            now.steps = steps;
            changed
        });
    }

    /// What the shares look like on disk: the modification times of each
    /// share directory and its entries (`provider.env`, the socket). A
    /// connect, a disconnect, a refresh or a component install moves one.
    pub fn signature(&self) -> Vec<(PathBuf, Option<std::time::SystemTime>)> {
        let mtime =
            |path: &std::path::Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let mut seen = Vec::new();
        for dir in self.0.dirs.iter().chain(&self.0.open_dirs) {
            seen.push((dir.clone(), mtime(dir)));
            let Ok(shares) = std::fs::read_dir(dir) else {
                continue;
            };
            for share in shares.flatten().map(|e| e.path()) {
                seen.push((share.clone(), mtime(&share)));
                let Ok(entries) = std::fs::read_dir(&share) else {
                    continue;
                };
                seen.extend(entries.flatten().map(|e| (e.path(), mtime(&e.path()))));
            }
        }
        seen.sort();
        seen
    }

    /// Bind what `wanted` names and publish the status, again whenever
    /// `wake` fires (a chain change, `RescanModels`), a share changes on disk,
    /// or a request is still unbound after `cadence.retry`. `after` runs
    /// after each settle. Never returns.
    pub async fn keep_bound(
        &self,
        wake: &tokio::sync::Notify,
        cadence: Cadence,
        wanted: impl Fn() -> Vec<Request>,
        after: impl Fn(&Self),
    ) {
        loop {
            self.settle(&wanted()).await;
            after(self);
            let seen = self.signature();
            let since = Instant::now();
            let woken = wake.notified();
            tokio::pin!(woken);
            loop {
                tokio::select! {
                    () = &mut woken => break,
                    () = tokio::time::sleep(cadence.poll) => {
                        let degraded = *self.0.status.borrow() != ChainStatus::Ready;
                        if self.signature() != seen
                            || (degraded && since.elapsed() >= cadence.retry)
                        {
                            break;
                        }
                    }
                }
            }
        }
    }

    /// `steps` compiled for dictation in `language` against what is
    /// installed and bound now: a translation no installed model covers
    /// directly pivots through English when its legs are installed.
    pub fn compile(&self, steps: &[StepSpec], language: Option<&str>) -> Compiled {
        let status = self.0.status.borrow().clone();
        let models = self.0.catalog.borrow().models.clone();
        compile(
            steps,
            &Context {
                language,
                unavailable: &|request| status.lacks(request),
                uncovered: &|request| rank(request, models.clone()).is_empty(),
            },
        )
    }

    /// The `share/model` a request is bound to, if it is.
    pub fn bound_to(&self, request: &Request) -> Option<String> {
        self.bound(&request.key()).map(|binding| binding.name())
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

    fn ignore(&self, snap: &str) {
        let first = self
            .0
            .ignored
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(snap.to_string());
        if first {
            myna_core::info_log!("process", "{snap}: not a curated text provider, ignored");
        }
    }

    /// Every candidate of every connected Myna text share, whether a
    /// share is connected that this process cannot see yet, and the shares
    /// that answered.
    async fn candidates(&self, within: Duration) -> (Vec<Candidate>, bool, BTreeSet<String>) {
        let mut providers: Vec<TextProvider> = Vec::new();
        let mut needs_restart = false;
        for dir in &self.0.dirs {
            let shares = resolve_text_shares(dir);
            needs_restart |= shares.needs_restart();
            providers.extend(shares.providers.into_iter().filter(|p| {
                p.is_curated() || {
                    self.ignore(&p.snap_name);
                    false
                }
            }));
        }
        for dir in &self.0.open_dirs {
            let shares = resolve_text_shares(dir);
            needs_restart |= shares.needs_restart();
            providers.extend(shares.providers);
        }
        let mut out = Vec::new();
        let mut answered = BTreeSet::new();
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
                Ok((via, models)) => {
                    answered.insert(share.clone());
                    out.extend(
                        models
                            .into_iter()
                            .filter(|capability| {
                                if step::check_of(capability).is_some() {
                                    return true;
                                }
                                myna_core::info_log!(
                                    "process",
                                    "{share}/{}: unknown class, not offered",
                                    capability.id
                                );
                                false
                            })
                            .map(|capability| Candidate {
                                via,
                                socket: provider.socket.clone(),
                                share: share.clone(),
                                capability,
                            }),
                    )
                }
                Err(e) => myna_core::info_log!("process", "{share}: no models ({e:?})"),
            }
        }
        (out, needs_restart, answered)
    }

    /// Bind `request`: warm the best-ranked model, and the next one only if
    /// that fails or runs past the bind budget. `None` when nothing covers it
    /// or nothing warms.
    async fn bind(&self, request: &Request) -> (Option<Binding>, bool) {
        let _one = self.0.binding.lock().await;
        self.bind_held(request).await
    }

    /// The last resort for a piece of text that finds its request unbound:
    /// binds only when nothing failed lately and no other bind is running,
    /// so a model that will not load costs one budget, not one per piece.
    async fn bind_inline(&self, request: &Request) -> Option<Binding> {
        let key = request.key();
        let lately = self
            .0
            .failed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key)
            .is_some_and(|at| at.elapsed() < INLINE_RETRY);
        if lately {
            return None;
        }
        let _one = self.0.binding.try_lock().ok()?;
        self.bind_held(request).await.0
    }

    /// [`Self::bind`] with the binding lock already held.
    async fn bind_held(&self, request: &Request) -> (Option<Binding>, bool) {
        let key = request.key();
        if let Some(done) = self.bound(&key) {
            return (Some(done), false);
        }
        let (candidates, needs_restart, _) = self.candidates(request.bind_budget).await;
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
                        .insert(key.clone(), binding.clone());
                    self.0
                        .failed
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&key);
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
                        WireError::TimedOut => "timed out",
                    }
                ),
            }
        }
        self.0
            .failed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, Instant::now());
        (None, needs_restart)
    }

    /// Unbind what a share that answered no longer offers (a removed
    /// component), so the request reads as missing and binds again if the
    /// model returns.
    fn forget_withdrawn(&self, models: &[Candidate], answered: &BTreeSet<String>) {
        self.0
            .bound
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|_, b| {
                !answered.contains(&b.share)
                    || models
                        .iter()
                        .any(|c| c.share == b.share && c.capability.id == b.model)
            });
    }

    /// Bind whatever of `requests` is unbound and publish whether all of
    /// them are. Called at daemon start, on a chain change, and when a share
    /// may have appeared.
    pub async fn settle(&self, requests: &[Request]) {
        let (models, _, answered) = self.candidates(BIND_BUDGET).await;
        self.forget_withdrawn(&models, &answered);
        self.0.catalog.send_if_modified(|now| {
            let changed = now.models != models;
            now.models = models;
            changed
        });
        let mut missing = BTreeMap::new();
        let mut needs_restart = false;
        for request in requests {
            let (binding, restart) = self.bind(request).await;
            needs_restart |= restart;
            if binding.is_none() {
                missing.insert(request.key(), Missing::of(request));
            }
        }
        let status = if missing.is_empty() {
            ChainStatus::Ready
        } else {
            ChainStatus::Degraded {
                missing: missing.into_values().collect(),
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
                .bind_inline(request)
                .await
                .ok_or_else(|| ModelError::Unavailable("no model is bound".into()))?,
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
            // Slow, not gone: the model stays bound, or every piece would
            // pay to load it again.
            Err(WireError::TimedOut) => Err(ModelError::TimedOut),
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
    use super::super::steps::{StepReport, StepState};
    use super::*;
    use serde_json::{json, Value};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[test]
    fn shares_are_searched_in_the_backend_dir_then_the_extra_ones() {
        let dirs = share_dirs(Some(Path::new("/b")), Some("/x::/y".into()));
        assert_eq!(dirs, [Path::new("/b"), Path::new("/x"), Path::new("/y")]);
        assert!(share_dirs(None, None).is_empty());
    }

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
        models: Arc<Mutex<Value>>,
    }

    #[derive(Clone, Copy)]
    enum Mode {
        Works,
        WarmFails,
        WarmHangs,
        RunRefuses,
        RunHangs,
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
        let models = Arc::new(Mutex::new(models));
        let served = models.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (models, w, r) = (served.clone(), w.clone(), r.clone());
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut line = String::new();
                    BufReader::new(read).read_line(&mut line).await.unwrap();
                    let call: Value = serde_json::from_str(&line).unwrap();
                    let reply = match call["op"].as_str().unwrap() {
                        "capabilities" => {
                            json!({"protocol": "myna-text-v1", "models": *models.lock().unwrap()})
                        }
                        "warm" => {
                            w.fetch_add(1, Ordering::SeqCst);
                            match mode {
                                Mode::Works | Mode::RunRefuses | Mode::RunHangs => {
                                    json!({"ok": true})
                                }
                                Mode::WarmFails => json!({"error": "no memory"}),
                                Mode::WarmHangs => std::future::pending().await,
                            }
                        }
                        _ => {
                            r.fetch_add(1, Ordering::SeqCst);
                            if matches!(mode, Mode::RunHangs) {
                                std::future::pending::<()>().await;
                            }
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
        Share {
            dir,
            warms,
            runs,
            models,
        }
    }

    fn model(id: &str, task: &str, latency: u64) -> Value {
        json!({"id": id, "task": task, "covers": {"language": ["en"], "to": ["fr"]},
               "latency_ms": latency})
    }

    fn bound(resolver: &Resolver, request: &Request) -> Option<String> {
        resolver.bound_to(request)
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
        let resolver = Resolver::open(vec![dir.clone()]);
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
        let resolver = Resolver::open(vec![dir.clone()]);
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
        let resolver = Resolver::open(vec![dir.clone()]);
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
        let resolver = Resolver::open(vec![dir.clone()]);
        let request = Request::cleanup(None);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert!(bound(&resolver, &request).is_none());
        assert_eq!(c[2].warms.load(Ordering::SeqCst), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn cleanup_and_translate_bind_from_two_snaps_under_one_backend_dir() {
        let dir = root("two-snaps");
        let text = share(
            &dir,
            "myna-text",
            json!([model("myna-disfluency-en", "text.cleanup", 10)]),
            Mode::Works,
        );
        let translation = share(
            &dir,
            "myna-translation",
            json!([model("bergamot-base-memory-en-fr", "text.translate", 25)]),
            Mode::Works,
        );
        let resolver = Resolver::open(vec![dir.clone()]);
        let cleanup = Request::cleanup(Some("en".into()));
        let translate = Request::translate("en", "fr");
        resolver.settle(&[cleanup.clone(), translate.clone()]).await;
        assert_eq!(
            bound(&resolver, &cleanup).as_deref(),
            Some("myna-text/myna-disfluency-en")
        );
        assert_eq!(
            bound(&resolver, &translate).as_deref(),
            Some("myna-translation/bergamot-base-memory-en-fr")
        );
        assert_eq!(text.warms.load(Ordering::SeqCst), 1);
        assert_eq!(translation.warms.load(Ordering::SeqCst), 1);
        assert_eq!(*resolver.status().borrow(), ChainStatus::Ready);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn pair(from: &str, to: &str) -> Value {
        json!({"id": format!("{from}-{to}"), "task": "text.translate",
               "covers": {"language": [from], "to": [to]}, "latency_ms": 20})
    }

    async fn typed(resolver: &Resolver, steps: &[StepSpec], text: &str) -> String {
        use super::super::pipeline::Pipeline;
        use super::super::{Field, Processor};
        let graph = resolver.compile(steps, Some("fr")).graph;
        Pipeline::new(graph, resolver.clone())
            .process(text.into(), &Field::default())
            .await
    }

    #[tokio::test]
    async fn a_translation_with_no_direct_model_runs_through_english() {
        let dir = root("pivot");
        let share = share(
            &dir,
            "myna-translation",
            json!([pair("en", "de"), pair("fr", "en")]),
            Mode::Works,
        );
        let resolver = Resolver::open(vec![dir.clone()]);
        let steps = [StepSpec::new("translate").with("to", "de")];
        resolver.settle(&[]).await;
        let compiled = resolver.compile(&steps, Some("fr"));
        assert!(!compiled.degraded());
        let wanted = compiled.graph.requests();
        assert_eq!(wanted.len(), 2);
        resolver.settle(&wanted).await;
        assert_eq!(*resolver.status().borrow(), ChainStatus::Ready);
        assert_eq!(share.warms.load(Ordering::SeqCst), 2);
        assert_eq!(typed(&resolver, &steps, "A lundi.").await, "A lundi.!!");
        assert_eq!(share.runs.load(Ordering::SeqCst), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_direct_model_that_appears_replaces_the_pivot() {
        let dir = root("pivot-direct");
        let share = share(
            &dir,
            "myna-translation",
            json!([pair("en", "de"), pair("fr", "en")]),
            Mode::Works,
        );
        let resolver = Resolver::open(vec![dir.clone()]);
        let steps = [StepSpec::new("translate").with("to", "de")];
        resolver.settle(&[]).await;
        assert_eq!(
            resolver.compile(&steps, Some("fr")).graph.requests().len(),
            2
        );
        *share.models.lock().unwrap() = json!([pair("fr", "de"), pair("fr", "en")]);
        resolver.settle(&[]).await;
        let wanted = resolver.compile(&steps, Some("fr")).graph.requests();
        assert_eq!(wanted, [Request::translate("fr", "de")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_pivot_missing_a_leg_names_that_leg_and_types_what_was_heard() {
        let dir = root("pivot-missing");
        share(
            &dir,
            "myna-translation",
            json!([pair("fr", "en")]),
            Mode::Works,
        );
        let resolver = Resolver::open(vec![dir.clone()]);
        let steps = [StepSpec::new("translate").with("to", "de")];
        resolver.settle(&[]).await;
        let wanted = resolver.compile(&steps, Some("fr")).graph.requests();
        resolver.settle(&wanted).await;
        let leg = wanted.iter().find(|r| r.params["to"] == "de").unwrap();
        assert_eq!(
            *resolver.status().borrow(),
            ChainStatus::Degraded {
                missing: vec![Missing::of(leg)],
                needs_restart: false
            }
        );
        let compiled = resolver.compile(&steps, Some("fr"));
        assert!(compiled.degraded());
        assert_eq!(
            compiled.report[0].state,
            StepState::Unavailable {
                request: leg.clone()
            }
        );
        assert_eq!(typed(&resolver, &steps, "A lundi.").await, "A lundi.");
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
        let resolver = Resolver::open(vec![dir.clone()]);
        let requests = [Request::cleanup(None), Request::translate("en", "fr")];
        resolver.settle(&requests).await;
        assert_eq!(
            *resolver.status().borrow(),
            ChainStatus::Degraded {
                missing: vec![Missing::of(&requests[1])],
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
    async fn a_model_of_an_unknown_class_is_uncovered_and_one_of_a_known_class_is_not() {
        let dir = root("class");
        let mut odd = model("odd", "text.cleanup", 1);
        odd["class"] = json!("passthrough");
        let mut known = model("known", "text.cleanup", 50);
        known["class"] = json!("edit");
        share(&dir, "odd", json!([odd]), Mode::Works);
        let resolver = Resolver::open(vec![dir.clone()]);
        let request = Request::cleanup(None);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert_eq!(bound(&resolver, &request), None);
        assert_eq!(
            *resolver.status().borrow(),
            ChainStatus::Degraded {
                missing: vec![Missing::of(&request)],
                needs_restart: false
            }
        );
        share(&dir, "known", json!([known]), Mode::Works);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert_eq!(bound(&resolver, &request).as_deref(), Some("known/known"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_share_connected_after_start_asks_for_a_restart() {
        let dir = root("restart");
        std::fs::create_dir_all(dir.join("late")).unwrap();
        let resolver = Resolver::open(vec![dir.clone()]);
        resolver.settle(&[Request::cleanup(None)]).await;
        assert_eq!(
            *resolver.status().borrow(),
            ChainStatus::Degraded {
                missing: vec![Missing::of(&Request::cleanup(None))],
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
        let mut resolver = Resolver::open(vec![dir.clone()]);
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
        let mut resolver = Resolver::open(vec![dir.clone()]);
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
        let mut resolver = Resolver::open(vec![dir.clone()]);
        let request = Request::cleanup(None);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert!(matches!(
            resolver.run(&request, "hi").await,
            Err(ModelError::Refused(why)) if why == "cannot"
        ));
        assert_eq!(bound(&resolver, &request).as_deref(), Some("tagger/a"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_slow_model_keeps_its_binding() {
        let dir = root("slow");
        let s = share(
            &dir,
            "tagger",
            json!([model("a", "text.cleanup", 5)]),
            Mode::RunHangs,
        );
        let mut resolver = Resolver::open(vec![dir.clone()]);
        let mut request = Request::cleanup(None);
        request.run_budget = Duration::from_millis(100);
        resolver.settle(std::slice::from_ref(&request)).await;
        for _ in 0..2 {
            assert!(matches!(
                resolver.run(&request, "hi").await,
                Err(ModelError::TimedOut)
            ));
        }
        assert_eq!(bound(&resolver, &request).as_deref(), Some("tagger/a"));
        assert_eq!(s.warms.load(Ordering::SeqCst), 1, "never warmed again");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_failed_bind_is_not_retried_inline_for_every_piece() {
        let dir = root("backoff");
        let s = share(
            &dir,
            "tagger",
            json!([model("a", "text.cleanup", 5)]),
            Mode::WarmFails,
        );
        let mut resolver = Resolver::open(vec![dir.clone()]);
        let request = Request::cleanup(None);
        for _ in 0..3 {
            assert!(matches!(
                resolver.run(&request, "hi").await,
                Err(ModelError::Unavailable(_))
            ));
        }
        assert_eq!(s.warms.load(Ordering::SeqCst), 1);
        // The background binder keeps trying whatever the backoff says.
        resolver.settle(std::slice::from_ref(&request)).await;
        assert_eq!(s.warms.load(Ordering::SeqCst), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_piece_does_not_queue_behind_a_bind_in_progress() {
        let dir = root("noqueue");
        share(
            &dir,
            "tagger",
            json!([model("a", "text.cleanup", 5)]),
            Mode::WarmHangs,
        );
        let mut resolver = Resolver::open(vec![dir.clone()]);
        let mut request = Request::cleanup(None);
        request.bind_budget = Duration::from_millis(600);
        resolver.prepare(std::slice::from_ref(&request));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = std::time::Instant::now();
        assert!(matches!(
            resolver.run(&request, "hi").await,
            Err(ModelError::Unavailable(_))
        ));
        assert!(started.elapsed() < Duration::from_millis(300));
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
        let mut resolver = Resolver::open(vec![dir.clone()]);
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

    #[tokio::test]
    async fn the_catalog_unions_every_share_and_missing_names_full_requests() {
        let dir = root("catalog");
        share(
            &dir,
            "myna-text",
            json!([model("tagger", "text.cleanup", 5)]),
            Mode::Works,
        );
        share(
            &dir,
            "myna-translation",
            json!([model("fr", "text.translate", 10)]),
            Mode::Works,
        );
        let resolver = Resolver::open(vec![dir.clone()]);
        let want = [
            Request::cleanup(Some("en".into())),
            Request::translate("en", "fr"),
            Request::translate("en", "de"),
            Request::translate("en", "es"),
        ];
        resolver.settle(&want).await;
        let shares: Vec<String> = resolver
            .catalog()
            .borrow()
            .models
            .iter()
            .map(|c| format!("{}/{}", c.share, c.capability.id))
            .collect();
        assert_eq!(shares, ["myna-text/tagger", "myna-translation/fr"]);
        let status = resolver.status().borrow().clone();
        let ChainStatus::Degraded { missing, .. } = &status else {
            panic!("{status:?}");
        };
        let targets: BTreeSet<&str> = missing
            .iter()
            .map(|m| m.params["to"].as_str().unwrap())
            .collect();
        assert_eq!(targets, BTreeSet::from(["de", "es"]));
        assert_eq!(status.missing_tasks(), ["text.translate"]);
        assert!(status.lacks(&want[2]));
        assert!(!status.lacks(&want[1]));
        assert!(!status.lacks(&want[0]));
        assert!(!ChainStatus::Ready.lacks(&want[2]));
        assert!(ChainStatus::Ready.missing_tasks().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_model_a_share_withdraws_is_unbound_and_one_it_cannot_answer_for_is_kept() {
        let dir = root("withdrawn");
        let s = share(
            &dir,
            "myna-text",
            json!([model("tagger", "text.cleanup", 5)]),
            Mode::Works,
        );
        let resolver = Resolver::open(vec![dir.clone()]);
        let request = Request::cleanup(Some("en".into()));
        resolver.settle(std::slice::from_ref(&request)).await;
        assert!(bound(&resolver, &request).is_some());
        // The share cannot be reached: its binding stands.
        std::fs::remove_file(dir.join("myna-text/provider.env")).unwrap();
        resolver.settle(std::slice::from_ref(&request)).await;
        assert!(bound(&resolver, &request).is_some());
        std::fs::write(
            dir.join("myna-text/provider.env"),
            "SNAP_NAME=myna-text\nMYNA_ROLE=text\nUNIX_SOCKET=text.sock\n",
        )
        .unwrap();
        *s.models.lock().unwrap() = json!([]);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert_eq!(bound(&resolver, &request), None);
        assert!(resolver.status().borrow().lacks(&request));
        *s.models.lock().unwrap() = json!([model("tagger", "text.cleanup", 5)]);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert!(bound(&resolver, &request).is_some());
        assert_eq!(*resolver.status().borrow(), ChainStatus::Ready);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_model_removed_mid_chain_types_what_the_rest_made_and_recovers() {
        use super::super::graph::Graph;
        use super::super::pipeline::Pipeline;
        use super::super::{Field, Processor};
        use myna_core::ProcessingChain;

        let dir = root("mid-chain");
        share(
            &dir,
            "myna-text",
            json!([model("tagger", "text.cleanup", 5)]),
            Mode::Works,
        );
        let translation = share(
            &dir,
            "myna-translation",
            json!([model("fr", "text.translate", 5)]),
            Mode::Works,
        );
        let resolver = Resolver::open(vec![dir.clone()]);
        let graph = Graph::preset(ProcessingChain::TIDY_FRENCH);
        resolver.settle(&graph.requests()).await;
        assert_eq!(*resolver.status().borrow(), ChainStatus::Ready);
        let mut pipeline = Pipeline::new(graph.clone(), resolver.clone());
        let field = Field::default();
        let heard = "so I think we should go";
        assert_eq!(
            pipeline.process(heard.into(), &field).await,
            "so I think we should go!!"
        );

        // The component is removed: its mount, and so its socket, is gone.
        std::fs::remove_file(translation.dir.join("text.sock")).unwrap();
        let typed = pipeline.process(heard.into(), &field).await;
        assert_eq!(
            typed, "so I think we should go!",
            "the cleaned text is typed"
        );
        let trace = pipeline.trace().lock().unwrap().clone();
        assert_eq!(trace.last().unwrap().task, "text.translate");
        assert_eq!(
            trace.last().unwrap().skipped,
            Some(myna_core::history::Skip::NoModel)
        );

        // Settings tells the daemon; the chain names the missing request.
        *translation.models.lock().unwrap() = json!([]);
        resolver.settle(&graph.requests()).await;
        let status = resolver.status().borrow().clone();
        assert_eq!(status.missing_tasks(), ["text.translate"]);

        // Reinstalled: a new socket, and the next text is translated again.
        let again = share(
            &dir,
            "myna-translation",
            json!([model("fr", "text.translate", 5)]),
            Mode::Works,
        );
        resolver.settle(&graph.requests()).await;
        assert_eq!(*resolver.status().borrow(), ChainStatus::Ready);
        assert_eq!(
            pipeline.process(heard.into(), &field).await,
            "so I think we should go!!"
        );
        assert!(again.runs.load(Ordering::SeqCst) >= 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn the_signature_moves_with_a_share_and_steps_publish_once() {
        let dir = root("signature");
        let resolver = Resolver::open(vec![dir.clone()]);
        let empty = resolver.signature();
        assert_eq!(resolver.signature(), empty);
        let s = share(&dir, "a", json!([]), Mode::Works);
        let one = resolver.signature();
        assert_ne!(one, empty);
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(
            s.dir.join("provider.env"),
            "SNAP_NAME=a\nMYNA_ROLE=text\nUNIX_SOCKET=text.sock\n# x\n",
        )
        .unwrap();
        assert_ne!(resolver.signature(), one);
        assert!(Resolver::open(vec![dir.join("none")]).signature().len() == 1);

        let mut seen = resolver.catalog();
        seen.borrow_and_update();
        let steps = vec![StepReport {
            id: "tidy".into(),
            state: super::super::steps::StepState::Applicable,
        }];
        resolver.set_steps(steps.clone());
        assert!(seen.has_changed().unwrap());
        seen.borrow_and_update();
        resolver.set_steps(steps);
        assert!(!seen.has_changed().unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_component_appearing_flips_the_chain_to_ready_without_a_call() {
        let dir = root("watch");
        let s = share(&dir, "myna-translation", json!([]), Mode::Works);
        let resolver = Resolver::open(vec![dir.clone()]);
        let wake = Arc::new(tokio::sync::Notify::new());
        let ran = Arc::new(AtomicUsize::new(0));
        let watcher = {
            let (resolver, wake, ran) = (resolver.clone(), wake.clone(), ran.clone());
            tokio::spawn(async move {
                let cadence = Cadence {
                    poll: Duration::from_millis(20),
                    retry: Duration::from_secs(600),
                };
                resolver
                    .keep_bound(
                        &wake,
                        cadence,
                        || vec![Request::translate("en", "fr")],
                        |_| {
                            ran.fetch_add(1, Ordering::SeqCst);
                        },
                    )
                    .await
            })
        };
        let mut status = resolver.status();
        let mut until = async |ready: bool| {
            tokio::time::timeout(Duration::from_secs(3), async {
                while (*status.borrow_and_update() == ChainStatus::Ready) != ready {
                    status.changed().await.unwrap();
                }
            })
            .await
            .expect("the status followed the share")
        };
        until(false).await;
        let settled = ran.load(Ordering::SeqCst);
        // Idle and degraded within the retry window: nothing re-settles.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(ran.load(Ordering::SeqCst), settled);
        std::thread::sleep(Duration::from_millis(20));
        *s.models.lock().unwrap() = json!([model("fr", "text.translate", 10)]);
        std::fs::write(
            s.dir.join("provider.env"),
            "SNAP_NAME=myna-translation\nMYNA_ROLE=text\nUNIX_SOCKET=text.sock\n# new\n",
        )
        .unwrap();
        until(true).await;
        // A wake settles again whatever the disk says.
        let before = ran.load(Ordering::SeqCst);
        wake.notify_one();
        tokio::time::timeout(Duration::from_secs(3), async {
            while ran.load(Ordering::SeqCst) == before {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a wake settles");
        watcher.abort();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_still_unbound_request_is_retried_after_the_retry_interval() {
        let dir = root("retry");
        let resolver = Resolver::open(vec![dir.clone()]);
        let wake = Arc::new(tokio::sync::Notify::new());
        let ran = Arc::new(AtomicUsize::new(0));
        let watcher = {
            let (resolver, wake, ran) = (resolver.clone(), wake.clone(), ran.clone());
            tokio::spawn(async move {
                let cadence = Cadence {
                    poll: Duration::from_millis(10),
                    retry: Duration::from_millis(60),
                };
                resolver
                    .keep_bound(
                        &wake,
                        cadence,
                        || vec![Request::cleanup(None)],
                        |_| {
                            ran.fetch_add(1, Ordering::SeqCst);
                        },
                    )
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            while ran.load(Ordering::SeqCst) < 3 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("retried while degraded");
        watcher.abort();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn only_the_share_that_withdrew_a_model_loses_its_binding() {
        let dir = root("same-id");
        let a = share(
            &dir,
            "a",
            json!([model("m", "text.cleanup", 5)]),
            Mode::Works,
        );
        share(
            &dir,
            "b",
            json!([model("m", "text.cleanup", 50)]),
            Mode::Works,
        );
        let resolver = Resolver::open(vec![dir.clone()]);
        let request = Request::cleanup(Some("en".into()));
        resolver.settle(std::slice::from_ref(&request)).await;
        assert_eq!(bound(&resolver, &request).as_deref(), Some("a/m"));
        *a.models.lock().unwrap() = json!([model("other", "text.translate", 5)]);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert_eq!(bound(&resolver, &request).as_deref(), Some("b/m"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn the_catalog_is_published_when_the_models_change_and_not_otherwise() {
        let dir = root("catalog-change");
        let s = share(&dir, "a", json!([]), Mode::Works);
        let resolver = Resolver::open(vec![dir.clone()]);
        let mut seen = resolver.catalog();
        seen.borrow_and_update();
        resolver.settle(&[]).await;
        assert!(!seen.has_changed().unwrap());
        *s.models.lock().unwrap() = json!([model("m", "text.cleanup", 5)]);
        resolver.settle(&[]).await;
        assert!(seen.has_changed().unwrap());
        assert_eq!(seen.borrow_and_update().models.len(), 1);
        resolver.settle(&[]).await;
        assert!(!seen.has_changed().unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn spoofed(root: &Path, dir: &str, snap: &str) {
        let share = share(
            root,
            dir,
            json!([model("t", "text.cleanup", 5)]),
            Mode::Works,
        );
        drop(share);
        std::fs::write(
            root.join(dir).join("provider.env"),
            format!("SNAP_NAME={snap}\nMYNA_ROLE=text\nUNIX_SOCKET=text.sock\n"),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn only_curated_snaps_are_bound() {
        let dir = root("curated");
        share(
            &dir,
            "myna-text",
            json!([model("tagger", "text.cleanup", 5)]),
            Mode::Works,
        );
        share(
            &dir,
            "myna-translation",
            json!([model("fr", "text.translate", 10)]),
            Mode::Works,
        );
        share(
            &dir,
            "evil",
            json!([model("e", "text.cleanup", 1)]),
            Mode::Works,
        );
        chat_share(&dir, "gemma4", "gemma4-e4b");
        chat_share(&dir, "qwen3", "qwen3-chat");
        let resolver = Resolver::new(vec![dir.clone()]);
        let want = [Request::cleanup(None), Request::translate("en", "fr")];
        resolver.settle(&want).await;
        assert_eq!(
            resolver.bound_to(&want[0]).as_deref(),
            Some("myna-text/tagger")
        );
        assert_eq!(
            resolver.bound_to(&want[1]).as_deref(),
            Some("myna-translation/fr")
        );
        let shares: BTreeSet<String> = resolver
            .catalog()
            .borrow()
            .models
            .iter()
            .map(|m| m.share.clone())
            .collect();
        assert_eq!(
            shares,
            ["gemma4", "myna-text", "myna-translation"]
                .map(String::from)
                .into()
        );
        assert_eq!(*resolver.status().borrow(), ChainStatus::Ready);
    }

    #[tokio::test]
    async fn an_ignored_share_neither_degrades_the_chain_nor_asks_for_a_restart() {
        let dir = root("ignored");
        share(
            &dir,
            "evil",
            json!([model("e", "text.cleanup", 1)]),
            Mode::Works,
        );
        let resolver = Resolver::new(vec![dir.clone()]);
        let request = Request::cleanup(None);
        let (candidates, needs_restart, answered) = resolver.candidates(request.bind_budget).await;
        assert!(candidates.is_empty() && !needs_restart && answered.is_empty());
        assert!(resolver.0.ignored.lock().unwrap().contains("evil"));
        assert!(resolver.bound_to(&request).is_none());
    }

    #[tokio::test]
    async fn a_dev_share_dir_bypasses_the_list() {
        let dir = root("open-dir");
        let open = root("open-extra");
        share(
            &dir,
            "evil",
            json!([model("e", "text.cleanup", 1)]),
            Mode::Works,
        );
        share(
            &open,
            "mine",
            json!([model("m", "text.cleanup", 2)]),
            Mode::Works,
        );
        let resolver = Resolver::with_open(vec![dir.clone()], vec![open.clone()]);
        let request = Request::cleanup(None);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert_eq!(resolver.bound_to(&request).as_deref(), Some("mine/m"));
        let shares: BTreeSet<String> = resolver
            .catalog()
            .borrow()
            .models
            .iter()
            .map(|m| m.share.clone())
            .collect();
        assert_eq!(shares, ["mine".to_string()].into());
    }

    #[tokio::test]
    async fn the_name_is_the_one_provider_env_declares() {
        let dir = root("spoof");
        spoofed(&dir, "myna-text", "evil");
        spoofed(&dir, "elsewhere", "myna-text");
        let resolver = Resolver::new(vec![dir.clone()]);
        let request = Request::cleanup(None);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert_eq!(resolver.bound_to(&request).as_deref(), Some("myna-text/t"));
        assert!(resolver.0.ignored.lock().unwrap().contains("evil"));
    }

    #[tokio::test]
    async fn a_late_share_in_a_curated_dir_asks_for_a_restart() {
        let dir = root("curated-restart");
        std::fs::create_dir_all(dir.join("late")).unwrap();
        let resolver = Resolver::new(vec![dir.clone()]);
        resolver.settle(&[Request::cleanup(None)]).await;
        assert!(matches!(
            *resolver.status().borrow(),
            ChainStatus::Degraded {
                needs_restart: true,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn the_daemons_resolver_curates_the_backend_dir_and_opens_the_extra_ones() {
        let backend = root("env-backend");
        let extra = root("env-extra");
        share(
            &backend,
            "evil",
            json!([model("e", "text.cleanup", 1)]),
            Mode::Works,
        );
        share(
            &extra,
            "mine",
            json!([model("m", "text.cleanup", 2)]),
            Mode::Works,
        );
        let resolver = Resolver::from_env(Some(&backend), Some(extra.clone().into()));
        let request = Request::cleanup(None);
        resolver.settle(std::slice::from_ref(&request)).await;
        assert_eq!(resolver.bound_to(&request).as_deref(), Some("mine/m"));
        assert!(resolver.0.ignored.lock().unwrap().contains("evil"));
    }
}
