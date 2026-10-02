use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::command::{CancellationToken, CommandRequest};
use crate::domain::{
    ActiveBackendState, BackendIdentity, BackendSurfaceError, CommandResult, ConnectionSnapshot,
};
use crate::operation_gate::{OperationCoordinator, OperationKind};
use crate::ports::{
    BackendRepository, SystemConfigurator, SystemConfiguratorError, SystemConfiguratorFailure,
};
use crate::snap_changes::ApplyProgress;

/// Myna's user service. Restarting it through the user's own systemd needs
/// no authorization, where `snap restart` costs a second polkit prompt.
pub const MYNA_USER_UNIT: &str = "snap.myna.myna.service";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrepareSwitchError {
    BackendUnavailable(BackendIdentity),
    Busy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchPlan {
    baseline: ConnectionSnapshot,
    selected: BackendIdentity,
    operations: Vec<CommandRequest>,
}

impl SwitchPlan {
    pub fn new(
        snapshot: &ConnectionSnapshot,
        selected: BackendIdentity,
    ) -> Result<Self, PrepareSwitchError> {
        if !snapshot.backends().contains(&selected) {
            return Err(PrepareSwitchError::BackendUnavailable(selected));
        }

        let connected = connected_backends(snapshot);
        let operations = if connected.as_slice() == [selected.clone()] {
            Vec::new()
        } else {
            let mut operations = connected
                .iter()
                .chain(snapshot.strays())
                .map(|backend| privileged_snap_request("disconnect", backend.slot()))
                .collect::<Vec<_>>();
            operations.push(privileged_snap_request("connect", selected.slot()));
            operations.push(myna_restart_request());
            operations
        };
        Ok(Self {
            baseline: snapshot.clone(),
            selected,
            operations,
        })
    }

    pub fn baseline(&self) -> &ConnectionSnapshot {
        &self.baseline
    }

    pub fn selected(&self) -> &BackendIdentity {
        &self.selected
    }

    pub fn operations(&self) -> &[CommandRequest] {
        &self.operations
    }

    pub fn is_noop(&self) -> bool {
        self.operations.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn with_operations_for_test(
        baseline: ConnectionSnapshot,
        selected: BackendIdentity,
        operations: Vec<CommandRequest>,
    ) -> Self {
        Self {
            baseline,
            selected,
            operations,
        }
    }
}

fn privileged_snap_request(action: &str, slot: String) -> CommandRequest {
    CommandRequest::new(
        "snap".to_owned(),
        vec![action.to_owned(), "myna:backend".to_owned(), slot],
    )
}

pub fn myna_restart_request() -> CommandRequest {
    CommandRequest::new(
        "systemctl".to_owned(),
        vec![
            "--user".to_owned(),
            "restart".to_owned(),
            MYNA_USER_UNIT.to_owned(),
        ],
    )
}

fn connected_backends(snapshot: &ConnectionSnapshot) -> Vec<BackendIdentity> {
    match snapshot.active_state() {
        ActiveBackendState::Disconnected => Vec::new(),
        ActiveBackendState::Connected(backend) => vec![backend],
        ActiveBackendState::MultiplyConnected(backends) => backends,
    }
}

fn selected_is_active(snapshot: &ConnectionSnapshot, selected: &BackendIdentity) -> bool {
    matches!(
        snapshot.active_state(),
        ActiveBackendState::Connected(ref backend) if backend == selected
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwitchOutcome {
    Applied {
        completed: Vec<CommandResult>,
        final_snapshot: ConnectionSnapshot,
    },
    Disagreed {
        completed: Vec<CommandResult>,
        final_snapshot: ConnectionSnapshot,
    },
    StaleDiscovery {
        final_snapshot: ConnectionSnapshot,
    },
    Noop {
        final_snapshot: ConnectionSnapshot,
    },
    Failed {
        completed: Vec<CommandResult>,
        error: SystemConfiguratorError,
        final_snapshot: Option<ConnectionSnapshot>,
        discovery_error: Option<BackendSurfaceError>,
    },
    Cancelled {
        completed: Vec<CommandResult>,
        final_snapshot: Option<ConnectionSnapshot>,
        discovery_error: Option<BackendSurfaceError>,
    },
    FinalDiscoveryFailed {
        completed: Vec<CommandResult>,
        error: BackendSurfaceError,
    },
}

/// What a finished switch tells the user; the radio shows the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitchNotice {
    None,
    /// The model asked for is not the one connected.
    Failed,
    /// The commands ran, but the connections could not be read back.
    Unconfirmed,
}

impl SwitchOutcome {
    pub fn notice(&self) -> SwitchNotice {
        match self {
            Self::Applied { .. } | Self::Noop { .. } | Self::Cancelled { .. } => SwitchNotice::None,
            Self::Failed { .. } | Self::Disagreed { .. } | Self::StaleDiscovery { .. } => {
                SwitchNotice::Failed
            }
            Self::FinalDiscoveryFailed { .. } => SwitchNotice::Unconfirmed,
        }
    }

    pub fn final_snapshot(&self) -> Option<&ConnectionSnapshot> {
        match self {
            Self::Applied { final_snapshot, .. }
            | Self::Disagreed { final_snapshot, .. }
            | Self::StaleDiscovery { final_snapshot }
            | Self::Noop { final_snapshot } => Some(final_snapshot),
            Self::Failed { final_snapshot, .. } | Self::Cancelled { final_snapshot, .. } => {
                final_snapshot.as_ref()
            }
            Self::FinalDiscoveryFailed { .. } => None,
        }
    }
}

pub async fn execute_switch(
    plan: &SwitchPlan,
    configurator: &dyn SystemConfigurator,
    repository: &dyn BackendRepository,
    cancellation: CancellationToken,
) -> SwitchOutcome {
    let preflight = match repository.refresh(CancellationToken::new()).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return SwitchOutcome::FinalDiscoveryFailed {
                completed: Vec::new(),
                error,
            }
        }
    };

    if plan.is_noop() {
        if &preflight == plan.baseline() && selected_is_active(&preflight, plan.selected()) {
            return SwitchOutcome::Noop {
                final_snapshot: preflight,
            };
        }
        return SwitchOutcome::StaleDiscovery {
            final_snapshot: preflight,
        };
    }

    if &preflight != plan.baseline() || !preflight.backends().contains(plan.selected()) {
        return SwitchOutcome::StaleDiscovery {
            final_snapshot: preflight,
        };
    }
    if cancellation.is_cancelled() {
        return cancelled_after_refresh(Vec::new(), repository).await;
    }

    let execution = configurator
        .execute_backend_switch(plan, cancellation.clone())
        .await;
    match execution {
        Ok(completed) => match repository.refresh(CancellationToken::new()).await {
            Ok(final_snapshot) if selected_is_active(&final_snapshot, plan.selected()) => {
                SwitchOutcome::Applied {
                    completed,
                    final_snapshot,
                }
            }
            Ok(final_snapshot) => SwitchOutcome::Disagreed {
                completed,
                final_snapshot,
            },
            Err(error) => SwitchOutcome::FinalDiscoveryFailed { completed, error },
        },
        Err(failure) => failed_after_refresh(failure, repository).await,
    }
}

/// How setting up waits for snapd to finish a change to Myna or a backend:
/// how often it looks, for how long, what it sleeps on in between, and what
/// stops it.
pub struct SnapdWait<'a> {
    pub interval: Duration,
    pub timeout: Duration,
    pub sleep: &'a dyn Fn(Duration) -> Pin<Box<dyn Future<Output = ()>>>,
    pub cancellation: CancellationToken,
}

