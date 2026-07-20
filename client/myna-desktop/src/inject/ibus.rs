//! `IbusInjector` — the shipped IBus text-injection backend (plan T22).
//!
//! Registers an IBus component + engine over `zbus` (hand-written
//! `org.freedesktop.IBus.*` interfaces — research R1), makes it the active
//! engine on `acquire`, and inserts committed text via `CommitText`. Focus/
//! secure detection and the engine restore-on-teardown are wired in branch
//! 003b (T018) and the safety branch 003e (T036); this foundational branch only
//! declares the type so the module tree and binary wiring compile.

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};

use super::{FocusEvent, InjectError, InjectionTarget, Injector};

/// IBus engine-over-`zbus` injector (implementation lands in T018).
#[derive(Debug, Default)]
pub struct IbusInjector {
    _private: (),
}

impl IbusInjector {
    /// Connect to the IBus daemon and register the myna engine. Returns
    /// `Err(Unavailable)` when IBus is not reachable. (Stub: T018.)
    pub async fn connect() -> Result<Self, InjectError> {
        Err(InjectError::Unavailable("IbusInjector not yet implemented (T018)".into()))
    }
}

#[async_trait]
impl Injector for IbusInjector {
    async fn acquire(&mut self) -> Result<InjectionTarget, InjectError> {
        Err(InjectError::Unavailable("IbusInjector not yet implemented (T018)".into()))
    }

    async fn set_activity(&mut self, _active: bool) {}

    async fn commit(&mut self, _text: &str) -> Result<(), InjectError> {
        Err(InjectError::Unavailable("IbusInjector not yet implemented (T018)".into()))
    }

    fn supports_preedit(&self) -> bool {
        // IBus has a replacement-safe preedit region (R9 seam); commit-only for
        // the MVP, but the capability is advertised.
        true
    }

    async fn cancel(&mut self) {}

    async fn end(&mut self) {}

    fn focus_events(&mut self) -> BoxStream<'static, FocusEvent> {
        stream::empty().boxed()
    }
}
