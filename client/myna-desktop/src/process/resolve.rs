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
use myna_orchestrator::backend::share::{resolve_text_shares, TextProvider};
use tokio::sync::watch;

use super::pipeline::{Answer, ModelError, Models, Request, BIND_BUDGET};
use super::step;
use super::steps::{compile, Compiled, Context, StepReport, StepSpec};
use super::wire::{self, Capability, WireError};

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

/// A model of a connected share that might serve a request.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub socket: PathBuf,
    pub share: String,
    pub capability: Capability,
}

/// Every saved chain as the bus lists it: whether installed models cover
/// its model steps, for dictation in `language`.
pub fn chain_summaries(
    resolver: &Resolver,
    chains: &[myna_core::chains::Chain],
    language: Option<&str>,
) -> Vec<ChainSummary> {
    chains
        .iter()
        .map(|chain| {
            let steps: Vec<StepSpec> = chain.steps.iter().map(StepSpec::of_chain).collect();
            let (missing, unknown) = resolver.readiness(&steps, language);
            ChainSummary {
                id: chain.id.clone(),
                name: chain.name.clone(),
                steps: chain.steps.len(),
                ready: missing.is_empty() && unknown == 0,
                missing,
                unknown,
            }
        })
        .collect()
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
        (latency(a), Reverse(quality(a)))
            .cmp(&(latency(b), Reverse(quality(b))))
            .then_with(|| (&a.share, &a.capability.id).cmp(&(&b.share, &b.capability.id)))
    });
    fits
}

pub use super::status::{Catalog, ChainStatus, ChainSummary, Missing};

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

#[derive(Clone)]
struct Binding {
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

    /// What `steps` need that no installed model covers, bound or not: a
    /// chain that is not active binds nothing, so only coverage tells.
    pub fn readiness(&self, steps: &[StepSpec], language: Option<&str>) -> (Vec<Request>, usize) {
        let models = self.0.catalog.borrow().models.clone();
        let uncovered = |request: &Request| rank(request, models.clone()).is_empty();
        let compiled = compile(
            steps,
            &Context {
                language,
                unavailable: &uncovered,
                uncovered: &uncovered,
            },
        );
        let mut missing = Vec::new();
        let mut unknown = 0;
        for report in compiled.report {
            match report.state {
                super::steps::StepState::Unavailable { request } => missing.push(request),
                super::steps::StepState::Unknown => unknown += 1,
                _ => {}
            }
        }
        (missing, unknown)
    }