/// What setting up is doing, as it starts doing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupStage {
    /// Reading the connections and what snapd is still doing.
    Checking,
    /// snapd is still changing Myna or a model, as its next read shows.
    Waiting(ApplyProgress),
    /// Connecting this backend snap to Myna, then restarting Myna.
    Connecting(String),
    /// Restarting Myna against the backend already connected.
    Restarting,
}

/// Why setting up did not leave dictation running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupError {
    /// The user dismissed the connection's polkit prompt, or the wizard
    /// stopped the setup: nothing to report.
    Cancelled,
    /// What went wrong, for the report.
    Failed(String),
    /// snapd was still working on one of the snaps when the wait ran out;
    /// carries its change summary, in snapd's English, for the details.
    Busy(String),
    /// A step that failed as snapd or systemctl reported it, whose report
    /// names that step.
    Step(SystemConfiguratorError),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => {
                formatter.write_str(&gettextrs::gettext("The change was cancelled."))
            }
            Self::Failed(message) => formatter.write_str(message),
            Self::Busy(change) => write!(formatter, "snapd is still busy with \"{change}\""),
            Self::Step(error) => write!(formatter, "{error}"),
        }
    }
}

impl From<String> for SetupError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

/// How setting up left the daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    /// Restarted, so the caller waits for it to come back.
    Restarted,
    /// Left running as it was.
    LeftRunning,
}

