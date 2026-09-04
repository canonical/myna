use std::sync::{Arc, Mutex};

use thiserror::Error;

use crate::command::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    BackendApply,
    BackendSwitch,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BeginOperationError {
    #[error("another operation is already active")]
    Busy,
}

#[derive(Clone, Debug)]
pub struct OperationRequest {
    token: u64,
    cancellation: CancellationToken,
}

impl OperationRequest {
    pub fn token(&self) -> u64 {
        self.token
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

#[derive(Clone, Debug, Default)]
pub struct OperationCoordinator {
    inner: Arc<Mutex<CoordinatorState>>,
}

#[derive(Debug, Default)]
struct CoordinatorState {
    next_token: u64,
    active: Option<ActiveOperation>,
}

#[derive(Clone, Debug)]
struct ActiveOperation {
    token: u64,
    kind: OperationKind,
    cancellation: CancellationToken,
}

impl OperationCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin(&self, kind: OperationKind) -> Result<OperationRequest, BeginOperationError> {
        let mut state = self
            .inner
            .lock()
            .expect("operation coordinator lock poisoned");
        if state.active.is_some() {
            return Err(BeginOperationError::Busy);
        }

        state.next_token = state.next_token.wrapping_add(1);
        if state.next_token == 0 {
            state.next_token = 1;
        }

        let cancellation = CancellationToken::new();
        let request = OperationRequest {
            token: state.next_token,
            cancellation: cancellation.clone(),
        };
        state.active = Some(ActiveOperation {
            token: request.token,
            kind,
            cancellation,
        });
        Ok(request)
    }

    pub fn active(&self) -> Option<OperationKind> {
        self.inner
            .lock()
            .expect("operation coordinator lock poisoned")
            .active
            .as_ref()
            .map(|operation| operation.kind)
    }

    pub fn complete(&self, token: u64) -> bool {
        let mut state = self
            .inner
            .lock()
            .expect("operation coordinator lock poisoned");
        if !matches!(state.active.as_ref(), Some(operation) if operation.token == token) {
            return false;
        }
        state.active = None;
        true
    }

    pub fn cancel(&self, token: u64) -> bool {
        let state = self
            .inner
            .lock()
            .expect("operation coordinator lock poisoned");
        let Some(active) = state.active.as_ref() else {
            return false;
        };
        if active.token != token {
            return false;
        }
        active.cancellation.cancel();
        true
    }

    pub fn abandon(&self, token: u64) -> bool {
        let mut state = self
            .inner
            .lock()
            .expect("operation coordinator lock poisoned");
        if !matches!(state.active.as_ref(), Some(operation) if operation.token == token) {
            return false;
        }
        let active = state.active.take().expect("matching active operation");
        active.cancellation.cancel();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{OperationCoordinator, OperationKind};

    #[test]
    fn stale_cancellation_cannot_release_newer_operation() {
        let gate = OperationCoordinator::new();
        let first = gate.begin(OperationKind::BackendApply).unwrap();
        assert!(gate.cancel(first.token()));

        assert!(gate.begin(OperationKind::BackendSwitch).is_err());
        assert!(gate.complete(first.token()));
        let second = gate.begin(OperationKind::BackendSwitch).unwrap();
        assert!(!gate.cancel(first.token()));
        assert_eq!(gate.active(), Some(OperationKind::BackendSwitch));
        assert!(gate.complete(second.token()));
    }

    #[test]
    fn request_keeps_cancellation_token_alive_after_completion() {
        let gate = OperationCoordinator::new();
        let request = gate.begin(OperationKind::BackendApply).unwrap();

        assert!(gate.complete(request.token()));
        assert!(!request.cancellation().is_cancelled());
    }
}
