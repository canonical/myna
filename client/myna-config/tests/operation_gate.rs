use myna_config::operation_gate::{OperationCoordinator, OperationKind};

#[test]
fn apply_and_switch_share_one_gate_from_confirmation_through_completion() {
    let gate = OperationCoordinator::new();
    let apply = gate.begin(OperationKind::BackendApply).unwrap();
    assert_eq!(gate.active(), Some(OperationKind::BackendApply));
    assert!(gate.begin(OperationKind::BackendSwitch).is_err());

    assert!(gate.complete(apply.token()));
    let switch = gate.begin(OperationKind::BackendSwitch).unwrap();
    assert!(gate.begin(OperationKind::BackendApply).is_err());
    assert!(gate.complete(switch.token()));
    assert_eq!(gate.active(), None);
}

#[test]
fn cancellation_and_stale_completion_are_safe() {
    let gate = OperationCoordinator::new();
    let first = gate.begin(OperationKind::BackendSwitch).unwrap();
    assert!(gate.cancel(first.token()));
    assert!(first.cancellation().is_cancelled());
    assert!(gate.begin(OperationKind::BackendApply).is_err());

    assert!(gate.complete(first.token()));
    let second = gate.begin(OperationKind::BackendApply).unwrap();
    assert!(!gate.complete(first.token()));
    assert_eq!(gate.active(), Some(OperationKind::BackendApply));
    assert!(gate.complete(second.token()));
}

#[test]
fn abandon_signals_and_releases_only_the_matching_operation() {
    let gate = OperationCoordinator::new();
    let first = gate.begin(OperationKind::BackendApply).unwrap();

    assert!(gate.abandon(first.token()));
    assert!(first.cancellation().is_cancelled());
    let second = gate.begin(OperationKind::BackendSwitch).unwrap();
    assert!(!gate.abandon(first.token()));
    assert_eq!(gate.active(), Some(OperationKind::BackendSwitch));
    assert!(gate.complete(second.token()));
}
