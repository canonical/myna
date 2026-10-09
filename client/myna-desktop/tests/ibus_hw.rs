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
//! no-op offline. With the gate set and no daemon answering it FAILS: the gate
//! is a claim that the service is there, and a case that runs against no
//! daemon asserts nothing while reporting green.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{FutureExt, StreamExt};
use myna_desktop::inject::ibus::IbusInjector;
use myna_desktop::inject::{FocusEvent, InjectError, Injector, Target};
use myna_platform::conformance::text_input::{self as suite, FieldKind, FieldView};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, MessageStream};

const IBUS_SERVICE: &str = "org.freedesktop.IBus";
const IBUS_PATH: &str = "/org/freedesktop/IBus";
const IC_IFACE: &str = "org.freedesktop.IBus.InputContext";

/// `IBusCapabilite`: the field renders preedit itself and takes focus.
const CAP_PREEDIT_TEXT: u32 = 1 << 0;
const CAP_FOCUS: u32 = 1 << 3;
/// The field reports the text around its cursor when asked.
const CAP_SURROUNDING_TEXT: u32 = 1 << 5;

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

/// How the daemon this suite talks to is stood up, quoted in every "it is not
/// there" failure so the reader knows what to start.
const HOW_TO_RUN: &str = "dev/gated-tests.sh starts a private ibus-daemon on its own IBUS_ADDRESS \
     and only then sets the gate; run `make test-client-gated`";

/// The daemon the gate promises. `MYNA_IBUS_TESTS=1` is a claim that an IBus
/// daemon is serving on `IBUS_ADDRESS`; if none is, the cases below must fail
/// rather than skip — several of them are written to tolerate a field that
/// stays quiet, so an absent daemon would report green having asserted
/// nothing. One round trip, and it is the one a client really makes.
async fn require_ibus() {
    let address = std::env::var("IBUS_ADDRESS")
        .unwrap_or_else(|_| panic!("MYNA_IBUS_TESTS=1 but IBUS_ADDRESS is unset. {HOW_TO_RUN}"));
    let conn = zbus::conn::Builder::address(address.as_str())
        .unwrap_or_else(|e| panic!("IBUS_ADDRESS is not an address: {address} ({e})"))
        .build()
        .await
        .unwrap_or_else(|e| {
            panic!("MYNA_IBUS_TESTS=1 but no IBus daemon answers on {address} ({e}). {HOW_TO_RUN}")
        });
    // The call every client starts with, and the one `Field` makes. Not a
    // property read: a serving daemon answers `GlobalEngine` with an error
    // until something sets one. The context dies with this connection.
    conn.call_method(
        Some(IBUS_SERVICE),
        IBUS_PATH,
        Some(IBUS_SERVICE),
        "CreateInputContext",
        &("myna-ibus-hw-probe",),
    )
    .await
    .unwrap_or_else(|e| {
        panic!("the IBus daemon on {address} is connected but not serving ({e}). {HOW_TO_RUN}")
    });
}

