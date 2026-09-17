//! Env-gated IBus integration suite (`MYNA_IBUS_TESTS=1`).
//!
//! The suite is a real IBus input-context client. `Field` creates and focuses
//! a context and observes what the daemon actually delivers to it, while
//! `IbusInjector` drives the engine side, so a case asserts the text that
//! reaches a field rather than that a signal was sent.
//!
//! It changes the global input engine, so run it only against the private
//! daemon `dev/gated-tests.sh` stands up, never a desktop session:
//!
//! ```sh
//! make test-client-gated
//! # or scoped, from client/ inside the workshop:
//! ../dev/gated-tests.sh cargo test -p myna-desktop --test ibus_hw
//! ```
//!
//! Cases share one daemon and one global input engine, so they take
//! `exclusive()` rather than relying on the caller passing `--test-threads=1`:
//! cargo-mutants runs a bare `cargo test`, and under it the suite went from
//! green to ten failures out of eleven. Every field is closed with its
//! content type reset first: when a focused context loses focus the daemon
//! copies its purpose and hints onto its fake context, which would otherwise
//! carry a PASSWORD into the next case.
//!
//! It skips cleanly when the gate is unset, so the suite compiles and runs as a
//! no-op offline.

use std::time::Duration;

use futures_util::StreamExt;
use myna_desktop::inject::ibus::IbusInjector;
use myna_desktop::inject::{FocusEvent, InjectError, Injector, Target};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, MessageStream};

const IBUS_SERVICE: &str = "org.freedesktop.IBus";
const IBUS_PATH: &str = "/org/freedesktop/IBus";
const IC_IFACE: &str = "org.freedesktop.IBus.InputContext";

/// `IBusCapabilite`: the field renders preedit itself and takes focus.
const CAP_PREEDIT_TEXT: u32 = 1 << 0;
const CAP_FOCUS: u32 = 1 << 3;

/// Served by ibus-engine-simple, which the headless daemon can spawn.
const PRIOR_ENGINE: &str = "xkb:us::eng";

/// The engine `IbusInjector` registers.
const MYNA_ENGINE: &str = "myna-stt";

/// Guards a hang only; every wait ends on the event it awaits.
const HANG_GUARD: Duration = Duration::from_secs(5);

/// How long a focus loss the engine already received may take to surface.
const NOTICE: Duration = Duration::from_secs(1);

const SENTINEL: &str = "after";

/// `IBusInputPurpose::PASSWORD`.
const PURPOSE_PASSWORD: u32 = 8;
/// `IBusInputHints::PRIVATE` and `HIDDEN_TEXT`.
const HINT_PRIVATE: u32 = 1 << 11;
const HINT_HIDDEN_TEXT: u32 = 1 << 12;

/// True when the IBus integration suite is enabled. Unset gate → skip.
fn ibus_enabled() -> bool {
    std::env::var("MYNA_IBUS_TESTS").as_deref() == Ok("1")
}

/// One case at a time, whatever the harness does with threads. A tokio mutex,
/// not a std one: the guard is held across the case's awaits.
static DAEMON: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Hold the daemon for the rest of the case.
async fn exclusive() -> tokio::sync::MutexGuard<'static, ()> {
    DAEMON.lock().await
}

/// What the daemon delivered to a field.
#[derive(Debug, PartialEq)]
enum Seen {
    Commit(String),
    /// Text and the visible flag.
    Preedit(String, bool),
    HidePreedit,
}

/// A focused IBus input context, as a text field in an application holds one.
struct Field {
    conn: Connection,
    stream: MessageStream,
    ic: OwnedObjectPath,
}

