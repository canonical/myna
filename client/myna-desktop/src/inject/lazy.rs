//! `LazyInjector` - hold the injection backend open only while it is there.
//!
//! [`ibus::IbusInjector`](super::ibus::IbusInjector) is reached over the IBus
//! daemon's private socket, and that daemon comes and goes: it starts *after*
//! the session for a user daemon, it is restarted by `ibus restart`, by an
//! input-source change, and by a GNOME Shell replace. Connecting once at
//! startup makes all three fatal.
//!
//! So the connection is opened at the first Press that needs it and dropped
//! again the moment the backend reports itself unreachable, which puts the
//! reconnect on the next Press. No timer, no backoff: `acquire` is driven by
//! the user asking for dictation, which is exactly when a stale connection is
//! worth replacing and exactly when the user is present to see it fail.

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};

use super::{FocusEvent, InjectError, InjectionTarget, Injector};

/// Opens a fresh connection to an injection backend.
#[async_trait]
pub trait Connect: Send {
    /// Connect, or explain why not.
    async fn connect(&mut self) -> Result<Box<dyn Injector>, InjectError>;

    /// Whether the backend this factory produces has a replacement-safe
    /// preedit region. Answered without connecting: the controller reads it
    /// while building, long before any Press, and it is a property of the
    /// protocol rather than of a live connection.
    fn supports_preedit(&self) -> bool;
}

/// An [`Injector`] that connects on demand through a [`Connect`] factory.
pub struct LazyInjector {
    connect: Box<dyn Connect>,
    inner: Option<Box<dyn Injector>>,
}

impl LazyInjector {
    /// Wrap a connection factory. Nothing is connected until the first
    /// [`Injector::acquire`].
    pub fn new(connect: impl Connect + 'static) -> Self {
        Self {
            connect: Box::new(connect),
            inner: None,
        }
    }

    /// Whether a connection is currently held (tests / diagnostics).
    pub fn is_connected(&self) -> bool {
        self.inner.is_some()
    }

    /// Drop the connection when the backend reported itself unreachable, so
    /// the next `acquire` reconnects. Other errors (a secure field, no
    /// target, a protocol failure) say nothing about the connection's health
    /// and must not cost us the engine registration.
    fn note(&mut self, err: &InjectError) {
        if matches!(err, InjectError::Unavailable(_)) {
            self.inner = None;
        }
    }
}

#[async_trait]
impl Injector for LazyInjector {
    async fn acquire(&mut self) -> Result<InjectionTarget, InjectError> {
        let held = self.inner.is_some();
        if !held {
            self.inner = Some(self.connect.connect().await?);
        }
        let result = self
            .inner
            .as_mut()
            .expect("connected just above")
            .acquire()
            .await;
        match result {
            // A connection we were holding turned out to be dead: IBus was
            // restarted since the last utterance. The user is pressing *now*,
            // so reconnect now and try once more rather than making this
            // press the one that merely notices.
            Err(InjectError::Unavailable(why)) if held => {
                myna_core::dbg_log!("inject", "held connection is stale ({why}); reconnecting");
                self.inner = None;
                self.inner = Some(self.connect.connect().await?);
                let result = self
                    .inner
                    .as_mut()
                    .expect("connected just above")
                    .acquire()
                    .await;
                if let Err(err) = &result {
                    self.note(err);
                }
                result
            }
            Err(err) => {
                self.note(&err);
                Err(err)
            }
            ok => ok,
        }
    }

    async fn set_activity(&mut self, active: bool) {
        if let Some(inner) = &mut self.inner {
            inner.set_activity(active).await;
        }
    }

    async fn commit(&mut self, text: &str) -> Result<(), InjectError> {
        let Some(inner) = &mut self.inner else {
            return Err(InjectError::Unavailable(
                "injection backend disconnected mid-session".into(),
            ));
        };
        let result = inner.commit(text).await;
        if let Err(err) = &result {
            self.note(err);
        }
        result
    }

    async fn set_preedit(&mut self, text: &str) {
        if let Some(inner) = &mut self.inner {
            inner.set_preedit(text).await;
        }
    }

    fn supports_preedit(&self) -> bool {
        self.connect.supports_preedit()
    }

    async fn cancel(&mut self) {
        if let Some(inner) = &mut self.inner {
            inner.cancel().await;
        }
    }

    async fn end(&mut self) {
        if let Some(inner) = &mut self.inner {
            inner.end().await;
        }
    }

    fn focus_events(&mut self) -> BoxStream<'static, FocusEvent> {
        match &mut self.inner {
            Some(inner) => inner.focus_events(),
            // No connection means `acquire` already failed and the controller
            // is aborting this utterance; an empty stream just never fires.
            None => stream::empty().boxed(),
        }
    }
}

/// The shipped factory: a fresh [`IbusInjector`](super::ibus::IbusInjector)
/// per connect.
pub struct IbusConnect;

#[async_trait]
impl Connect for IbusConnect {
    async fn connect(&mut self) -> Result<Box<dyn Injector>, InjectError> {
        super::ibus::IbusInjector::connect()
            .await
            .map(|i| Box::new(i) as Box<dyn Injector>)
    }