/// Leave dictation running on a backend. With none connected, switch to
/// `preferred`, or the first discovered backend without it; the switch
/// restarts Myna. With one connected, only restart, because the daemon may
/// have started before the backend was installed. `report` hears each
/// stage as it starts; a wait cancelled before it ends changes nothing.
pub async fn ensure_backend_active(
    repository: &dyn BackendRepository,
    configurator: &dyn SystemConfigurator,
    preferred: &str,
    wait: &SnapdWait<'_>,
    report: &dyn Fn(SetupStage),
) -> Result<Settled, SetupError> {
    report(SetupStage::Checking);
    let mut snapshot = repository
        .refresh(CancellationToken::new())
        .await
        .map_err(|error| error.message().to_owned())?;
    let snaps = std::iter::once(crate::onboarding::MYNA_SNAP.to_owned())
        .chain(
            snapshot
                .backends()
                .iter()
                .map(|backend| backend.snap_name().to_owned()),
        )
        .collect::<Vec<_>>();
    if wait_for_snapd(configurator, &snaps, wait, report).await? {
        report(SetupStage::Checking);
        snapshot = repository
            .refresh(CancellationToken::new())
            .await
            .map_err(|error| error.message().to_owned())?;
    }
    if let ActiveBackendState::Connected(_) = snapshot.active_state() {
        report(SetupStage::Restarting);
        return configurator
            .restart_myna(CancellationToken::new())
            .await
            .map(|()| Settled::Restarted)
            .map_err(|error| match error {
                SystemConfiguratorError::Cancelled => SetupError::Cancelled,
                error => SetupError::Step(error),
            });
    }
    let backends = snapshot.backends();
    let Some(selected) = backends
        .iter()
        .find(|backend| backend.snap_name() == preferred)
        .or_else(|| backends.first())
        .cloned()
    else {
        return Err(SetupError::Failed(gettextrs::gettext(
            "No model is connected.",
        )));
    };
    let plan = SwitchPlan::new(&snapshot, selected.clone()).map_err(|_| {
        SetupError::Failed(gettextrs::gettext(
            "The model is not available to switch to.",
        ))
    })?;
    report(SetupStage::Connecting(selected.snap_name().to_owned()));
    match execute_switch(&plan, configurator, repository, CancellationToken::new()).await {
        SwitchOutcome::Applied { .. } => Ok(Settled::Restarted),
        SwitchOutcome::Noop { .. } => Ok(Settled::LeftRunning),
        SwitchOutcome::Failed { error, .. } => Err(SetupError::Step(error)),
        SwitchOutcome::FinalDiscoveryFailed { error, .. } => {
            Err(SetupError::Failed(error.message().to_owned()))
        }
        SwitchOutcome::Cancelled { .. } => Err(SetupError::Cancelled),
        SwitchOutcome::Disagreed { .. } | SwitchOutcome::StaleDiscovery { .. } => {
            Err(SetupError::Failed(gettextrs::gettext(
                "The model changed while it was being switched on. Try again.",
            )))
        }
    }
}

/// Wait until snapd has no change in progress on one of `snaps`, and say
/// whether there was one.
async fn wait_for_snapd(
    configurator: &dyn SystemConfigurator,
    snaps: &[String],
    wait: &SnapdWait<'_>,
    report: &dyn Fn(SetupStage),
) -> Result<bool, SetupError> {
    let mut waited = Duration::ZERO;
    loop {
        if wait.cancellation.is_cancelled() {
            return Err(SetupError::Cancelled);
        }
        let changes = configurator
            .changes_in_progress(wait.cancellation.clone())
            .await?;
        let Some(change) = changes.iter().find(|change| change.concerns(snaps)) else {
            return Ok(waited > Duration::ZERO);
        };
        if waited >= wait.timeout {
            return Err(SetupError::Busy(change.summary().to_owned()));
        }
        report(SetupStage::Waiting(change.progress()));
        (wait.sleep)(wait.interval).await;
        waited += wait.interval;
    }
}

async fn cancelled_after_refresh(
    completed: Vec<CommandResult>,
    repository: &dyn BackendRepository,
) -> SwitchOutcome {
    match repository.refresh(CancellationToken::new()).await {
        Ok(snapshot) => SwitchOutcome::Cancelled {
            completed,
            final_snapshot: Some(snapshot),
            discovery_error: None,
        },
        Err(error) => SwitchOutcome::Cancelled {
            completed,
            final_snapshot: None,
            discovery_error: Some(error),
        },
    }
}