impl Field {
    async fn open(purpose: u32, hints: u32) -> Self {
        let address = std::env::var("IBUS_ADDRESS").expect("IBUS_ADDRESS from dev/gated-tests.sh");
        let conn = zbus::conn::Builder::address(address.as_str())
            .expect("parse IBUS_ADDRESS")
            .max_queued(1024)
            .build()
            .await
            .expect("connect the field to IBus");
        // Opened before any call, so no signal to the context can be missed.
        let stream = MessageStream::from(&conn);
        let ic = conn
            .call_method(
                Some(IBUS_SERVICE),
                IBUS_PATH,
                Some(IBUS_SERVICE),
                "CreateInputContext",
                &("myna-ibus-hw",),
            )
            .await
            .expect("CreateInputContext")
            .body()
            .deserialize::<OwnedObjectPath>()
            .expect("input context path");
        let field = Self { conn, stream, ic };
        field
            .ic_call(
                IC_IFACE,
                "SetCapabilities",
                &(CAP_PREEDIT_TEXT | CAP_FOCUS,),
            )
            .await;
        field.set_content_type(purpose, hints).await;
        field.ic_call(IC_IFACE, "FocusIn", &()).await;
        field
    }

    async fn ic_call(
        &self,
        iface: &str,
        member: &str,
        body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
    ) {
        self.conn
            .call_method(Some(IBUS_SERVICE), &self.ic, Some(iface), member, body)
            .await
            .unwrap_or_else(|e| panic!("{iface}.{member} on {}: {e}", self.ic));
    }

    /// The daemon takes the content type only as a write-only property.
    async fn set_content_type(&self, purpose: u32, hints: u32) {
        self.ic_call(
            "org.freedesktop.DBus.Properties",
            "Set",
            &(IC_IFACE, "ContentType", Value::from((purpose, hints))),
        )
        .await;
    }

    async fn focus_out(&self) {
        self.ic_call(IC_IFACE, "FocusOut", &()).await;
    }

    /// Make `PRIOR_ENGINE` global, so a target's `release` has one to restore.
    async fn use_prior_engine(&self) {
        self.conn
            .call_method(
                Some(IBUS_SERVICE),
                IBUS_PATH,
                Some(IBUS_SERVICE),
                "SetGlobalEngine",
                &(PRIOR_ENGINE,),
            )
            .await
            .expect("SetGlobalEngine to the prior engine");
        assert_eq!(global_engine().await.as_deref(), Some(PRIOR_ENGINE));
    }

    /// Receive the daemon's `GlobalEngineChanged` broadcasts.
    async fn watch_global_engine(&self) {
        self.conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "AddMatch",
                &("type='signal',interface='org.freedesktop.IBus',member='GlobalEngineChanged'",),
            )
            .await
            .expect("AddMatch GlobalEngineChanged");
    }

    /// Wait for `GlobalEngineChanged(engine)`. The daemon emits it once the
    /// engine is attached to the focused context and focused, before it
    /// answers `SetGlobalEngine`.
    async fn wait_global_engine(&mut self, engine: &str) {
        let stream = &mut self.stream;
        let changed = async move {
            while let Some(msg) = stream.next().await {
                let msg = msg.expect("message from IBus");
                let header = msg.header();
                if header.message_type() == zbus::message::Type::Signal
                    && header.member().map(|m| m.as_str()) == Some("GlobalEngineChanged")
                    && msg.body().deserialize::<String>().ok().as_deref() == Some(engine)
                {
                    return;
                }
            }
            panic!("IBus closed the field's connection");
        };
        tokio::time::timeout(HANG_GUARD, changed)
            .await
            .unwrap_or_else(|_| panic!("global engine never became {engine}"));
    }

    async fn next(&mut self) -> Seen {
        let ic = self.ic.clone();
        let stream = &mut self.stream;
        let seen = async move {
            while let Some(msg) = stream.next().await {
                let msg = msg.expect("message from IBus");
                let header = msg.header();
                if header.message_type() != zbus::message::Type::Signal
                    || header.path().map(|p| p.as_str()) != Some(ic.as_str())
                {
                    continue;
                }
                let body = msg.body();
                match header.member().map(|m| m.as_str()) {
                    Some("CommitText") => {
                        let text: OwnedValue = body.deserialize().expect("CommitText (v)");
                        return Seen::Commit(ibus_text(text));
                    }
                    Some("UpdatePreeditText") => {
                        let (text, _cursor, visible): (OwnedValue, u32, bool) =
                            body.deserialize().expect("UpdatePreeditText (vub)");
                        let text = ibus_text(text);
                        // The daemon's clear on every engine switch; it carries
                        // no text, and Myna hides preedit rather than send it.
                        if text.is_empty() && !visible {
                            continue;
                        }
                        return Seen::Preedit(text, visible);
                    }
                    Some("HidePreeditText") => return Seen::HidePreedit,
                    _ => {}
                }
            }
            panic!("IBus closed the field's connection");
        };
        tokio::time::timeout(HANG_GUARD, seen)
            .await
            .unwrap_or_else(|_| panic!("nothing delivered to {} within {HANG_GUARD:?}", self.ic))
    }

    /// Prove nothing reached the field since the last `next`: the sentinel
    /// travels the same ordered path, so anything sent before it arrives first.
    async fn expect_only_sentinel(&mut self, target: &mut dyn Target) {
        target.commit(SENTINEL).await.expect("commit the sentinel");
        assert_eq!(self.next().await, Seen::Commit(SENTINEL.into()));
    }

    async fn close(self) {
        self.set_content_type(0, 0).await;
        // Refocus so the reset reaches the fake context even after a focus_out.
        self.ic_call(IC_IFACE, "FocusIn", &()).await;
        self.focus_out().await;
        self.ic_call("org.freedesktop.IBus.Service", "Destroy", &())
            .await;
    }
}

