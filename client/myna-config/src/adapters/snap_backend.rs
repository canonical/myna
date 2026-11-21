use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use myna_core::provider_env::{parse_env, DeclaredRole, PROVIDER_ENV_FILENAME};

use crate::adapters::snapd_client::{SnapdError, SnapdReads};
use crate::command::{
    CancellationToken, CommandError, CommandOutput, CommandRequest, CommandRunner,
};
use crate::diagnostics::InstalledSnap;
use crate::domain::{
    connection_snapshot_with_roles, parse_engine_options, parse_model_options,
    parse_modelctl_config, parse_status, BackendIdentity, BackendSnapshot, BackendSurface,
    BackendSurfaceError, ConnectionSnapshot, ContentConnections, ContentSlot, ParseError,
};
use crate::ports::BackendRepository;

const DEFAULT_SNAP_ROOT: &str = "/var/snap";
const MAX_MODELCTL_CANDIDATES: usize = 4;

pub struct SnapBackendRepository {
    runner: Arc<dyn CommandRunner>,
    snapd: Arc<dyn SnapdReads>,
    snap_root: PathBuf,
    modelctl_cache: Mutex<ModelctlCache>,
}

#[derive(Default)]
struct ModelctlCache {
    generation: u64,
    apps: BTreeMap<String, String>,
}

enum ModelctlResolution {
    Verified {
        app: String,
        cache_generation: u64,
    },
    CachedProbeFailed {
        app: String,
        error: BackendSurfaceError,
    },
}

impl SnapBackendRepository {
    pub fn new(runner: Arc<dyn CommandRunner>, snapd: Arc<dyn SnapdReads>) -> Self {
        Self::with_snap_root(runner, snapd, DEFAULT_SNAP_ROOT)
    }

