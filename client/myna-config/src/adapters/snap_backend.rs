use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::command::{
    CancellationToken, CommandError, CommandOutput, CommandRequest, CommandRunner,
};
use crate::diagnostics::{parse_snap_list, InstalledSnap};
use crate::domain::{
    parse_connections, parse_engine_options, parse_model_options, parse_modelctl_config,
    parse_status, BackendIdentity, BackendSnapshot, BackendStatus, BackendSurface,
    BackendSurfaceError, ConnectionSnapshot, ParseError,
};
use crate::ports::BackendRepository;

const MAX_MODELCTL_CANDIDATES: usize = 4;

pub struct SnapBackendRepository {
    runner: Arc<dyn CommandRunner>,
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
        status: BackendStatus,
        cache_generation: u64,
    },
    CachedStatusFailed {
        app: String,
        error: BackendSurfaceError,
    },
}

impl SnapBackendRepository {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
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
        let request = CommandRequest::new(executable.to_owned(), arguments.clone());
        self.runner
            .run(request, cancellation)
            .await
            .map_err(|error| command_error(surface, executable, arguments, error))
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
            match self.verified_status(&app, cancellation.clone()).await {
                Ok(status) => {
                    return Ok(ModelctlResolution::Verified {
                        app,
                        status,
                        cache_generation: generation,
                    });
                }
                Err(error) => {
                    self.invalidate_cached_app(snap, &app, generation);
                    return Ok(ModelctlResolution::CachedStatusFailed { app, error });
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
                "snap",
                strings(&["info", snap]),
                "installed snap advertises no runnable commands",
                "",
            )]);
        }

        let mut failures = Vec::new();
        for candidate in candidates.into_iter().take(MAX_MODELCTL_CANDIDATES) {
            match self.verified_status(&candidate, cancellation.clone()).await {
                Ok(status) => {
                    let mut cache = self
                        .modelctl_cache
                        .lock()
                        .expect("modelctl app cache lock poisoned");
                    if cache.generation == generation {
                        cache.apps.insert(snap.to_owned(), candidate.clone());
                    }
                    return Ok(ModelctlResolution::Verified {
                        app: candidate,
                        status,
                        cache_generation: generation,
                    });
                }
                Err(error) => failures.push(error),
            }
        }
        failures.push(BackendSurfaceError::new(
            BackendSurface::ModelctlApp,
            "snap",
            strings(&["info", snap]),
            "none of the installed snap commands passed modelctl status verification",
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

    async fn verified_status(
        &self,
        app: &str,
        cancellation: CancellationToken,
    ) -> Result<BackendStatus, BackendSurfaceError> {
        let arguments = modelctl_arguments(app, &["status", "--format=json"]);
        let output = self
            .command(
                BackendSurface::Status,
                "snap",
                arguments.clone(),
                cancellation,
            )
            .await?;
        parse_status(output.stdout())
            .map_err(|error| parse_error(BackendSurface::Status, "snap", arguments, error))
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
                get_arguments.clone(),
                cancellation.clone(),
            )
            .await
        {
            Ok(output) => match parse_modelctl_config(output.stdout()) {
                Ok(config) => snapshot.set_modelctl_config(config),
                Err(error) => snapshot.add_error(parse_error(
                    BackendSurface::ModelctlConfig,
                    "snap",
                    get_arguments,
                    error,
                )),
            },
            Err(error) => snapshot.add_error(error),
        }
        failed |= snapshot.error(BackendSurface::ModelctlConfig).is_some();

        let models_arguments = modelctl_arguments(app, &["list-models", "--format=json"]);
        match self
            .command(
                BackendSurface::Models,
                "snap",
                models_arguments.clone(),
                cancellation.clone(),
            )
            .await
        {
            Ok(output) => match parse_model_options(output.stdout()) {
                Ok(models) => snapshot.set_models(models),
                Err(error) => snapshot.add_error(parse_error(
                    BackendSurface::Models,
                    "snap",
                    models_arguments,
                    error,
                )),
            },
            Err(error) => snapshot.add_error(error),
        }
        failed |= snapshot.error(BackendSurface::Models).is_some();

        let engines_arguments = modelctl_arguments(app, &["list-engines", "--format=json"]);
        match self
            .command(
                BackendSurface::Engines,
                "snap",
                engines_arguments.clone(),
                cancellation,
            )
            .await
        {
            Ok(output) => match parse_engine_options(output.stdout()) {
                Ok(engines) => snapshot.set_engines(engines),
                Err(error) => snapshot.add_error(parse_error(
                    BackendSurface::Engines,
                    "snap",
                    engines_arguments,
                    error,
                )),
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
        let arguments = strings(&["list", "--unicode=never"]);
        let request = CommandRequest::new("snap".to_owned(), arguments.clone())
            .with_environment(BTreeMap::from([("LC_ALL".to_owned(), "C".to_owned())]));
        let output = self
            .runner
            .run(request, cancellation)
            .await
            .map_err(|error| {
                command_error(
                    BackendSurface::SnapInventory,
                    "snap",
                    arguments.clone(),
                    error,
                )
            })?;
        parse_snap_list(output.stdout()).map_err(|error| {
            BackendSurfaceError::new(
                BackendSurface::SnapInventory,
                "snap",
                arguments,
                error.to_string(),
                "",
            )
        })
    }

    async fn discover(
        &self,
        cancellation: CancellationToken,
    ) -> Result<ConnectionSnapshot, BackendSurfaceError> {
        let arguments = strings(&["connections", "--all"]);
        let request = CommandRequest::new("snap".to_owned(), arguments.clone())
            .with_environment(BTreeMap::from([("LC_ALL".to_owned(), "C".to_owned())]));
        let output = self
            .runner
            .run(request, cancellation)
            .await
            .map_err(|error| {
                command_error(
                    BackendSurface::Connections,
                    "snap",
                    arguments.clone(),
                    error,
                )
            })?;
        parse_connections(output.stdout())
            .map_err(|error| parse_error(BackendSurface::Connections, "snap", arguments, error))
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
                status,
                cache_generation,
            }) => {
                snapshot.set_identity(BackendIdentity::with_modelctl_app(
                    backend.snap_name(),
                    app.clone(),
                ));
                snapshot.set_status(status);
                if self.modelctl_data(&app, cancellation, &mut snapshot).await {
                    self.invalidate_cached_app(backend.snap_name(), &app, cache_generation);
                }
            }
            Ok(ModelctlResolution::CachedStatusFailed { app, error }) => {
                snapshot.set_identity(BackendIdentity::with_modelctl_app(
                    backend.snap_name(),
                    app.clone(),
                ));
                snapshot.add_error(error);
                self.modelctl_data(&app, cancellation, &mut snapshot).await;
            }
            Err(errors) => {
                for error in errors {
                    snapshot.add_error(error);
                }
                add_unavailable_modelctl_errors(&mut snapshot, backend.snap_name());
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

fn parse_error(
    surface: BackendSurface,
    executable: &str,
    arguments: Vec<String>,
    error: ParseError,
) -> BackendSurfaceError {
    BackendSurfaceError::new(surface, executable, arguments, error.to_string(), "")
}

fn command_error(
    surface: BackendSurface,
    executable: &str,
    arguments: Vec<String>,
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
        "myna-config",
        "command_failure surface={} executable={} kind={}",
        surface_key(surface),
        executable,
        kind
    );
    let stderr = match &error {
        CommandError::NonZero { stderr, .. } => stderr.clone(),
        _ => String::new(),
    };
    BackendSurfaceError::new(surface, executable, arguments, error.to_string(), stderr)
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

fn add_unavailable_modelctl_errors(snapshot: &mut BackendSnapshot, snap: &str) {
    for surface in [
        BackendSurface::Status,
        BackendSurface::ModelctlConfig,
        BackendSurface::Models,
        BackendSurface::Engines,
    ] {
        if snapshot.error(surface).is_none() {
            snapshot.add_error(BackendSurfaceError::new(
                surface,
                "snap",
                Vec::new(),
                format!("modelctl app for {snap} is unavailable"),
                "",
            ));
        }
    }
}