/// The string field of a serialized `IBusText`.
fn ibus_text(value: OwnedValue) -> String {
    match Value::from(value) {
        Value::Structure(s) => match s.fields().get(2) {
            Some(Value::Str(text)) => text.to_string(),
            other => panic!("IBusText without a string: {other:?}"),
        },
        other => panic!("not an IBusText: {other:?}"),
    }
}

async fn global_engine() -> Option<String> {
    IbusInjector::connect()
        .await
        .expect("connect to IBus daemon")
        .global_engine()
        .await
}

/// A focused field of the given content type, the prior engine global, and an
/// injector that has not acquired yet.
async fn session(purpose: u32, hints: u32) -> (Field, IbusInjector) {
    let field = Field::open(purpose, hints).await;
    field.use_prior_engine().await;
    let injector = IbusInjector::connect()
        .await
        .expect("connect to IBus daemon");
    (field, injector)
}

async fn assert_acquire_refused(injector: &mut IbusInjector) {
    let acquired = injector.acquire().await;
    assert!(
        matches!(acquired, Err(InjectError::SecureField)),
        "acquire must refuse a secure field: {acquired:?}"
    );
    assert_eq!(
        global_engine().await.as_deref(),
        Some(PRIOR_ENGINE),
        "a refused acquire restores the prior engine"
    );
}