    pub fn with_snap_root(
        runner: Arc<dyn CommandRunner>,
        snapd: Arc<dyn SnapdReads>,
        snap_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            runner,
            snapd,
            snap_root: snap_root.into(),
            modelctl_cache: Mutex::new(ModelctlCache::default()),
        }
    }

    async fn command(
        &self,
        surface: BackendSurface,
        executable: &str,
        arguments: Vec<String>,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput, BackendSurfaceError> {
        let request = CommandRequest::new(executable.to_owned(), arguments);
        self.runner
            .run(request, cancellation)
            .await
            .map_err(|error| command_error(surface, executable, error))
    }

    async fn resolve_modelctl(
        &self,
        snap: &str,
        cancellation: CancellationToken,
    ) -> Result<ModelctlResolution, Vec<BackendSurfaceError>> {
        let (generation, cached) = {
            let cache = self
                .modelctl_cache
                .lock()
                .expect("modelctl app cache lock poisoned");
            (cache.generation, cache.apps.get(snap).cloned())
        };
        if let Some(app) = cached {
            match self.verified_modelctl(&app, cancellation.clone()).await {
                Ok(()) => {
                    return Ok(ModelctlResolution::Verified {
                        app,
                        cache_generation: generation,
                    });
                }
                Err(error) => {
                    self.invalidate_cached_app(snap, &app, generation);
                    return Ok(ModelctlResolution::CachedProbeFailed { app, error });
                }
            }
        }

        let info_arguments = strings(&["info", snap]);
        let info = self
            .command(
                BackendSurface::ModelctlApp,
                "snap",
                info_arguments,
                cancellation.clone(),
            )
            .await
            .map_err(|error| vec![error])?;
        let candidates =
            prioritized_modelctl_candidates(parse_snap_commands(info.stdout(), snap), snap);
        if candidates.is_empty() {
            return Err(vec![BackendSurfaceError::new(
                BackendSurface::ModelctlApp,
                "installed snap advertises no runnable commands",
                "",
            )]);
        }

        let mut failures = Vec::new();
        for candidate in candidates.into_iter().take(MAX_MODELCTL_CANDIDATES) {
            match self
                .verified_modelctl(&candidate, cancellation.clone())
                .await
            {
                Ok(()) => {
                    let mut cache = self
                        .modelctl_cache
                        .lock()
                        .expect("modelctl app cache lock poisoned");
                    if cache.generation == generation {
                        cache.apps.insert(snap.to_owned(), candidate.clone());
                    }
                    return Ok(ModelctlResolution::Verified {
                        app: candidate,
                        cache_generation: generation,
                    });
                }
                Err(error) => failures.push(error),
            }
        }
        failures.push(BackendSurfaceError::new(
            BackendSurface::ModelctlApp,
            "none of the installed snap commands answered `modelctl version`",
            "",
        ));
        Err(failures)
    }

    fn invalidate_cached_app(&self, snap: &str, app: &str, generation: u64) {
        let mut cache = self
            .modelctl_cache
            .lock()
            .expect("modelctl app cache lock poisoned");
        if cache.generation == generation && cache.apps.get(snap).map(String::as_str) == Some(app) {
            cache.apps.remove(snap);
        }
    }

    /// Prove `app` is this snap's modelctl. `version` is the only subcommand
    /// that answers on every backend: `status` exits 1 with "no active
    /// engine" before one is selected, which is a state, not a wrong app.
    async fn verified_modelctl(
        &self,
        app: &str,
        cancellation: CancellationToken,
    ) -> Result<(), BackendSurfaceError> {
        let arguments = modelctl_arguments(app, &["version", "--format=json"]);
        self.command(BackendSurface::ModelctlApp, "snap", arguments, cancellation)
            .await
            .map(|_| ())
    }

    async fn modelctl_status(
        &self,
        app: &str,
        cancellation: CancellationToken,
        snapshot: &mut BackendSnapshot,
    ) {
        let arguments = modelctl_arguments(app, &["status", "--format=json"]);
        match self
            .command(BackendSurface::Status, "snap", arguments, cancellation)
            .await
        {
            Ok(output) => match parse_status(output.stdout()) {
                Ok(status) => snapshot.set_status(status),
                Err(error) => snapshot.add_error(parse_error(BackendSurface::Status, error)),
            },
            Err(error) if is_unconfigured(&error) => {}
            Err(error) => snapshot.add_error(error),
        }
    }

    async fn modelctl_data(
        &self,
        app: &str,
        cancellation: CancellationToken,
        snapshot: &mut BackendSnapshot,
    ) -> bool {
        let mut failed = false;
        let get_arguments = modelctl_arguments(app, &["get"]);
        match self
            .command(
                BackendSurface::ModelctlConfig,
                "snap",
                get_arguments,
                cancellation.clone(),
            )
            .await
        {
            Ok(output) => match parse_modelctl_config(output.stdout()) {
                Ok(config) => snapshot.set_modelctl_config(config),
                Err(error) => {
                    snapshot.add_error(parse_error(BackendSurface::ModelctlConfig, error))
                }
            },
            Err(error) => snapshot.add_error(error),
        }
        failed |= snapshot.error(BackendSurface::ModelctlConfig).is_some();

        let models_arguments = modelctl_arguments(app, &["list-models", "--format=json"]);
        match self
            .command(
                BackendSurface::Models,
                "snap",
                models_arguments,
                cancellation.clone(),
            )
            .await
        {
            Ok(output) => match parse_model_options(output.stdout()) {
                Ok(models) => snapshot.set_models(models),
                Err(error) => snapshot.add_error(parse_error(BackendSurface::Models, error)),
            },
            Err(error) if is_unconfigured(&error) => {}
            Err(error) => snapshot.add_error(error),
        }
        failed |= snapshot.error(BackendSurface::Models).is_some();

        let engines_arguments = modelctl_arguments(app, &["list-engines", "--format=json"]);
        match self
            .command(
                BackendSurface::Engines,
                "snap",
                engines_arguments,
                cancellation,
            )
            .await
        {
            Ok(output) => match parse_engine_options(output.stdout()) {
                Ok(engines) => snapshot.set_engines(engines),
                Err(error) => snapshot.add_error(parse_error(BackendSurface::Engines, error)),
            },
            Err(error) => snapshot.add_error(error),
        }
        failed |= snapshot.error(BackendSurface::Engines).is_some();
        failed
    }
}

