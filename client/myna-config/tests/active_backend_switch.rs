use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use async_trait::async_trait;
use myna_config::active_backend::{
    execute_switch, ActiveBackendController, BackendHealth, PrepareSwitchError, SwitchOutcome,
    SwitchPlan,
};
use myna_config::command::{CancellationToken, CommandRequest};
use myna_config::domain::{
    parse_connections, ActiveBackendState, BackendIdentity, BackendSnapshot, BackendSurface,
    BackendSurfaceError, CommandResult, ConnectionSnapshot,
};
use myna_config::operation_gate::{OperationCoordinator, OperationKind};
use myna_config::ports::{
    BackendRepository, SystemConfigurator, SystemConfiguratorError, SystemConfiguratorFailure,
};

fn connections(slots: &[&str], connected: &[&str]) -> ConnectionSnapshot {
    let mut text = String::from("Interface Plug Slot Notes\n");
    for slot in slots {
        let plug = if connected.contains(slot) {
            "myna:backend"
        } else {
            "-"
        };
        text.push_str(&format!(
            "content[ubustt-socket] {plug} {slot}:ubustt-socket manual\n"
        ));
    }
    parse_connections(&text).unwrap()
}

fn argv(plan: &SwitchPlan) -> Vec<(&str, Vec<&str>)> {
    plan.operations()
        .iter()
        .map(|request| {
            (
                request.executable(),
                request.arguments().iter().map(String::as_str).collect(),
            )
        })
        .collect()
}

#[test]
fn plan_connects_from_zero_connections() {
    let plan = SwitchPlan::new(
        &connections(&["myna-parakeet"], &[]),
        BackendIdentity::new("myna-parakeet"),
    )
    .unwrap();
    assert_eq!(
        argv(&plan),
        [
            (
                "snap",
                vec!["connect", "myna:backend", "myna-parakeet:ubustt-socket"]
            ),
            ("snap", vec!["restart", "myna.myna"])
        ]
    );
}

#[test]
fn plan_switches_one_connection_disconnect_first() {
    let plan = SwitchPlan::new(
        &connections(&["myna-parakeet", "myna-whisper"], &["myna-parakeet"]),
        BackendIdentity::new("myna-whisper"),
    )
    .unwrap();
    assert_eq!(
        argv(&plan),
        [
            (
                "snap",
                vec!["disconnect", "myna:backend", "myna-parakeet:ubustt-socket"]
            ),
            (
                "snap",
                vec!["connect", "myna:backend", "myna-whisper:ubustt-socket"]
            ),
            ("snap", vec!["restart", "myna.myna"])
        ]
    );
}

#[test]
fn plan_disconnects_every_multiple_connection_before_connecting() {
    let plan = SwitchPlan::new(
        &connections(
            &["myna-parakeet", "myna-whisper", "other"],
            &["myna-parakeet", "myna-whisper"],
        ),
        BackendIdentity::new("other"),
    )
    .unwrap();
    assert_eq!(plan.operations().len(), 4);
    assert_eq!(plan.operations()[0].arguments()[0], "disconnect");
    assert_eq!(plan.operations()[1].arguments()[0], "disconnect");
    assert_eq!(plan.operations()[2].arguments()[0], "connect");
    assert_eq!(plan.operations()[3].arguments(), ["restart", "myna.myna"]);
}

#[test]
fn same_backend_as_exactly_one_connection_is_noop() {
    let plan = SwitchPlan::new(
        &connections(&["myna-parakeet"], &["myna-parakeet"]),
        BackendIdentity::new("myna-parakeet"),
    )
    .unwrap();
    assert!(plan.operations().is_empty());
    assert!(plan.is_noop());
}