    /// Publish the saved chains and the active one.
    pub fn set_chains(&self, chains: Vec<ChainSummary>, active: String) {
        self.0.catalog.send_if_modified(|now| {
            let changed = now.chains != chains || now.active != active;
            now.chains = chains;
            now.active = active;
            changed
        });
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

    /// The connected shares, curated where their directory says so, and
    /// whether one is connected that this process cannot see yet.
    fn scan(&self) -> (Vec<TextProvider>, bool) {
        let mut providers = Vec::new();
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
        (providers, needs_restart)
    }

    /// Ask one share what it serves; `None` when it does not answer.
    async fn probe(provider: &TextProvider, within: Duration) -> Option<Vec<Capability>> {
        match wire::capabilities(&provider.socket, within).await {
            Ok(caps) => Some(caps.models),
            Err(e) => {
                myna_core::info_log!("process", "{}: no models ({e:?})", provider.snap_name);
                None
            }
        }
    }

    /// The models of `provider` this build has a check for, as candidates.
    fn curate(provider: &TextProvider, models: Vec<Capability>) -> Vec<Candidate> {
        models
            .into_iter()
            .filter(|capability| {
                if step::check_of(capability).is_some() {
                    return true;
                }
                myna_core::info_log!(
                    "process",
                    "{}/{}: unknown class, not offered",
                    provider.snap_name,
                    capability.id
                );
                false
            })
            .map(|capability| Candidate {
                socket: provider.socket.clone(),
                share: provider.snap_name.clone(),
                capability,
            })
            .collect()
    }

    /// Everything the connected shares offer, whether a share is connected
    /// that this process cannot see yet, and the shares that answered. One
    /// round trip per share, whichever requests are then bound from it.
    async fn candidates(&self, within: Duration) -> (Vec<Candidate>, bool, BTreeSet<String>) {
        let (providers, needs_restart) = self.scan();
        let mut out = Vec::new();
        let mut answered = BTreeSet::new();
        for provider in providers {
            let Some(models) = Self::probe(&provider, within).await else {
                continue;
            };
            answered.insert(provider.snap_name.clone());
            out.extend(Self::curate(&provider, models));
        }
        (out, needs_restart, answered)
    }

    /// Bind `request`: warm the best-ranked model, and the next one only if
    /// that fails or runs past the bind budget. `None` when nothing covers it
    /// or nothing warms.
    async fn bind(&self, request: &Request) -> (Option<Binding>, bool) {
        let (candidates, needs_restart, _) = self.candidates(request.bind_budget).await;
        let _one = self.0.binding.lock().await;
        (self.bind_held(request, &candidates).await, needs_restart)
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
        let (candidates, _, _) = self.candidates(request.bind_budget).await;
        self.bind_held(request, &candidates).await
    }

    /// Warm from a list already probed this settle, with the binding lock
    /// held.
    async fn bind_held(&self, request: &Request, candidates: &[Candidate]) -> Option<Binding> {
        let key = request.key();
        if let Some(done) = self.bound(&key) {
            return Some(done);
        }
        for candidate in rank(request, candidates.to_vec()).into_iter().take(2) {
            let warmed = wire::warm(
                &candidate.socket,
                &candidate.capability.id,
                &request.task,
                &request.params,
                request.bind_budget,
            )
            .await;
            match warmed {
                Ok(()) => {
                    let binding = Binding {
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
                    return Some(binding);
                }
                Err(e) => myna_core::info_log!(
                    "process",
                    "{}: {}/{} did not warm ({:?})",
                    request.task,
                    candidate.share,
                    candidate.capability.id,
                    e
                ),
            }
        }
        self.0
            .failed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, Instant::now());
        None
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
        let (models, needs_restart, answered) = self.candidates(BIND_BUDGET).await;
        self.forget_withdrawn(&models, &answered);
        let mut missing = BTreeMap::new();
        for request in requests {
            let binding = {
                let _one = self.0.binding.lock().await;
                self.bind_held(request, &models).await
            };
            if binding.is_none() {
                missing.insert(request.key(), Missing::of(request));
            }
        }
        self.0.catalog.send_if_modified(|now| {
            let changed = now.models != models;
            now.models = models;
            changed
        });
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
        let run = wire::run(
            &binding.socket,
            &binding.model,
            &request.task,
            &request.params,
            text,
            request.run_budget,
        )
        .await;
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
        /// How often the share was asked for its models: once per settle.
        probes: Arc<AtomicUsize>,
        models: Arc<Mutex<Value>>,
    }

    #[derive(Clone, Copy)]
    enum Mode {
        Works,
        WarmFails,
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
        let probes = Arc::new(AtomicUsize::new(0));
        let (w, r, p) = (warms.clone(), runs.clone(), probes.clone());
        let models = Arc::new(Mutex::new(models));
        let served = models.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (models, w, r, p) = (served.clone(), w.clone(), r.clone(), p.clone());
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut line = String::new();
                    BufReader::new(read).read_line(&mut line).await.unwrap();
                    let call: Value = serde_json::from_str(&line).unwrap();
                    let reply = match call["op"].as_str().unwrap() {
                        "capabilities" => {
                            p.fetch_add(1, Ordering::SeqCst);
                            json!({"protocol": "myna-text-v1", "models": *models.lock().unwrap()})
                        }
                        "warm" => {
                            w.fetch_add(1, Ordering::SeqCst);
                            match mode {
                                Mode::Works => json!({"ok": true}),
                                Mode::WarmFails => json!({"error": "no memory"}),
                            }
                        }
                        _ => {
                            r.fetch_add(1, Ordering::SeqCst);
                            json!({"text": format!("{}!", call["text"].as_str().unwrap())})
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
            probes,
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
        assert_eq!(
            (
                text.probes.load(Ordering::SeqCst),
                translation.probes.load(Ordering::SeqCst)
            ),
            (1, 1),
            "one round trip per share per settle, not per request"
        );
        assert_eq!(*resolver.status().borrow(), ChainStatus::Ready);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn pair(from: &str, to: &str) -> Value {
        json!({"id": format!("{from}-{to}"), "task": "text.translate",
               "covers": {"language": [from], "to": [to]}, "latency_ms": 20})
    }

    async fn typed(resolver: &Resolver, steps: &[StepSpec], text: &str) -> String {
        use super::super::pipeline::Pipeline;
        use super::super::Processor;
        let graph = resolver.compile(steps, Some("fr")).graph;
        Pipeline::new(graph, resolver.clone())
            .process(text.into())
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
            ["myna-text", "myna-translation"].map(String::from).into()
        );
        assert_eq!(*resolver.status().borrow(), ChainStatus::Ready);
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
}