#[async_trait(?Send)]
impl BackendRepository for SnapBackendRepository {
    async fn installed_snaps(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<InstalledSnap>, BackendSurfaceError> {
        self.snapd
            .installed_snaps(cancellation)
            .await
            .map_err(|error| snapd_error(BackendSurface::SnapInventory, error))
    }

    async fn discover(
        &self,
        cancellation: CancellationToken,
    ) -> Result<ConnectionSnapshot, BackendSurfaceError> {
        let connections = self
            .snapd
            .content_connections(cancellation)
            .await
            .map_err(|error| snapd_error(BackendSurface::Connections, error))?;
        if !connections
            .slots()
            .iter()
            .any(ContentSlot::is_inference_provider)
        {
            return Ok(connection_snapshot_with_roles(
                &connections,
                &BTreeMap::new(),
            ));
        }
        let snap_root = self.snap_root.clone();
        gio::spawn_blocking(move || snapshot_reading_provider_roles(&connections, &snap_root))
            .await
            .map_err(|_| {
                BackendSurfaceError::new(
                    BackendSurface::Connections,
                    "provider role discovery task panicked",
                    "",
                )
            })
    }

    async fn read_snapshot(
        &self,
        backend: &BackendIdentity,
        cancellation: CancellationToken,
    ) -> BackendSnapshot {
        let mut snapshot = BackendSnapshot::empty(backend.clone());
        match self
            .resolve_modelctl(backend.snap_name(), cancellation.clone())
            .await
        {
            Ok(ModelctlResolution::Verified {
                app,
                cache_generation,
            }) => {
                snapshot.set_identity(backend.clone().with_modelctl_app(app.clone()));
                self.modelctl_status(&app, cancellation.clone(), &mut snapshot)
                    .await;
                if self.modelctl_data(&app, cancellation, &mut snapshot).await {
                    self.invalidate_cached_app(backend.snap_name(), &app, cache_generation);
                }
            }
            Ok(ModelctlResolution::CachedProbeFailed { app, error }) => {
                snapshot.set_identity(backend.clone().with_modelctl_app(app.clone()));
                snapshot.add_error(error);
                self.modelctl_status(&app, cancellation.clone(), &mut snapshot)
                    .await;
                self.modelctl_data(&app, cancellation, &mut snapshot).await;
            }
            Err(errors) => {
                for error in errors {
                    snapshot.add_error(error);
                }
            }
        }
        snapshot
    }

    async fn refresh(
        &self,
        cancellation: CancellationToken,
    ) -> Result<ConnectionSnapshot, BackendSurfaceError> {
        {
            let mut cache = self
                .modelctl_cache
                .lock()
                .expect("modelctl app cache lock poisoned");
            cache.generation = cache.generation.wrapping_add(1);
            cache.apps.clear();
        }
        self.discover(cancellation).await
    }
}

fn snapshot_reading_provider_roles(
    connections: &ContentConnections,
    snap_root: &Path,
) -> ConnectionSnapshot {
    let mut roles = BTreeMap::new();
    for slot in connections.slots() {
        if !slot.is_inference_provider() {
            continue;
        }
        if let Some(role) = read_slot_role(snap_root, slot) {
            roles.insert(
                (slot.snap_name().to_owned(), slot.slot_name().to_owned()),
                role,
            );
        }
    }
    connection_snapshot_with_roles(connections, &roles)
}

fn read_slot_role(snap_root: &Path, slot: &ContentSlot) -> Option<DeclaredRole> {
    let snap = slot.snap_name();
    if !is_single_normal_component(snap) {
        return None;
    }
    for raw in slot.share_source_dirs() {
        let Some(dir) = expand_snap_share_dir(snap_root, snap, raw) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(dir.join(PROVIDER_ENV_FILENAME)) else {
            continue;
        };
        if let Some(env) = parse_env(&text) {
            if let Some(role) = DeclaredRole::from_env(&env) {
                return Some(role);
            }
        }
    }
    None
}

fn expand_snap_share_dir(snap_root: &Path, snap: &str, source: &str) -> Option<PathBuf> {
    let (base, rest) = if let Some(rest) = source.strip_prefix("$SNAP_COMMON") {
        (snap_root.join(snap).join("common"), rest)
    } else {
        let rest = source.strip_prefix("$SNAP_DATA")?;
        (snap_root.join(snap).join("current"), rest)
    };
    if rest.is_empty() {
        return Some(base);
    }
    let rel = Path::new(rest.strip_prefix('/')?);
    rel.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        .then(|| base.join(rel))
}

fn is_single_normal_component(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn parse_snap_commands(output: &str, snap: &str) -> Vec<String> {
    let mut in_commands = false;
    let mut commands = Vec::new();
    for line in output.lines() {
        if line == "commands:" {
            in_commands = true;
            continue;
        }
        if in_commands && !line.starts_with(char::is_whitespace) {
            break;
        }
        if !in_commands {
            continue;
        }
        let Some(command) = line.trim().strip_prefix("- ") else {
            continue;
        };
        if (command == snap || command.starts_with(&format!("{snap}.")))
            && !commands.iter().any(|known| known == command)
        {
            commands.push(command.to_owned());
        }
    }
    commands
}

fn prioritized_modelctl_candidates(candidates: Vec<String>, snap: &str) -> Vec<String> {
    let expected_app = snap.strip_prefix("myna-").unwrap_or(snap);
    let is_likely = |candidate: &str| {
        if candidate == snap {
            return true;
        }
        let app = candidate
            .strip_prefix(snap)
            .and_then(|suffix| suffix.strip_prefix('.'))
            .unwrap_or(candidate);
        app == expected_app
            || ["modelctl", "control", "config", "settings"]
                .iter()
                .any(|marker| app.contains(marker))
    };

    let (likely, fallback): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|candidate| is_likely(candidate));
    likely.into_iter().chain(fallback).collect()
}

fn modelctl_arguments(app: &str, arguments: &[&str]) -> Vec<String> {
    std::iter::once("run".to_owned())
        .chain(std::iter::once(app.to_owned()))
        .chain(arguments.iter().map(|value| (*value).to_owned()))
        .collect()
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn parse_error(surface: BackendSurface, error: ParseError) -> BackendSurfaceError {
    BackendSurfaceError::new(surface, error.to_string(), "")
}

fn snapd_error(surface: BackendSurface, error: SnapdError) -> BackendSurfaceError {
    BackendSurfaceError::new(surface, error.to_string(), "")
}

fn command_error(
    surface: BackendSurface,
    executable: &str,
    error: CommandError,
) -> BackendSurfaceError {
    let kind = match &error {
        CommandError::NotFound { .. } => "not_found",
        CommandError::Spawn { .. } => "spawn",
        CommandError::Timeout { .. } => "timeout",
        CommandError::Cancelled => "cancelled",
        CommandError::NonZero { .. } => "nonzero",
        CommandError::InvalidUtf8 { .. } => "invalid_utf8",
        CommandError::FakeScriptExhausted => "fake_exhausted",
    };
    let executable = std::path::Path::new(executable)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("unknown");
    // Structured, suppressed-by-default diagnostic. Users see failures via the
    // Diagnostics UI; this record is only for developers running with
    // `G_MESSAGES_DEBUG=myna-config`.
    gtk4::glib::g_debug!(
        crate::LOG_DOMAIN,
        "command_failure surface={} executable={} kind={}",
        surface_key(surface),
        executable,
        kind
    );
    let stderr = match &error {
        CommandError::NonZero { stderr, .. } => stderr.clone(),
        _ => String::new(),
    };
    BackendSurfaceError::new(surface, error.to_string(), stderr)
}

fn surface_key(surface: BackendSurface) -> &'static str {
    match surface {
        BackendSurface::SnapInventory => "snap_inventory",
        BackendSurface::Connections => "connections",
        BackendSurface::ModelctlApp => "modelctl_app",
        BackendSurface::ModelctlConfig => "modelctl_config",
        BackendSurface::Status => "status",
        BackendSurface::Models => "models",
        BackendSurface::Engines => "engines",
    }
}

/// A backend with no engine selected yet. modelctl exits non-zero for every
/// surface that needs one, which is a state the page already shows ("Active
/// engine: none selected") and not something to repeat as a failure.
fn is_unconfigured(error: &BackendSurfaceError) -> bool {
    let stderr = error.stderr();
    stderr.contains("no active engine") || stderr.contains("engine manifest not found")
}