/// Commit `text` until its result is `refused`, bounded: the daemon writes the
/// content type to the engine asynchronously. Every accepted commit must reach
/// the field. Returns whether the wanted result was seen.
async fn commit_until(
    field: &mut Field,
    target: &mut dyn Target,
    text: &str,
    refused: bool,
) -> bool {
    for _ in 0..100 {
        match target.commit(text).await {
            Err(InjectError::SecureField) if refused => return true,
            Err(InjectError::SecureField) => {}
            Ok(()) => {
                assert_eq!(field.next().await, Seen::Commit(text.into()));
                if !refused {
                    return true;
                }
            }
            Err(other) => panic!("commit {text:?}: {other:?}"),
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[test]
fn gate_skips_cleanly_when_unset() {
    if ibus_enabled() {
        eprintln!(
            "MYNA_IBUS_TESTS set: see ordinary_field_receives_preedit_and_commit for the real assertions"
        );
    } else {
        eprintln!("skipping ibus_hw: set MYNA_IBUS_TESTS=1 with a running IBus daemon");
    }
}

#[tokio::test]
async fn ordinary_field_receives_preedit_and_commit() {
    if !ibus_enabled() {
        eprintln!("skipping ordinary_field_receives_preedit_and_commit: MYNA_IBUS_TESTS unset");
        return;
    }
    let _serial = exclusive().await;
    let (mut field, mut injector) = session(0, 0).await;
    let mut target = injector.acquire().await.expect("acquire an ordinary field");

    target.set_preedit("hel").await;
    assert_eq!(field.next().await, Seen::Preedit("hel".into(), true));
    target.commit("hello").await.expect("commit hello");
    assert_eq!(field.next().await, Seen::HidePreedit);
    assert_eq!(field.next().await, Seen::Commit("hello".into()));
    // No preedit is up, so clearing it sends nothing.
    target.set_preedit("").await;
    field.expect_only_sentinel(target.as_mut()).await;

    target.release().await;
    assert_eq!(
        global_engine().await.as_deref(),
        Some(PRIOR_ENGINE),
        "release restores the prior engine"
    );
    field.close().await;
}

#[tokio::test]
async fn focus_leaving_the_field_ends_the_session() {
    if !ibus_enabled() {
        eprintln!("skipping focus_leaving_the_field_ends_the_session: MYNA_IBUS_TESTS unset");
        return;
    }
    let _serial = exclusive().await;
    let (field, mut injector) = session(0, 0).await;
    let target = injector.acquire().await.expect("acquire an ordinary field");
    let mut events = target.focus_events();

    field.focus_out().await;
    let event = tokio::time::timeout(HANG_GUARD, events.next())
        .await
        .expect("a focus event within the hang guard");
    assert_eq!(event, Some(FocusEvent::FocusOut));

    target.release().await;
    field.close().await;
}

/// A target whose lease a newer acquire superseded no longer owns anything:
/// releasing it must leave the engine alone, because the live target is still
/// writing into the user's field with it. The restoration responsibility moves
/// to the live target rather than dying with the superseded one: the engine the
/// connection displaced is the user's own, never ours, whichever release ends
/// up handing it back.
#[tokio::test]
async fn releasing_a_superseded_target_leaves_the_live_engine_alone() {
    if !ibus_enabled() {
        eprintln!(
            "skipping releasing_a_superseded_target_leaves_the_live_engine_alone: MYNA_IBUS_TESTS unset"
        );
        return;
    }
    let _serial = exclusive().await;
    let (mut field, mut injector) = session(0, 0).await;
    let superseded = injector.acquire().await.expect("acquire the field");
    let mut live = injector
        .acquire()
        .await
        .expect("acquire it again, superseding the first lease");

    superseded.release().await;
    assert_eq!(
        global_engine().await.as_deref(),
        Some(MYNA_ENGINE),
        "the superseded target restored the engine from under the live one"
    );
    live.commit("still ours").await.expect("commit while live");
    assert_eq!(field.next().await, Seen::Commit("still ours".into()));

    live.release().await;
    assert_eq!(
        global_engine().await.as_deref(),
        Some(PRIOR_ENGINE),
        "the last release must hand back the user's own input method, not ours"
    );
    field.close().await;
}

/// The hide on release is conditional on this target actually showing a
/// preedit region: a live one is cleared, and a target that showed none sends
/// nothing ahead of the next utterance's text.
#[tokio::test]
async fn release_clears_a_live_preedit_and_sends_nothing_without_one() {
    if !ibus_enabled() {
        eprintln!(
            "skipping release_clears_a_live_preedit_and_sends_nothing_without_one: MYNA_IBUS_TESTS unset"
        );
        return;
    }
    let _serial = exclusive().await;
    let (mut field, mut injector) = session(0, 0).await;
    let mut target = injector.acquire().await.expect("acquire the field");
    target.set_preedit("unstable").await;
    assert_eq!(field.next().await, Seen::Preedit("unstable".into(), true));

    target.release().await;
    assert_eq!(
        field.next().await,
        Seen::HidePreedit,
        "release left the volatile region showing in the field"
    );

    // Nothing is showing now, so the next release must emit no hide at all.
    // The sentinel travels the same ordered path, so a stray one arrives first.
    let quiet = injector.acquire().await.expect("reacquire the field");
    let seen = sentinel_via_fresh_acquire(&mut field, &mut injector, Some(quiet)).await;
    assert_eq!(
        seen,
        Seen::Commit(SENTINEL.into()),
        "release hid a preedit region it never showed"
    );

    field.close().await;
}

#[tokio::test]
async fn text_never_follows_focus_into_another_field() {
    if !ibus_enabled() {
        eprintln!("skipping text_never_follows_focus_into_another_field: MYNA_IBUS_TESTS unset");
        return;
    }
    let _serial = exclusive().await;
    let (first, mut injector) = session(0, 0).await;
    let mut target = injector.acquire().await.expect("acquire the first field");

    first.focus_out().await;
    let mut other = Field::open(0, 0).await;
    let committed = target.commit("stray").await;

    let seen = sentinel_via_fresh_acquire(&mut other, &mut injector, Some(target)).await;
    assert_eq!(
        seen,
        Seen::Commit(SENTINEL.into()),
        "text acquired for the first field reached the other one (commit returned {committed:?})"
    );
    assert!(
        committed.is_err(),
        "commit after focus left the acquired field must fail: {committed:?}"
    );

    other.close().await;
    first.close().await;
}

#[tokio::test]
async fn focus_lost_while_acquiring_is_not_missed() {
    if !ibus_enabled() {
        eprintln!("skipping focus_lost_while_acquiring_is_not_missed: MYNA_IBUS_TESTS unset");
        return;
    }
    let _serial = exclusive().await;
    let (mut first, injector) = session(0, 0).await;
    first.watch_global_engine().await;
    let acquiring = tokio::spawn(async move {
        let mut injector = injector;
        let acquired = injector.acquire().await;
        (injector, acquired)
    });

    first.wait_global_engine(MYNA_ENGINE).await;
    first.focus_out().await;
    assert!(
        !acquiring.is_finished(),
        "the FocusOut must reach the engine while acquire is still running"
    );
    let (mut injector, mut acquired) = acquiring.await.expect("acquire task");

    let noticed = match &acquired {
        Err(_) => true,
        Ok(target) => {
            let mut events = target.focus_events();
            matches!(
                tokio::time::timeout(NOTICE, events.next()).await,
                Ok(Some(FocusEvent::FocusOut))
            )
        }
    };
    let mut other = Field::open(0, 0).await;
    let committed = match &mut acquired {
        Ok(target) => Some(target.commit("stray").await),
        Err(_) => None,
    };
    let acquired_as = format!("{acquired:?}");
    let seen = sentinel_via_fresh_acquire(&mut other, &mut injector, acquired.ok()).await;
    assert!(
        noticed && seen == Seen::Commit(SENTINEL.into()),
        "focus lost during acquire: acquire returned {acquired_as}, loss noticed: {noticed}, \
         commit returned {committed:?}, then the other field saw {seen:?}"
    );

    other.close().await;
    first.close().await;
}

/// Release `earlier`, acquire `field` afresh and commit the sentinel. It
/// travels the injector's connection after anything committed earlier, so
/// `field` sees that first if it received it.
async fn sentinel_via_fresh_acquire(
    field: &mut Field,
    injector: &mut IbusInjector,
    earlier: Option<Box<dyn Target>>,
) -> Seen {
    if let Some(earlier) = earlier {
        earlier.release().await;
    }
    let mut target = injector
        .acquire()
        .await
        .expect("reacquire for the sentinel");
    target.commit(SENTINEL).await.expect("commit the sentinel");
    let seen = field.next().await;
    target.release().await;
    seen
}

#[tokio::test]
async fn password_field_is_refused_at_acquire() {
    if !ibus_enabled() {
        eprintln!("skipping password_field_is_refused_at_acquire: MYNA_IBUS_TESTS unset");
        return;
    }
    let _serial = exclusive().await;
    let (mut field, mut injector) = session(PURPOSE_PASSWORD, 0).await;
    assert_acquire_refused(&mut injector).await;

    field.set_content_type(0, 0).await;
    let mut target = injector
        .acquire()
        .await
        .expect("acquire the field once ordinary");
    field.expect_only_sentinel(target.as_mut()).await;

    target.release().await;
    field.close().await;
}

#[tokio::test]
async fn field_turning_secure_mid_session_gets_no_text() {
    if !ibus_enabled() {
        eprintln!("skipping field_turning_secure_mid_session_gets_no_text: MYNA_IBUS_TESTS unset");
        return;
    }
    let _serial = exclusive().await;
    let (mut field, mut injector) = session(0, 0).await;
    let mut target = injector.acquire().await.expect("acquire an ordinary field");
    target.commit("hello").await.expect("commit hello");
    assert_eq!(field.next().await, Seen::Commit("hello".into()));

    // PASSWORD rather than HIDDEN_TEXT: the daemon masks preedit and re-sends
    // it when HIDDEN_TEXT changes, which would muddy the sentinel.
    field.set_content_type(PURPOSE_PASSWORD, 0).await;
    assert!(
        commit_until(&mut field, target.as_mut(), "probe", true).await,
        "commit never refused the field after it turned secure"
    );
    target.set_preedit("secret").await;
    let committed = target.commit("secret").await;
    assert!(
        matches!(committed, Err(InjectError::SecureField)),
        "commit into a secure field: {committed:?}"
    );

    field.set_content_type(0, 0).await;
    assert!(
        commit_until(&mut field, target.as_mut(), SENTINEL, false).await,
        "commit never accepted the field after it turned ordinary"
    );

    target.release().await;
    field.close().await;
}

#[tokio::test]
async fn pin_field_marked_hidden_text_is_refused() {
    if !ibus_enabled() {
        eprintln!("skipping pin_field_marked_hidden_text_is_refused: MYNA_IBUS_TESTS unset");
        return;
    }
    let _serial = exclusive().await;
    // GNOME Shell forwards a Wayland PIN as purpose 0 with PRIVATE|HIDDEN_TEXT.
    let (field, mut injector) = session(0, HINT_PRIVATE | HINT_HIDDEN_TEXT).await;
    assert_acquire_refused(&mut injector).await;

    field.close().await;
}

#[tokio::test]
async fn private_field_is_not_refused() {
    if !ibus_enabled() {
        eprintln!("skipping private_field_is_not_refused: MYNA_IBUS_TESTS unset");
        return;
    }
    let _serial = exclusive().await;
    let (mut field, mut injector) = session(0, HINT_PRIVATE).await;
    let mut target = injector.acquire().await.expect("acquire a private field");
    target.commit("hello").await.expect("commit hello");
    assert_eq!(field.next().await, Seen::Commit("hello".into()));

    target.release().await;
    field.close().await;
}

/// Live **visual** probe for the preedit path (R9): shows an underlined
/// preedit, replaces it, then commits — while you watch a real focused field.
/// Run it and click into any editable text field when prompted:
///
/// ```text
/// MYNA_IBUS_TESTS=1 cargo test --workspace --test ibus_hw \
///     ibus_preedit_visual_probe -- --ignored --nocapture
/// ```
///
/// Expected: "unstable one" appears underlined in the field, is *replaced* by
/// "unstable two", then disappears as "probe: committed." is inserted. If the
/// text appears but is NOT underlined, the app renders preedit without
/// attributes (fine); if nothing appears, the app/daemon drops
/// `UpdatePreeditText` - report which app you focused. Takes over the global
/// IME for ~12 s (same caveat as every test in this file).
#[tokio::test]
#[ignore = "manual: needs a person watching a focused field"]
async fn ibus_preedit_visual_probe() {
    if !ibus_enabled() {
        eprintln!("skipping ibus_preedit_visual_probe: MYNA_IBUS_TESTS unset");
        return;
    }
    let _serial = exclusive().await;
    eprintln!("preedit probe: click into an editable text field — acquiring in 5 s…");
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    let mut injector = IbusInjector::connect()
        .await
        .expect("connect to IBus daemon");
    let mut target = match injector.acquire().await {
        Ok(target) => target,
        Err(other) => panic!("unexpected acquire error: {other:?}"),
    };
    eprintln!(">>> showing preedit 'unstable one' (3 s)");
    target.set_preedit("unstable one").await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    eprintln!(">>> replacing with 'unstable two' (3 s)");
    target.set_preedit("unstable two").await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    eprintln!(">>> committing 'probe: committed.' — preedit must clear");
    target.commit("probe: committed.").await.expect("commit");
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    target.release().await;
    eprintln!("probe done — was the preedit visible and underlined?");
}