async fn failed_after_refresh(
    failure: SystemConfiguratorFailure,
    repository: &dyn BackendRepository,
) -> SwitchOutcome {
    let (completed, error) = failure.into_parts();
    let final_discovery = repository.refresh(CancellationToken::new()).await;
    if error == SystemConfiguratorError::Cancelled {
        return match final_discovery {
            Ok(snapshot) => SwitchOutcome::Cancelled {
                completed,
                final_snapshot: Some(snapshot),
                discovery_error: None,
            },
            Err(error) => SwitchOutcome::Cancelled {
                completed,
                final_snapshot: None,
                discovery_error: Some(error),
            },
        };
    }
    match final_discovery {
        Ok(snapshot) => SwitchOutcome::Failed {
            completed,
            error,
            final_snapshot: Some(snapshot),
            discovery_error: None,
        },
        Err(discovery_error) => SwitchOutcome::Failed {
            completed,
            error,
            final_snapshot: None,
            discovery_error: Some(discovery_error),
        },
    }
}

#[derive(Clone, Debug)]
pub struct SwitchRequest {
    operation_token: u64,
    plan: SwitchPlan,
    cancellation: CancellationToken,
}

impl SwitchRequest {
    pub fn operation_token(&self) -> u64 {
        self.operation_token
    }

    pub fn plan(&self) -> &SwitchPlan {
        &self.plan
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

pub struct ActiveBackendController {
    inner: RefCell<ControllerState>,
    coordinator: OperationCoordinator,
}

struct ControllerState {
    snapshot: ConnectionSnapshot,
    verified: bool,
    generation: u64,
    pending: Option<PendingSwitch>,
}

struct PendingSwitch {
    token: u64,
    selected: BackendIdentity,
}

impl ActiveBackendController {
    pub fn with_coordinator(
        snapshot: ConnectionSnapshot,
        coordinator: OperationCoordinator,
    ) -> Self {
        Self {
            inner: RefCell::new(ControllerState {
                snapshot,
                verified: true,
                generation: 0,
                pending: None,
            }),
            coordinator,
        }
    }

    pub fn snapshot(&self) -> ConnectionSnapshot {
        self.inner.borrow().snapshot.clone()
    }

    pub fn set_snapshot(&self, snapshot: ConnectionSnapshot) -> bool {
        let mut inner = self.inner.borrow_mut();
        if inner.pending.is_some() {
            return false;
        }
        inner.snapshot = snapshot;
        inner.verified = true;
        true
    }

    /// The backend the list marks: a pending switch's target, else the one
    /// connected backend.
    pub fn chosen(&self) -> Option<BackendIdentity> {
        let inner = self.inner.borrow();
        if let Some(pending) = &inner.pending {
            return Some(pending.selected.clone());
        }
        match inner.snapshot.active_state() {
            ActiveBackendState::Connected(active) => Some(active),
            _ => None,
        }
    }

    pub fn begin(&self, selected: BackendIdentity) -> Result<SwitchRequest, PrepareSwitchError> {
        let mut inner = self.inner.borrow_mut();
        if inner.pending.is_some() {
            return Err(PrepareSwitchError::Busy);
        }
        let plan = SwitchPlan::new(&inner.snapshot, selected.clone())?;
        let operation = self
            .coordinator
            .begin(OperationKind::BackendSwitch)
            .map_err(|_| PrepareSwitchError::Busy)?;
        let operation_token = operation.token();
        let cancellation = operation.cancellation();
        inner.generation = operation_token;
        inner.pending = Some(PendingSwitch {
            token: operation_token,
            selected,
        });
        Ok(SwitchRequest {
            operation_token,
            plan,
            cancellation,
        })
    }

    pub fn abandon(&self) {
        let mut inner = self.inner.borrow_mut();
        if let Some(pending) = inner.pending.take() {
            self.coordinator.abandon(pending.token);
        }
    }

    pub fn complete(&self, operation_token: u64, outcome: SwitchOutcome) -> bool {
        let mut inner = self.inner.borrow_mut();
        if inner
            .pending
            .as_ref()
            .is_none_or(|pending| pending.token != operation_token)
        {
            return false;
        }
        inner.pending = None;
        self.coordinator.complete(operation_token);
        if let Some(snapshot) = outcome.final_snapshot() {
            inner.snapshot = snapshot.clone();
            inner.verified = true;
        } else {
            inner.verified = false;
        }
        true
    }

    pub fn switching_to(&self) -> Option<BackendIdentity> {
        let inner = self.inner.borrow();
        inner
            .pending
            .as_ref()
            .map(|pending| pending.selected.clone())
    }

    pub fn verified(&self) -> bool {
        self.inner.borrow().verified
    }
}