#[test]
fn selecting_one_of_multiple_connections_still_converges_to_one() {
    let plan = SwitchPlan::new(
        &connections(
            &["myna-parakeet", "myna-whisper"],
            &["myna-parakeet", "myna-whisper"],
        ),
        BackendIdentity::new("myna-parakeet"),
    )
    .unwrap();
    assert_eq!(
        argv(&plan),
        [
            (
                "snap",
                vec!["disconnect", "myna:backend", "myna-parakeet:ubustt-socket"]
            ),
            (
                "snap",
                vec!["disconnect", "myna:backend", "myna-whisper:ubustt-socket"]
            ),
            (
                "snap",
                vec!["connect", "myna:backend", "myna-parakeet:ubustt-socket"]
            ),
            ("snap", vec!["restart", "myna.myna"]),
        ]
    );
}

#[test]
fn missing_selected_backend_is_rejected() {
    assert_eq!(
        SwitchPlan::new(
            &connections(&["myna-parakeet"], &[]),
            BackendIdentity::new("vanished"),
        )
        .unwrap_err(),
        PrepareSwitchError::BackendUnavailable(BackendIdentity::new("vanished"))
    );
}

#[test]
fn preview_is_exact_shell_free_and_honest_about_snapd_authorization() {
    let plan = SwitchPlan::new(
        &connections(&["old$backend", "new;backend"], &["old$backend"]),
        BackendIdentity::new("new;backend"),
    )
    .unwrap();
    let text = plan.confirmation_text();
    assert!(text.contains(r#"["snap", "disconnect", "myna:backend", "old$backend:ubustt-socket"]"#));
    assert!(text.contains(r#"["snap", "connect", "myna:backend", "new;backend:ubustt-socket"]"#));
    assert!(text.contains(r#"["snap", "restart", "myna.myna"]"#));
    assert!(
        text.contains("without a shell") || text.contains("Nothing is executed through a shell")
    );
    assert!(
        text.contains("user service is restarted") && text.contains("new content mount"),
        "confirmation must explain the daemon restart and mount refresh; got: {text}"
    );
    assert!(
        text.contains("snapd may request administrator authorization"),
        "confirmation must honestly warn about snapd auth; got: {text}"
    );
    assert!(
        text.contains("possibly more than once"),
        "confirmation must not promise a single auth; got: {text}"
    );
    assert!(!text.contains(
        "You will give one confirmation in this app and one administrator authorization"
    ));
    assert!(!text.contains("prompt for each command"));
}

#[derive(Clone)]
struct FakeRepository {
    discoveries: Rc<RefCell<VecDeque<Result<ConnectionSnapshot, BackendSurfaceError>>>>,
    calls: Rc<RefCell<usize>>,
}

impl FakeRepository {
    fn new(
        discoveries: impl IntoIterator<Item = Result<ConnectionSnapshot, BackendSurfaceError>>,
    ) -> Self {
        Self {
            discoveries: Rc::new(RefCell::new(discoveries.into_iter().collect())),
            calls: Rc::new(RefCell::new(0)),
        }
    }

    fn calls(&self) -> usize {
        *self.calls.borrow()
    }
}

#[async_trait(?Send)]
impl BackendRepository for FakeRepository {
    async fn discover(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<ConnectionSnapshot, BackendSurfaceError> {
        *self.calls.borrow_mut() += 1;
        self.discoveries.borrow_mut().pop_front().unwrap()
    }

    async fn read_snapshot(
        &self,
        backend: &BackendIdentity,
        _cancellation: CancellationToken,
    ) -> BackendSnapshot {
        BackendSnapshot::new(backend.clone())
    }

    async fn refresh(
        &self,
        cancellation: CancellationToken,
    ) -> Result<ConnectionSnapshot, BackendSurfaceError> {
        self.discover(cancellation).await
    }
}

#[derive(Clone)]
struct FakeConfigurator {
    result: Rc<RefCell<Option<ConfiguratorResult>>>,
    calls: Rc<RefCell<Vec<Vec<CommandRequest>>>>,
}

type ConfiguratorResult = Result<Vec<CommandResult>, SystemConfiguratorFailure>;

impl FakeConfigurator {
    fn returning(result: ConfiguratorResult) -> Self {
        Self {
            result: Rc::new(RefCell::new(Some(result))),
            calls: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<Vec<CommandRequest>> {
        self.calls.borrow().clone()
    }
}

#[async_trait(?Send)]
impl SystemConfigurator for FakeConfigurator {
    async fn execute_backend_switch(
        &self,
        plan: &SwitchPlan,
        _cancellation: CancellationToken,
    ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
        self.calls.borrow_mut().push(plan.operations().to_vec());
        self.result.borrow_mut().take().unwrap()
    }
}

fn success(plan: &SwitchPlan) -> Vec<CommandResult> {
    plan.operations()
        .iter()
        .map(|request| {
            CommandResult::new(
                request.executable(),
                request.arguments().to_vec(),
                Some(0),
                "",
                "",
            )
        })
        .collect()
}

fn error(message: &str) -> BackendSurfaceError {
    BackendSurfaceError::new(BackendSurface::Connections, "snap", vec![], message, "")
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    gtk4::glib::MainContext::new().block_on(future)
}

#[test]
fn execution_rediscovers_before_and_after_and_reports_agreement() {
    let initial = connections(&["old", "new"], &["old"]);
    let final_state = connections(&["old", "new"], &["new"]);
    let plan = SwitchPlan::new(&initial, BackendIdentity::new("new")).unwrap();
    let repository = FakeRepository::new([Ok(initial), Ok(final_state.clone())]);
    let configurator = FakeConfigurator::returning(Ok(success(&plan)));

    let outcome = block_on(execute_switch(
        &plan,
        true,
        &configurator,
        &repository,
        CancellationToken::new(),
    ));

    assert!(matches!(
        outcome,
        SwitchOutcome::Applied { final_snapshot, .. }
            if final_snapshot == final_state
    ));
    assert_eq!(repository.calls(), 2);
    assert_eq!(configurator.calls(), [plan.operations()]);
}

#[test]
fn stale_discovery_and_disappearing_selection_are_blocked_without_privilege() {
    for current in [
        connections(&["old", "new", "external"], &["old", "external"]),
        connections(&["old"], &["old"]),
    ] {
        let original = connections(&["old", "new"], &["old"]);
        let plan = SwitchPlan::new(&original, BackendIdentity::new("new")).unwrap();
        let repository = FakeRepository::new([Ok(current.clone())]);
        let configurator = FakeConfigurator::returning(Ok(vec![]));
        let outcome = block_on(execute_switch(
            &plan,
            true,
            &configurator,
            &repository,
            CancellationToken::new(),
        ));
        assert!(matches!(
            outcome,
            SwitchOutcome::StaleDiscovery { final_snapshot } if final_snapshot == current
        ));
        assert!(configurator.calls().is_empty());
        assert_eq!(repository.calls(), 1);
    }
}

#[test]
fn cached_noop_is_rechecked_and_external_change_is_reported_without_privilege() {
    let cached = connections(&["old", "new"], &["new"]);
    let changed = connections(&["old", "new"], &["old"]);
    let plan = SwitchPlan::new(&cached, BackendIdentity::new("new")).unwrap();
    assert!(plan.is_noop());
    let repository = FakeRepository::new([Ok(changed.clone())]);
    let configurator = FakeConfigurator::returning(Ok(vec![]));

    let outcome = block_on(execute_switch(
        &plan,
        true,
        &configurator,
        &repository,
        CancellationToken::new(),
    ));

    assert!(matches!(
        outcome,
        SwitchOutcome::StaleDiscovery { final_snapshot } if final_snapshot == changed
    ));
    assert_eq!(repository.calls(), 1);
    assert!(configurator.calls().is_empty());
}

#[test]
fn verified_noop_returns_the_final_reread_state_without_authorization() {
    let cached = connections(&["old", "new"], &["new"]);
    let plan = SwitchPlan::new(&cached, BackendIdentity::new("new")).unwrap();
    let repository = FakeRepository::new([Ok(cached.clone())]);
    let configurator = FakeConfigurator::returning(Ok(vec![]));

    let outcome = block_on(execute_switch(
        &plan,
        true,
        &configurator,
        &repository,
        CancellationToken::new(),
    ));

    assert!(matches!(
        outcome,
        SwitchOutcome::Noop { final_snapshot } if final_snapshot == cached
    ));
    assert_eq!(repository.calls(), 1);
    assert!(configurator.calls().is_empty());
}

#[test]
fn every_operation_failure_and_auth_denial_still_rediscover_actual_state() {
    let initial = connections(&["old", "new"], &["old"]);
    let plan = SwitchPlan::new(&initial, BackendIdentity::new("new")).unwrap();
    let disconnected = connections(&["old", "new"], &[]);
    for (completed, error, final_state) in [
        (
            vec![],
            SystemConfiguratorError::execution("pkexec", vec![], Some(1), "disconnect", "failed"),
            initial.clone(),
        ),
        (
            success(&plan)[..1].to_vec(),
            SystemConfiguratorError::execution("pkexec", vec![], Some(1), "connect", "failed"),
            disconnected.clone(),
        ),
        (
            vec![],
            SystemConfiguratorError::authorization_denied("pkexec", vec![], Some(126), "denied"),
            initial.clone(),
        ),
    ] {
        let repository = FakeRepository::new([Ok(initial.clone()), Ok(final_state.clone())]);
        let configurator = FakeConfigurator::returning(Err(SystemConfiguratorFailure::new(
            completed.clone(),
            error.clone(),
        )));
        let outcome = block_on(execute_switch(
            &plan,
            true,
            &configurator,
            &repository,
            CancellationToken::new(),
        ));
        assert!(matches!(
            outcome,
            SwitchOutcome::Failed {
                completed: actual_completed,
                error: actual_error,
                final_snapshot: Some(actual),
                ..
            } if actual_completed == completed && actual_error == error && actual == final_state
        ));
        assert_eq!(repository.calls(), 2);
    }
}

#[test]
fn cancellation_and_confirmation_rejection_rediscover_without_false_rollback() {
    let initial = connections(&["old", "new"], &["old"]);
    let plan = SwitchPlan::new(&initial, BackendIdentity::new("new")).unwrap();
    for confirmed in [false, true] {
        let final_state = connections(&["old", "new"], &[]);
        let repository = FakeRepository::new(if confirmed {
            vec![Ok(initial.clone()), Ok(final_state.clone())]
        } else {
            vec![Ok(final_state.clone())]
        });
        let configurator = FakeConfigurator::returning(Err(SystemConfiguratorFailure::new(
            success(&plan)[..1].to_vec(),
            SystemConfiguratorError::Cancelled,
        )));
        let cancellation = CancellationToken::new();
        if confirmed {
            cancellation.cancel();
        }
        let outcome = block_on(execute_switch(
            &plan,
            confirmed,
            &configurator,
            &repository,
            cancellation,
        ));
        assert!(matches!(
            outcome,
            SwitchOutcome::Cancelled {
                final_snapshot: Some(actual),
                ..
            } if actual == final_state
        ));
    }
}

#[test]
fn partial_reconciliation_and_post_operation_disagreement_are_honest() {
    let initial = connections(&["old", "new", "external"], &["old", "external"]);
    let plan = SwitchPlan::new(&initial, BackendIdentity::new("new")).unwrap();
    let disagreement = connections(&["old", "new", "external"], &["external"]);
    let repository = FakeRepository::new([Ok(initial), Ok(disagreement.clone())]);
    let configurator = FakeConfigurator::returning(Ok(success(&plan)));

    let outcome = block_on(execute_switch(
        &plan,
        true,
        &configurator,
        &repository,
        CancellationToken::new(),
    ));
    assert!(matches!(
        outcome,
        SwitchOutcome::Disagreed { final_snapshot, .. } if final_snapshot == disagreement
    ));
}

#[test]
fn failed_final_rediscovery_is_exposed() {
    let initial = connections(&["old", "new"], &["old"]);
    let plan = SwitchPlan::new(&initial, BackendIdentity::new("new")).unwrap();
    let repository = FakeRepository::new([Ok(initial), Err(error("refresh failed"))]);
    let configurator = FakeConfigurator::returning(Ok(success(&plan)));
    assert!(matches!(
        block_on(execute_switch(
            &plan,
            true,
            &configurator,
            &repository,
            CancellationToken::new(),
        )),
        SwitchOutcome::FinalDiscoveryFailed { .. }
    ));
}

#[test]
fn controller_serializes_operations_ignores_stale_tokens_and_applies_final_discovery() {
    let initial = connections(&["old", "new"], &["old"]);
    let controller = ActiveBackendController::new(initial.clone());
    let first = controller.begin(BackendIdentity::new("new")).unwrap();
    assert_eq!(
        controller.begin(BackendIdentity::new("old")).unwrap_err(),
        PrepareSwitchError::Busy
    );
    controller.cancel();
    assert!(first.cancellation().is_cancelled());
    assert_eq!(
        controller.begin(BackendIdentity::new("new")).unwrap_err(),
        PrepareSwitchError::Busy
    );
    let actual = connections(&["old", "new"], &["new"]);

    assert!(controller.complete(
        first.operation_token(),
        SwitchOutcome::Applied {
            completed: vec![],
            final_snapshot: initial.clone(),
        }
    ));
    let second = controller.begin(BackendIdentity::new("new")).unwrap();
    assert!(!controller.complete(
        first.operation_token(),
        SwitchOutcome::Applied {
            completed: vec![],
            final_snapshot: initial,
        }
    ));
    assert!(controller.complete(
        second.operation_token(),
        SwitchOutcome::Applied {
            completed: vec![],
            final_snapshot: actual.clone(),
        }
    ));
    assert_eq!(
        controller.snapshot().active_state(),
        ActiveBackendState::Connected(BackendIdentity::new("new"))
    );
}

#[test]
fn controller_uses_the_shared_apply_switch_gate() {
    let gate = OperationCoordinator::new();
    let controller = ActiveBackendController::with_coordinator(
        connections(&["old", "new"], &["old"]),
        gate.clone(),
    );
    let apply = gate.begin(OperationKind::BackendApply).unwrap();
    assert_eq!(
        controller.begin(BackendIdentity::new("new")).unwrap_err(),
        PrepareSwitchError::Busy
    );
    assert!(gate.complete(apply.token()));

    let switch = controller.begin(BackendIdentity::new("new")).unwrap();
    assert!(gate.begin(OperationKind::BackendApply).is_err());
    controller.cancel();
    assert!(switch.cancellation().is_cancelled());
    assert_eq!(gate.active(), Some(OperationKind::BackendSwitch));
    assert!(gate.begin(OperationKind::BackendApply).is_err());
    assert!(controller.complete(
        switch.operation_token(),
        SwitchOutcome::Cancelled {
            completed: vec![],
            final_snapshot: Some(connections(&["old", "new"], &["old"])),
            discovery_error: None,
        }
    ));
    assert_eq!(gate.active(), None);
}

#[test]
fn backend_options_include_installed_connection_and_health_context() {
    let controller = ActiveBackendController::new(connections(
        &["myna-parakeet", "myna-whisper"],
        &["myna-parakeet"],
    ));
    controller.set_health("myna-parakeet", BackendHealth::Healthy);
    controller.set_health("myna-whisper", BackendHealth::Degraded);
    let options = controller.options();
    assert_eq!(options.len(), 2);
    assert!(options[0].installed());
    assert!(options[0].connected());
    assert_eq!(options[0].health(), BackendHealth::Healthy);
    assert!(!options[1].connected());
    assert_eq!(options[1].health(), BackendHealth::Degraded);
}

#[test]
fn selector_only_marks_a_backend_selected_for_one_actual_connection() {
    let disconnected =
        ActiveBackendController::new(connections(&["myna-parakeet", "myna-whisper"], &[]));
    assert_eq!(disconnected.selected_index(), None);

    let connected = ActiveBackendController::new(connections(
        &["myna-parakeet", "myna-whisper"],
        &["myna-whisper"],
    ));
    assert_eq!(connected.selected_index(), Some(1));

    let multiple = ActiveBackendController::new(connections(
        &["myna-parakeet", "myna-whisper"],
        &["myna-parakeet", "myna-whisper"],
    ));
    assert_eq!(multiple.selected_index(), None);
}