/// Skip when the gate is unset, saying so; fail when it is set and the daemon
/// it promises is not there.
macro_rules! skip_unless_ibus {
    () => {
        if !ibus_enabled() {
            // cargo attributes this to the case it came from.
            eprintln!("skipped: set MYNA_IBUS_TESTS=1 (needs a running IBus daemon)");
            return;
        }
        require_ibus().await;
    };
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
        Self::open_at(&address, purpose, hints).await
    }

    async fn open_at(address: &str, purpose: u32, hints: u32) -> Self {
        let conn = zbus::conn::Builder::address(address)
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

    /// Drop what the daemon delivered so far, unread.
    fn forget_delivered(&mut self) {
        self.stream = MessageStream::from(&self.conn);
    }

    /// Wait for the daemon to emit `member` to this field.
    async fn wait_signal(&mut self, member: &str) {
        let ic = self.ic.clone();
        let stream = &mut self.stream;
        let seen = async move {
            while let Some(msg) = stream.next().await {
                let msg = msg.expect("message from IBus");
                let header = msg.header();
                if header.message_type() == zbus::message::Type::Signal
                    && header.path().map(|p| p.as_str()) == Some(ic.as_str())
                    && header.member().map(|m| m.as_str()) == Some(member)
                {
                    return;
                }
            }
            panic!("IBus closed the field's connection");
        };
        tokio::time::timeout(HANG_GUARD, seen)
            .await
            .unwrap_or_else(|_| panic!("{member} never reached {}", self.ic));
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

/// What a field shows, rebuilt from what the daemon delivered to it.
#[derive(Debug, Default)]
struct Shown {
    text: String,
    preedit: String,
    preedit_visible: bool,
}

impl Shown {
    fn view(&self) -> FieldView {
        FieldView {
            text: self.text.clone(),
            preedit: if self.preedit_visible {
                self.preedit.clone()
            } else {
                String::new()
            },
        }
    }
}

/// `VoidSymbol` released: a key no engine acts on.
const VOID_SYMBOL: u32 = 0xff_ffff;
const RELEASE_MASK: u32 = 1 << 30;

impl Field {
    /// Apply one message the daemon sent this field to `shown`.
    fn apply(&self, msg: &zbus::Message, shown: &mut Shown) {
        let header = msg.header();
        if header.message_type() != zbus::message::Type::Signal
            || header.path().map(|p| p.as_str()) != Some(self.ic.as_str())
        {
            return;
        }
        let body = msg.body();
        match header.member().map(|m| m.as_str()) {
            Some("CommitText") => {
                let text: OwnedValue = body.deserialize().expect("CommitText (v)");
                shown.text.push_str(&ibus_text(text));
            }
            Some("UpdatePreeditText") => {
                let (text, _cursor, visible): (OwnedValue, u32, bool) =
                    body.deserialize().expect("UpdatePreeditText (vub)");
                shown.preedit = ibus_text(text);
                shown.preedit_visible = visible;
            }
            Some("ShowPreeditText") => shown.preedit_visible = true,
            Some("HidePreeditText") => shown.preedit_visible = false,
            _ => {}
        }
    }

    /// Everything the engine sent before now. A key event travels daemon ->
    /// engine -> daemon, so its reply reaches the field after anything the
    /// engine emitted ahead of answering it.
    async fn settle(&mut self, shown: &mut Shown) {
        let reply = self
            .conn
            .call_method(
                Some(IBUS_SERVICE),
                &self.ic,
                Some(IC_IFACE),
                "ProcessKeyEvent",
                &(VOID_SYMBOL, 0u32, RELEASE_MASK),
            )
            .await
            .expect("ProcessKeyEvent");
        let call = reply.header().reply_serial();
        let ours = |msg: &zbus::Message| {
            msg.header().message_type() == zbus::message::Type::MethodReturn
                && msg.header().reply_serial() == call
        };
        loop {
            let msg = tokio::time::timeout(HANG_GUARD, self.stream.next())
                .await
                .expect("the key event's reply within the hang guard")
                .expect("IBus closed the field's connection")
                .expect("message from IBus");
            if ours(&msg) {
                return;
            }
            self.apply(&msg, shown);
        }
    }

    /// What has already arrived, for a field without focus: the daemon
    /// routes nothing to it any more, so there is nothing to wait for.
    fn drain(&mut self, shown: &mut Shown) {
        while let Some(Some(msg)) = self.stream.next().now_or_never() {
            self.apply(&msg.expect("message from IBus"), shown);
        }
    }
}

/// The conformance suite's hand on a [`Field`], shared with the fixture so it
/// can close the field once the check is over.
struct SuiteField {
    field: Arc<tokio::sync::Mutex<Option<Field>>>,
    focused: bool,
    shown: Shown,
}

#[async_trait]
impl suite::Field for SuiteField {
    async fn lose_focus(&mut self) {
        let mut field = self.field.lock().await;
        let field = field.as_mut().expect("an open field");
        field.focus_out().await;
        self.focused = false;
    }

    async fn focus(&mut self) {
        let mut field = self.field.lock().await;
        let field = field.as_mut().expect("an open field");
        field.ic_call(IC_IFACE, "FocusIn", &()).await;
        self.focused = true;
    }

    async fn blip(&mut self) {
        let mut field = self.field.lock().await;
        let field = field.as_mut().expect("an open field");
        field.focus_out().await;
        tokio::time::sleep(BLIP).await;
        field.ic_call(IC_IFACE, "FocusIn", &()).await;
    }

    async fn grab(&mut self) {
        let mut field = self.field.lock().await;
        let field = field.as_mut().expect("an open field");
        field.focus_out().await;
    }

    async fn ungrab(&mut self) {
        let mut field = self.field.lock().await;
        let field = field.as_mut().expect("an open field");
        field.ic_call(IC_IFACE, "FocusIn", &()).await;
    }

    async fn turn_secure(&mut self) {
        let mut field = self.field.lock().await;
        let field = field.as_mut().expect("an open field");
        field.set_content_type(PURPOSE_PASSWORD, 0).await;
    }

    async fn observe(&mut self) -> Option<FieldView> {
        let mut field = self.field.lock().await;
        let field = field.as_mut().expect("an open field");
        if self.focused {
            field.settle(&mut self.shown).await;
        } else {
            field.drain(&mut self.shown);
        }
        Some(self.shown.view())
    }
}

/// One focused IBus field and a fresh `IbusInjector` per check; the field of
/// the check before is closed first, so no content type outlives it.
#[derive(Default)]
struct IbusFixture {
    open: Option<Arc<tokio::sync::Mutex<Option<Field>>>>,
}

impl IbusFixture {
    async fn close(&mut self) {
        if let Some(open) = self.open.take() {
            if let Some(field) = open.lock().await.take() {
                field.close().await;
            }
        }
    }
}

#[async_trait]
impl suite::Fixture for IbusFixture {
    async fn setup(&mut self, kind: FieldKind) -> (Box<dyn Injector>, Box<dyn suite::Field>) {
        self.close().await;
        let purpose = match kind {
            FieldKind::Plain => 0,
            FieldKind::Secure => PURPOSE_PASSWORD,
        };
        let (field, injector) = session(purpose, 0).await;
        let field = Arc::new(tokio::sync::Mutex::new(Some(field)));
        self.open = Some(Arc::clone(&field));
        let field = SuiteField {
            field,
            focused: true,
            shown: Shown::default(),
        };
        (Box::new(injector), Box::new(field))
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

/// A serialized `IBusText` carrying `text`, as a client sends it.
fn ibus_text_value(text: &str) -> Value<'static> {
    let attributes = zbus::zvariant::StructureBuilder::new()
        .add_field("IBusAttrList".to_string())
        .add_field(std::collections::HashMap::<String, Value<'static>>::new())
        .add_field(Vec::<Value<'static>>::new())
        .build()
        .expect("IBusAttrList");
    Value::from(
        zbus::zvariant::StructureBuilder::new()
            .add_field("IBusText".to_string())
            .add_field(std::collections::HashMap::<String, Value<'static>>::new())
            .add_field(text.to_string())
            .append_field(Value::Value(Box::new(Value::from(attributes))))
            .build()
            .expect("IBusText"),
    )
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

/// The gate read both ways: unset, the suite skips and says so; set, the
/// daemon it promises answers the connection every other case makes.
#[tokio::test]
async fn the_daemon_the_gate_promises_is_serving() {
    skip_unless_ibus!();
    let _serial = exclusive().await;
    let injector = IbusInjector::connect().await.unwrap_or_else(|e| {
        panic!("MYNA_IBUS_TESTS=1 but IbusInjector cannot connect ({e}). {HOW_TO_RUN}")
    });
    assert_eq!(
        injector.capabilities(),
        myna_desktop::inject::ibus::CAPABILITIES
    );
}

#[tokio::test]
async fn ordinary_field_receives_preedit_and_commit() {
    skip_unless_ibus!();
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

/// The engine asks for the field's surrounding text on focus, and the target
/// reports the character before the cursor, which separates a dictation from
/// the one before it.
#[tokio::test]
async fn the_field_reports_the_text_before_its_cursor() {
    skip_unless_ibus!();
    let _serial = exclusive().await;
    let (mut field, mut injector) = session(0, 0).await;
    field
        .ic_call(
            IC_IFACE,
            "SetCapabilities",
            &(CAP_PREEDIT_TEXT | CAP_FOCUS | CAP_SURROUNDING_TEXT,),
        )
        .await;
    // The daemon also asks on an activation that first reads `FocusId`, so
    // only a later one, with both answers cached, shows the engine asking.
    injector
        .acquire()
        .await
        .expect("acquire an ordinary field")
        .release()
        .await;
    field.forget_delivered();
    let target = injector.acquire().await.expect("acquire it again");
    assert_eq!(target.char_before_cursor(), None, "the field has not said");

    field.wait_signal("RequireSurroundingText").await;
    let text = "how things turned.";
    let cursor = text.chars().count() as u32;
    field
        .ic_call(
            IC_IFACE,
            "SetSurroundingText",
            &(ibus_text_value(text), cursor, cursor),
        )
        .await;
    let mut before = None;
    for _ in 0..100 {
        before = target.char_before_cursor();
        if before.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(before, Some('.'));

    target.release().await;
    field.close().await;
}

#[tokio::test]
async fn focus_leaving_the_field_ends_the_session() {
    skip_unless_ibus!();
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

/// How long an X11 key grab holds focus off the field: a key tap.
const BLIP: Duration = Duration::from_millis(150);

/// When the key's command reaches the daemon after the FocusOut its grab
/// caused: 5-9 ms through gdbus, about 80 ms through `myna.toggle` (Xubuntu).
const EDGE_AFTER: Duration = Duration::from_millis(20);

/// A global key grab on X11 sends the focused window FocusOut, then FocusIn
/// once the key is released, and the toolkit relays both to the same input
/// context; the key's Toggle arrives in between. Text written before the
/// Toggle is held, text after it waits, and both land once focus is back.
#[tokio::test]
async fn a_focus_blip_around_an_activation_is_not_a_loss() {
    skip_unless_ibus!();
    let _serial = exclusive().await;
    let (mut field, mut injector) = session(0, 0).await;
    let mut target = injector.acquire().await.expect("acquire an ordinary field");
    let mut events = target.focus_events();
    target
        .commit("before")
        .await
        .expect("commit before the blip");
    assert_eq!(field.next().await, Seen::Commit("before".into()));

    field.focus_out().await;
    tokio::time::sleep(EDGE_AFTER).await;
    let held = target.commit("held").await;
    target.set_preedit("draft").await;
    target.activated();
    let refocus = async {
        tokio::time::sleep(BLIP).await;
        field.ic_call(IC_IFACE, "FocusIn", &()).await;
    };
    let (during, ()) = tokio::join!(target.commit("during"), refocus);
    held.expect("a commit before the Toggle is held");
    during.expect("a commit after the Toggle lands once focus is back");
    assert_eq!(field.next().await, Seen::Commit("held".into()));
    assert_eq!(field.next().await, Seen::Preedit("draft".into(), true));
    assert_eq!(field.next().await, Seen::HidePreedit);
    assert_eq!(field.next().await, Seen::Commit("during".into()));
    field.expect_only_sentinel(target.as_mut()).await;
    assert!(
        events.next().now_or_never().is_none(),
        "the blip was reported as a focus loss"
    );

    target.release().await;
    field.close().await;
}

/// An ibus-daemon of the case's own, so it has never activated Myna's engine.
struct FreshDaemon {
    child: std::process::Child,
    address: String,
    dir: std::path::PathBuf,
}

impl FreshDaemon {
    async fn start() -> Self {
        let dir = std::env::temp_dir().join(format!("myna-fresh-ibus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory for the daemon");
        let address = format!("unix:path={}/bus", dir.display());
        let child = std::process::Command::new("ibus-daemon")
            .args([
                "--panel",
                "disable",
                "--config",
                "disable",
                "--address",
                &address,
            ])
            .env("XDG_CONFIG_HOME", &dir)
            .env("XDG_CACHE_HOME", &dir)
            // It refuses to start where a client would find another daemon.
            .env_remove("IBUS_ADDRESS")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start ibus-daemon");
        let daemon = Self {
            child,
            address,
            dir,
        };
        let serving = async {
            loop {
                if let Ok(builder) = zbus::conn::Builder::address(daemon.address.as_str()) {
                    if builder.build().await.is_ok() {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(HANG_GUARD, serving)
            .await
            .expect("the fresh ibus-daemon never served");
        daemon
    }
}

impl Drop for FreshDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The first dictation after ibus-daemon starts, by a key on X11. The daemon
/// has not read the engine's `FocusId`, so it focuses the engine with a plain
/// `FocusIn` on the fake context the key's grab holds focused; noble's ibus
/// then sends `FocusOutId(fake)`, `FocusInId(field)` at the key's release.
/// The dictation reaches the field.
#[tokio::test]
async fn the_first_activation_on_a_fresh_daemon_reaches_the_field() {
    skip_unless_ibus!();
    let _serial = exclusive().await;
    let daemon = FreshDaemon::start().await;
    let mut field = Field::open_at(&daemon.address, 0, 0).await;
    // The grab: focus leaves the field for the daemon's fake context.
    field.focus_out().await;
    let address = daemon.address.as_str().try_into().expect("address");
    let mut injector = IbusInjector::connect_to(address)
        .await
        .expect("connect to the fresh daemon");

    let release = async {
        tokio::time::sleep(BLIP).await;
        field.ic_call(IC_IFACE, "FocusIn", &()).await;
    };
    let (acquired, ()) = tokio::join!(injector.acquire(), release);
    let mut target = acquired.expect("acquire on a fresh daemon");
    let mut events = target.focus_events();
    // Held if the field's focus has not reached the engine yet.
    target
        .commit("first")
        .await
        .expect("commit the first dictation");
    target.commit(" words").await.expect("commit more");
    tokio::time::sleep(EDGE_AFTER).await;
    assert!(
        events.next().now_or_never().is_none(),
        "the key's release was reported as a focus loss"
    );
    target.release().await;
    assert_eq!(field.next().await, Seen::Commit("first".into()));
    assert_eq!(field.next().await, Seen::Commit(" words".into()));
}

/// A start key held past the grace: the grab keeps the fake context focused
/// until it is released, and its repeats, signalled while the injector
/// acquires, keep the acquire waiting for the field.
#[tokio::test]
async fn a_start_key_held_past_the_grace_still_reaches_the_field() {
    skip_unless_ibus!();
    let _serial = exclusive().await;
    let (mut field, mut injector) = session(0, 0).await;
    field.focus_out().await;
    let activation = injector.activation();
    let hold = async {
        let held = tokio::time::Instant::now();
        while held.elapsed() < Duration::from_millis(2000) {
            tokio::time::sleep(Duration::from_millis(50)).await;
            activation.signal();
        }
        field.ic_call(IC_IFACE, "FocusIn", &()).await;
    };
    let (acquired, ()) = tokio::join!(injector.acquire(), hold);
    let mut target = acquired.expect("acquire with the key held");
    target.commit("held").await.expect("commit after the hold");
    target.release().await;
    assert_eq!(field.next().await, Seen::Commit("held".into()));
    field.close().await;
}

/// Text held through a blip lands at release when nothing is written after.
#[tokio::test]
async fn text_held_through_a_blip_lands_at_release() {
    skip_unless_ibus!();
    let _serial = exclusive().await;
    let (mut field, mut injector) = session(0, 0).await;
    let mut target = injector.acquire().await.expect("acquire an ordinary field");

    field.focus_out().await;
    tokio::time::sleep(EDGE_AFTER).await;
    target.commit("held").await.expect("held");
    target.activated();
    tokio::time::sleep(BLIP).await;
    field.ic_call(IC_IFACE, "FocusIn", &()).await;
    target.release().await;
    assert_eq!(field.next().await, Seen::Commit("held".into()));
    field.close().await;
}

/// Without an activation the same calls may be an application moving focus
/// between fields that share its context, so they end the dictation and
/// nothing written meanwhile lands.
#[tokio::test]
async fn a_focus_blip_without_an_activation_ends_the_dictation() {
    skip_unless_ibus!();
    let _serial = exclusive().await;
    let (mut field, mut injector) = session(0, 0).await;
    let mut target = injector.acquire().await.expect("acquire an ordinary field");
    let mut events = target.focus_events();

    field.focus_out().await;
    tokio::time::sleep(EDGE_AFTER).await;
    let _ = target.commit("stray").await;
    tokio::time::sleep(BLIP).await;
    field.ic_call(IC_IFACE, "FocusIn", &()).await;
    let event = tokio::time::timeout(NOTICE, events.next()).await;
    assert_eq!(event, Ok(Some(FocusEvent::FocusOut)));
    let committed = target.commit("stray").await;
    assert!(
        matches!(committed, Err(InjectError::FocusLost)),
        "{committed:?}"
    );
    let seen = sentinel_via_fresh_acquire(&mut field, &mut injector, Some(target)).await;
    assert_eq!(seen, Seen::Commit(SENTINEL.into()));
    field.close().await;
}

/// Focus that does not come back within the grace is a loss, even around an
/// activation, and the commit held for it is refused, never written late.
#[tokio::test]
async fn focus_gone_past_the_grace_is_a_loss() {
    skip_unless_ibus!();
    let _serial = exclusive().await;
    let (mut field, mut injector) = session(0, 0).await;
    let mut target = injector.acquire().await.expect("acquire an ordinary field");
    let mut events = target.focus_events();

    field.focus_out().await;
    tokio::time::sleep(EDGE_AFTER).await;
    target.activated();
    let committed = target.commit("held").await;
    assert!(
        matches!(committed, Err(InjectError::FocusLost)),
        "{committed:?}"
    );
    let event = tokio::time::timeout(NOTICE, events.next()).await;
    assert_eq!(event, Ok(Some(FocusEvent::FocusOut)));

    field.ic_call(IC_IFACE, "FocusIn", &()).await;
    let seen = sentinel_via_fresh_acquire(&mut field, &mut injector, Some(target)).await;
    assert_eq!(seen, Seen::Commit(SENTINEL.into()));
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
    skip_unless_ibus!();
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
    skip_unless_ibus!();
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
    skip_unless_ibus!();
    let _serial = exclusive().await;
    let (first, mut injector) = session(0, 0).await;
    let mut target = injector.acquire().await.expect("acquire the first field");

    let mut events = target.focus_events();
    first.focus_out().await;
    let mut other = Field::open(0, 0).await;
    // Held, at most, while the daemon's calls are in flight.
    let committed = target.commit("stray").await;
    let event = tokio::time::timeout(NOTICE, events.next()).await;
    assert_eq!(event, Ok(Some(FocusEvent::FocusOut)));
    let refused = target.commit("stray").await;
    assert!(
        matches!(refused, Err(InjectError::FocusLost)),
        "commit after focus left the acquired field must fail: {refused:?}"
    );

    let seen = sentinel_via_fresh_acquire(&mut other, &mut injector, Some(target)).await;
    assert_eq!(
        seen,
        Seen::Commit(SENTINEL.into()),
        "text acquired for the first field reached the other one (commit returned {committed:?})"
    );

    other.close().await;
    first.close().await;
}

#[tokio::test]
async fn focus_lost_while_acquiring_is_not_missed() {
    skip_unless_ibus!();
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
    skip_unless_ibus!();
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
    skip_unless_ibus!();
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
    skip_unless_ibus!();
    let _serial = exclusive().await;
    // GNOME Shell forwards a Wayland PIN as purpose 0 with PRIVATE|HIDDEN_TEXT.
    let (field, mut injector) = session(0, HINT_PRIVATE | HINT_HIDDEN_TEXT).await;
    assert_acquire_refused(&mut injector).await;

    field.close().await;
}

#[tokio::test]
async fn private_field_is_not_refused() {
    skip_unless_ibus!();
    let _serial = exclusive().await;
    let (mut field, mut injector) = session(0, HINT_PRIVATE).await;
    let mut target = injector.acquire().await.expect("acquire a private field");
    target.commit("hello").await.expect("commit hello");
    assert_eq!(field.next().await, Seen::Commit("hello".into()));

    target.release().await;
    field.close().await;
}

/// The suite every text input backend passes, against the real engine and
/// a real input context: one suite, every backend.
#[tokio::test]
async fn the_ibus_backend_conforms() {
    skip_unless_ibus!();
    let _serial = exclusive().await;
    let mut fixture = IbusFixture::default();
    let report = suite::run(&mut fixture).await;
    fixture.close().await;
    assert_eq!(
        report.passed,
        [
            "commits_reach_a_plain_field",
            "no_commit_after_focus_loss",
            "late_focus_streams_still_report_the_loss",
            "a_focused_target_reports_nothing",
            "a_newer_target_supersedes_the_older",
            "release_after_focus_loss_then_reacquire",
            "a_focus_blip_with_an_activation_is_not_a_loss",
            "a_focus_blip_without_an_activation_is_a_loss",
            "a_blip_kept_alive_by_continued_activation_is_not_a_loss",
            "secure_fields_are_refused",
            "a_field_turning_secure_is_refused",
            "commit_clears_the_preedit",
            "no_preedit_after_focus_loss",
            "release_clears_the_preedit",
        ]
    );
    assert!(report.unobserved.is_empty(), "{report:?}");
    assert_eq!(report.not_applicable, ["preedit_without_support_is_inert"]);
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
    skip_unless_ibus!();
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