    fn supports_preedit(&self) -> bool {
        // Matches `IbusInjector::supports_preedit` - IBus always has one.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inject::mock::MockInjector;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A factory that fails the first `fails` connects, then hands out mocks.
    struct FlakyConnect {
        fails: usize,
        attempts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Connect for FlakyConnect {
        async fn connect(&mut self) -> Result<Box<dyn Injector>, InjectError> {
            let n = self.attempts.fetch_add(1, Ordering::SeqCst);
            if n < self.fails {
                return Err(InjectError::Unavailable("ibus is not up yet".into()));
            }
            Ok(Box::new(MockInjector::new()))
        }

        fn supports_preedit(&self) -> bool {
            true
        }
    }

    fn flaky(fails: usize) -> (LazyInjector, Arc<AtomicUsize>) {
        let attempts = Arc::new(AtomicUsize::new(0));
        let injector = LazyInjector::new(FlakyConnect {
            fails,
            attempts: Arc::clone(&attempts),
        });
        (injector, attempts)
    }

    /// The whole point: constructing the injector connects to nothing, so a
    /// daemon that starts before IBus starts anyway.
    #[tokio::test]
    async fn construction_does_not_connect() {
        let (injector, attempts) = flaky(0);
        assert!(!injector.is_connected());
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
    }

    /// A backend that is down at the first Press is an error for *that*
    /// utterance, and a later Press reconnects - the daemon does not need
    /// restarting to notice IBus came back.
    #[tokio::test]
    async fn a_failed_connect_is_retried_on_the_next_press() {
        let (mut injector, attempts) = flaky(1);

        assert!(matches!(
            injector.acquire().await,
            Err(InjectError::Unavailable(_))
        ));
        assert!(!injector.is_connected());

        injector.acquire().await.expect("second press connects");
        assert!(injector.is_connected());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// An injector whose first `acquire` after construction reports the
    /// connection dead, standing in for one whose IBus went away.
    struct Stale;

    #[async_trait]
    impl Injector for Stale {
        async fn acquire(&mut self) -> Result<InjectionTarget, InjectError> {
            Err(InjectError::Unavailable("Broken pipe".into()))
        }
        async fn set_activity(&mut self, _: bool) {}
        async fn commit(&mut self, _: &str) -> Result<(), InjectError> {
            Err(InjectError::Unavailable("Broken pipe".into()))
        }
        async fn cancel(&mut self) {}
        async fn end(&mut self) {}
        fn focus_events(&mut self) -> BoxStream<'static, FocusEvent> {
            stream::empty().boxed()
        }
    }

    /// Hands out `stale` dead injectors first (each connect succeeds, the
    /// connection then fails on use), then live mocks.
    struct GoesStale {
        stale: usize,
        attempts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Connect for GoesStale {
        async fn connect(&mut self) -> Result<Box<dyn Injector>, InjectError> {
            let n = self.attempts.fetch_add(1, Ordering::SeqCst);
            if n < self.stale {
                Ok(Box::new(Stale))
            } else {
                Ok(Box::new(MockInjector::new()))
            }
        }

        fn supports_preedit(&self) -> bool {
            true
        }
    }

    /// The logout regression (2026-09-06): IBus restarted between two
    /// utterances, the held connection answered every call with "Broken
    /// pipe", and no later press ever reconnected. A held connection that
    /// reports itself dead is replaced within the *same* press.
    #[tokio::test]
    async fn a_stale_held_connection_is_replaced_within_the_press() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut injector = LazyInjector::new(GoesStale {
            stale: 0,
            attempts: Arc::clone(&attempts),
        });
        injector.acquire().await.expect("first press connects");
        injector.end().await;

        // IBus restarts under the held connection.
        injector.inner = Some(Box::new(Stale));

        injector
            .acquire()
            .await
            .expect("reconnected and acquired in one press");
        assert!(injector.is_connected());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// One retry, not a loop: a backend that is dead again on the fresh
    /// connection is this press's error, exactly as a failed connect is.
    #[tokio::test]
    async fn a_fresh_connection_that_is_also_dead_is_not_retried() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut injector = LazyInjector::new(GoesStale {
            stale: 1,
            attempts: Arc::clone(&attempts),
        });
        injector.inner = Some(Box::new(Stale));

        assert!(matches!(
            injector.acquire().await,
            Err(InjectError::Unavailable(_))
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 1, "reconnected once");
        assert!(!injector.is_connected(), "and dropped the dead replacement");
    }

    /// Without a held connection there is nothing stale to replace: a fresh
    /// connection that fails on first use is dropped for the next press.
    #[tokio::test]
    async fn a_fresh_connection_that_fails_on_first_use_is_dropped() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut injector = LazyInjector::new(GoesStale {
            stale: 1,
            attempts: Arc::clone(&attempts),
        });
        assert!(matches!(
            injector.acquire().await,
            Err(InjectError::Unavailable(_))
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(!injector.is_connected());
    }

    /// A connection held across utterances is reused: re-registering the IBus
    /// component per Press would be both slow and visible to the user.
    #[tokio::test]
    async fn a_live_connection_is_reused() {
        let (mut injector, attempts) = flaky(0);
        injector.acquire().await.expect("acquire");
        injector.end().await;
        injector.acquire().await.expect("acquire");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    /// `supports_preedit` is answered by the factory, so the controller can
    /// read it while building - before anything is connected.
    #[tokio::test]
    async fn preedit_support_is_known_before_connecting() {
        let (injector, _) = flaky(0);
        assert!(injector.supports_preedit());
        assert!(!injector.is_connected());
    }
}
