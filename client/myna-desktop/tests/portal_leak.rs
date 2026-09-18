//! Env-gated fake-portal suite (`MYNA_DBUS_TESTS=1`): what a bind attempt
//! leaves behind when the backend takes the request and never answers.
//!
//! This is the reported bug, reproduced at the boundary the user sees it at.
//! A VM was left sitting at a lock screen with the daemon running; 47 minutes
//! later the desktop had six stacked "Add Keyboard Shortcuts" sheets on it.
//! The portal resolves `BindShortcuts` on a `Response` *signal*, so an
//! unanswered sheet is a pending request that no D-Bus timeout ever cancels:
//! the daemon abandons the attempt on its own clock, retries, and raises
//! another. Nothing closes the one it walked away from.
//!
//! `xdg-desktop-portal` cannot be scripted into that state, so the portal is
//! faked here - it owns `org.freedesktop.portal.Desktop`, answers
//! `CreateSession`, and then sits on `BindShortcuts` forever. It books every
//! request and session it hands out and un-books them on `Close`, so "what did
//! the daemon leave behind" is a number this test can assert on.
//!
//! Runs under the private session bus `dev/gated-tests.sh` already stands up
//! for `dbus_hw` (same gate, same reason: it needs a bus and nothing else).
//!
//! ```sh
//! MYNA_DBUS_TESTS=1 dbus-run-session -- cargo test -p myna-desktop --test portal_leak
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::message::Header;
use zbus::object_server::ObjectServer;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{interface, Connection};

use myna_desktop::dbus::serve::ZbusBus;
use myna_desktop::dbus::{FakeBus, PropertyValue, BUS_NAME, OBJECT_PATH};
use myna_desktop::shortcut::portal::{
    ActivationMode, GlobalShortcutTrigger, TriggerError, DEFAULT_TRIGGER,
};

/// True when a session bus is available. Same gate as `dbus_hw`: the fake
/// portal needs a bus to own a name on, and nothing else.
fn dbus_enabled() -> bool {
    std::env::var("MYNA_DBUS_TESTS").as_deref() == Ok("1")
}

/// How the bus this suite talks to is stood up, quoted in every "it is not
/// there" failure so the reader knows what to start.
const HOW_TO_RUN: &str = "dev/gated-tests.sh runs the suite under dbus-run-session and only then \
     sets the gate; run `make test-client-gated`";

/// The bus the gate promises. Set gate, no bus → fail: the fake portal has
/// nowhere to own its name, and a case that never raised a sheet would report
/// the same green as one that raised none.
async fn require_session_bus() -> zbus::Connection {
    zbus::Connection::session().await.unwrap_or_else(|e| {
        panic!("MYNA_DBUS_TESTS=1 but no session bus answers ({e}). {HOW_TO_RUN}")
    })
}

/// Skip when the gate is unset, saying so; fail when it is set and the bus it
/// promises is not there.
macro_rules! skip_unless_dbus {
    () => {
        if !dbus_enabled() {
            // cargo attributes this to the case it came from.
            eprintln!("skipped: set MYNA_DBUS_TESTS=1 (needs a session bus)");
            return;
        }
        let _bus = require_session_bus().await;
    };
}

/// The gate read both ways: unset, the suite skips and says so; set, the bus
/// it promises answers.
#[tokio::test]
async fn the_bus_the_gate_promises_answers() {
    skip_unless_dbus!();
    let conn = require_session_bus().await;
    zbus::fdo::DBusProxy::new(&conn)
        .await
        .expect("bus proxy")
        .get_id()
        .await
        .unwrap_or_else(|e| panic!("the session bus does not answer GetId ({e}). {HOW_TO_RUN}"));
}

/// What the fake portal has handed out and not been asked to take back.
///
/// The interesting assertion is on the *live* sets, not on totals: a daemon
/// that retries forever is fine as long as each attempt tidies up after
/// itself, and a daemon that leaks one session per attempt is the bug whether
/// it retries three times or six.
#[derive(Default, Debug)]
struct Ledger {
    /// `BindShortcuts` calls received - i.e. sheets raised.
    binds: usize,
    /// `ListShortcuts` calls received - i.e. silent lookups.
    lists: usize,
    /// Every `preferred_trigger` a `BindShortcuts` offered.
    preferred: Vec<String>,
    /// Request paths handed out and not yet `Close`d.
    live_requests: HashSet<String>,
    /// Session paths handed out and not yet `Close`d.
    live_sessions: HashSet<String>,
    /// High-water mark of `live_requests` - how many sheets were on screen at
    /// once. This is the number in the screenshot.
    peak_live_requests: usize,
}

type Shared = Arc<Mutex<Ledger>>;

/// `/org/freedesktop/portal/desktop`, the GlobalShortcuts interface.
struct GlobalShortcutsFake {
    ledger: Shared,
    conn: Arc<std::sync::OnceLock<Connection>>,
    /// Shortcut ids `ListShortcuts` reports as already bound.
    stored: Vec<String>,
    /// How `BindShortcuts` is answered.
    answer: Answer,
}

/// What the fake does with a `BindShortcuts`.
#[derive(Clone, Copy)]
enum Answer {
    /// Sit on it: the sheet nobody is in front of.
    Never,
    /// Grant it - what a real portal does with a stored binding to answer from.
    Grant,
    /// Answer with response 1: the user dismissed the sheet.
    Dismiss,
}

/// The client picks its own handle tokens and computes the object path it will
/// listen on; the portal has to derive the identical path or its `Response`
/// lands nowhere. Mirrors `ashpd::proxy::Proxy::unique_name`.
fn portal_path(prefix: &str, sender: &str, token: &str) -> String {
    let unique = sender.trim_start_matches(':').replace('.', "_");
    format!("/org/freedesktop/portal/desktop/{prefix}/{unique}/{token}")
}

fn token(options: &HashMap<String, OwnedValue>, key: &str) -> String {
    options
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or("t")
        .to_string()
}

#[interface(name = "org.freedesktop.portal.GlobalShortcuts")]
impl GlobalShortcutsFake {
    #[zbus(property)]
    fn version(&self) -> u32 {
        1
    }

    /// Answered immediately and successfully: the session is not what this
    /// test is about, it is the thing the daemon is expected to clean up.
    async fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let sender = hdr
            .sender()
            .map(|s| s.to_string())
            .unwrap_or_else(|| ":0.0".into());
        let request = portal_path("request", &sender, &token(&options, "handle_token"));
        let session = portal_path("session", &sender, &token(&options, "session_handle_token"));

        server
            .at(
                session.as_str(),
                SessionFake {
                    path: session.clone(),
                    ledger: Arc::clone(&self.ledger),
                },
            )
            .await?;
        self.ledger
            .lock()
            .unwrap()
            .live_sessions
            .insert(session.clone());

        // The client installed its `Response` match before calling, so
        // emitting before this method's reply is delivered is still caught.
        let mut results: HashMap<&str, Value> = HashMap::new();
        results.insert(
            "session_handle",
            Value::from(OwnedObjectPath::try_from(session.as_str()).unwrap()),
        );
        self.conn
            .get()
            .expect("connection published before serving")
            .emit_signal(
                None::<&str>,
                request.as_str(),
                "org.freedesktop.portal.Request",
                "Response",
                &(0u32, results),
            )
            .await?;

        Ok(OwnedObjectPath::try_from(request.as_str()).unwrap())
    }

    /// Answers immediately with whatever this fake was told is already bound.
    /// The real portal raises no UI here, and neither does this.
    async fn list_shortcuts(
        &self,
        _session_handle: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let sender = hdr
            .sender()
            .map(|s| s.to_string())
            .unwrap_or_else(|| ":0.0".into());
        let request = portal_path("request", &sender, &token(&options, "handle_token"));
        self.ledger.lock().unwrap().lists += 1;

        let shortcuts: Vec<(&str, HashMap<&str, Value>)> = self
            .stored
            .iter()
            .map(|id| {
                let mut meta: HashMap<&str, Value> = HashMap::new();
                meta.insert("description", Value::from("myna dictation"));
                meta.insert("trigger_description", Value::from("Super+T"));
                (id.as_str(), meta)
            })
            .collect();
        let mut results: HashMap<&str, Value> = HashMap::new();
        results.insert("shortcuts", Value::from(shortcuts));
        self.conn
            .get()
            .expect("connection published before serving")
            .emit_signal(
                None::<&str>,
                request.as_str(),
                "org.freedesktop.portal.Request",
                "Response",
                &(0u32, results),
            )
            .await?;

        Ok(OwnedObjectPath::try_from(request.as_str()).unwrap())
    }

    /// Takes the request, books it, and never answers - the sheet nobody is
    /// in front of. The only way this request ever goes away is `Close`.
    #[allow(clippy::too_many_arguments)]
    async fn bind_shortcuts(
        &self,
        _session_handle: OwnedObjectPath,
        shortcuts: Vec<(String, HashMap<String, OwnedValue>)>,
        _parent_window: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let sender = hdr
            .sender()
            .map(|s| s.to_string())
            .unwrap_or_else(|| ":0.0".into());
        let request = portal_path("request", &sender, &token(&options, "handle_token"));

        server
            .at(
                request.as_str(),
                RequestFake {
                    path: request.clone(),
                    ledger: Arc::clone(&self.ledger),
                },
            )
            .await?;
        {
            let mut ledger = self.ledger.lock().unwrap();
            ledger.binds += 1;
            ledger
                .preferred
                .extend(shortcuts.iter().filter_map(|(_, options)| {
                    options
                        .get("preferred_trigger")
                        .and_then(|v| <&str>::try_from(v).ok())
                        .map(str::to_string)
                }));
            ledger.live_requests.insert(request.clone());
            ledger.peak_live_requests = ledger.peak_live_requests.max(ledger.live_requests.len());
        }

        let response = match self.answer {
            Answer::Never => None,
            Answer::Grant => Some(0u32),
            Answer::Dismiss => Some(1u32),
        };
        if let Some(response) = response {
            let mut meta: HashMap<&str, Value> = HashMap::new();
            meta.insert("description", Value::from("myna dictation"));
            meta.insert("trigger_description", Value::from("Super+T"));
            let mut results: HashMap<&str, Value> = HashMap::new();
            if response == 0 {
                results.insert("shortcuts", Value::from(vec![("dictate", meta)]));
            }
            self.conn
                .get()
                .expect("connection published before serving")
                .emit_signal(
                    None::<&str>,
                    request.as_str(),
                    "org.freedesktop.portal.Request",
                    "Response",
                    &(response, results),
                )
                .await?;
        }

        Ok(OwnedObjectPath::try_from(request.as_str()).unwrap())
    }
}

/// A pending request. `Close` is the portal's own "end all related user
/// interaction (dialogs, etc)" - i.e. the sheet coming off the screen.
struct RequestFake {
    path: String,
    ledger: Shared,
}

#[interface(name = "org.freedesktop.portal.Request")]
impl RequestFake {
    async fn close(&self) {
        self.ledger.lock().unwrap().live_requests.remove(&self.path);
    }
}

struct SessionFake {
    path: String,
    ledger: Shared,
}

#[interface(name = "org.freedesktop.portal.Session")]
impl SessionFake {
    /// Closing the session is what tears down the backend's shortcut
    /// interaction for it, so it retires the session's pending request too -
    /// this is the behaviour `xdg-desktop-portal` implements, and the only
    /// lever a client has when `ashpd` keeps the request handle to itself.
    async fn close(&self) {
        let mut ledger = self.ledger.lock().unwrap();
        ledger.live_sessions.remove(&self.path);
        ledger.live_requests.clear();
    }
}

/// Only one fake at a time: `org.freedesktop.portal.Desktop` is a well-known
/// name, and a second owner does not fail - it *queues*, so a parallel test
/// silently talks to the other test's portal and asserts on a ledger nobody
/// used. Held for the whole of each test, released with the name.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A fake portal and its exclusive claim on the portal name.
struct Portal {
    conn: Connection,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl Portal {
    /// Give the name up before the next test asks for it. Dropping the
    /// connection would get there eventually; "eventually" is the race.
    async fn shutdown(self) {
        let _ = self
            .conn
            .release_name("org.freedesktop.portal.Desktop")
            .await;
    }
}

/// Stand the fake portal up and own the portal's well-known name.
async fn fake_portal() -> (Portal, Shared) {
    fake_portal_holding(&[]).await
}

/// As [`fake_portal`], but reporting `stored` from `ListShortcuts`.
async fn fake_portal_holding(stored: &[&str]) -> (Portal, Shared) {
    fake_portal_with(stored, Answer::Never).await
}

/// As [`fake_portal_holding`], answering `BindShortcuts` as `answer` says.
async fn fake_portal_with(stored: &[&str], answer: Answer) -> (Portal, Shared) {
    let serial = SERIAL.lock().await;
    let ledger: Shared = Arc::new(Mutex::new(Ledger::default()));
    let slot = Arc::new(std::sync::OnceLock::new());

    let conn = zbus::connection::Builder::session()
        .expect("a session bus")
        .name("org.freedesktop.portal.Desktop")
        .expect("portal name is valid")
        .serve_at(
            "/org/freedesktop/portal/desktop",
            GlobalShortcutsFake {
                ledger: Arc::clone(&ledger),
                conn: Arc::clone(&slot),
                stored: stored.iter().map(|s| s.to_string()).collect(),
                answer,
            },
        )
        .expect("desktop path is valid")
        .build()
        .await
        .expect("fake portal owns org.freedesktop.portal.Desktop");

    let _ = slot.set(conn.clone());
    (
        Portal {
            conn,
            _serial: serial,
        },
        ledger,
    )
}

/// The reported bug, in one assertion.
///
/// Three bind attempts against a portal that never answers, each abandoned the
/// way the retry loop abandons one. Before the fix this leaves three pending
/// requests and three open sessions behind - three sheets on the user's
/// screen, and the count keeps climbing for as long as the daemon runs. After
/// it, an abandoned attempt takes its own sheet down, so at most one is ever
/// live no matter how many times the daemon tries.
#[tokio::test(flavor = "multi_thread")]
async fn an_abandoned_bind_does_not_leave_its_sheet_on_screen() {
    skip_unless_dbus!();
    let (portal, ledger) = fake_portal().await;

    for _ in 0..3 {
        let client = zbus::Connection::session().await.expect("client bus");
        // Two seconds, not the production two minutes: the wait itself is not
        // what is under test, abandoning it correctly is.
        let outcome = GlobalShortcutTrigger::bind_with_connection_timeout(
            client,
            "dictate",
            Some(DEFAULT_TRIGGER),
            ActivationMode::Toggle,
            Duration::from_secs(2),
        )
        .await;
        assert!(
            matches!(outcome, Err(TriggerError::BindUnanswered(_))),
            "a portal that never answers must be abandoned as unanswered, got {:?}",
            outcome.err()
        );
    }

    // Let the Close calls land: they are fire-and-forget from the client's
    // point of view, and the assertion is about the portal's books.
    tokio::time::sleep(Duration::from_millis(200)).await;
    portal.shutdown().await;

    let ledger = ledger.lock().unwrap();
    assert_eq!(ledger.binds, 3, "each attempt should raise its own sheet");
    assert_eq!(
        ledger.peak_live_requests, 1,
        "sheets stacked: {} were live at once, which is the reported bug",
        ledger.peak_live_requests
    );
    assert!(
        ledger.live_requests.is_empty(),
        "abandoned requests left pending: {:?}",
        ledger.live_requests
    );
    assert!(
        ledger.live_sessions.is_empty(),
        "abandoned sessions left open: {:?}",
        ledger.live_sessions
    );
}

/// Point `consent` at a scratch dir for the duration of a test. Safe because
/// the fake-portal tests are serialized on `SERIAL`; env is process-wide.
struct Consent(std::path::PathBuf);

impl Consent {
    fn none() -> Self {
        let dir = std::env::temp_dir().join(format!("myna-consent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        std::env::set_var("MYNA_STATE_DIR", &dir);
        Self(dir)
    }

    fn given() -> Self {
        let c = Self::none();
        std::fs::write(c.0.join("shortcut-bound"), b"").expect("mark consent");
        c
    }

    fn is_given(&self) -> bool {
        self.0.join("shortcut-bound").exists()
    }
}

impl Drop for Consent {
    fn drop(&mut self) {
        std::env::remove_var("MYNA_STATE_DIR");
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The daemon must never ask for a key.
///
/// `BindShortcuts` is what puts the portal's "Add Keyboard Shortcuts" sheet on
/// the screen, and the daemon used to call it on every start - so a fresh
/// login raised the sheet, and dismissing it raised it again at the next
/// login, forever. Startup takes what `ListShortcuts` reports and nothing
/// else; with nothing bound that is a clean refusal, not a dialog.
#[tokio::test(flavor = "multi_thread")]
async fn attaching_with_nothing_bound_asks_for_nothing() {
    skip_unless_dbus!();
    let (portal, ledger) = fake_portal_holding(&[]).await;
    let _consent = Consent::none();
    let client = zbus::Connection::session().await.expect("client bus");

    let outcome =
        GlobalShortcutTrigger::attach_with_connection(client, "dictate", ActivationMode::Toggle)
            .await;

    assert!(
        matches!(outcome, Err(TriggerError::NoShortcutBound(_))),
        "an unbound portal should refuse, not prompt; got {:?}",
        outcome.err()
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    portal.shutdown().await;
    let ledger = ledger.lock().unwrap();
    assert_eq!(ledger.binds, 0, "startup raised a bind sheet");
    assert_eq!(ledger.lists, 1, "startup should have asked what is bound");
    assert!(
        ledger.live_sessions.is_empty(),
        "a refused attach left its session open: {:?}",
        ledger.live_sessions
    );
}

/// And with a binding in place it attaches to it - still without prompting.
#[tokio::test(flavor = "multi_thread")]
async fn attaching_to_an_existing_binding_still_asks_for_nothing() {
    skip_unless_dbus!();
    let (portal, ledger) = fake_portal_holding(&["dictate"]).await;
    let client = zbus::Connection::session().await.expect("client bus");

    let trigger =
        GlobalShortcutTrigger::attach_with_connection(client, "dictate", ActivationMode::Toggle)
            .await;
    assert!(trigger.is_ok(), "attach failed: {:?}", trigger.err());
    portal.shutdown().await;

    let ledger = ledger.lock().unwrap();
    assert_eq!(ledger.binds, 0, "attaching raised a bind sheet");
    assert_eq!(
        ledger.live_sessions.len(),
        1,
        "the session has to stay open - it is what the signals arrive on"
    );
}

/// A binding for someone else is not ours to take.
#[tokio::test(flavor = "multi_thread")]
async fn attaching_ignores_a_binding_for_another_shortcut() {
    skip_unless_dbus!();
    let (portal, ledger) = fake_portal_holding(&["something-else"]).await;
    let _consent = Consent::none();
    let client = zbus::Connection::session().await.expect("client bus");

    let outcome =
        GlobalShortcutTrigger::attach_with_connection(client, "dictate", ActivationMode::Toggle)
            .await;

    assert!(
        matches!(outcome, Err(TriggerError::NoShortcutBound(_))),
        "got {:?}",
        outcome.err()
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    portal.shutdown().await;
    assert_eq!(ledger.lock().unwrap().binds, 0);
}

/// GNOME's `ListShortcuts` reports only what was bound on the current session,
/// never what the portal has stored - so once the user has been through the
/// dialog, re-issuing `BindShortcuts` is the only way to get the binding back,
/// and it is silent because the portal answers from its store.
#[tokio::test(flavor = "multi_thread")]
async fn a_shortcut_the_user_asked_for_is_re_bound_at_startup() {
    skip_unless_dbus!();
    let (portal, ledger) = fake_portal_with(&[], Answer::Grant).await;
    let _consent = Consent::given();
    let client = zbus::Connection::session().await.expect("client bus");

    let trigger =
        GlobalShortcutTrigger::attach_with_connection(client, "dictate", ActivationMode::Toggle)
            .await;
    assert!(trigger.is_ok(), "attach failed: {:?}", trigger.err());
    portal.shutdown().await;

    assert_eq!(
        ledger.lock().unwrap().binds,
        1,
        "a consented startup has to re-issue the bind"
    );
    assert!(_consent.is_given(), "consent must survive a working bind");
}

/// And the sheet is asked for exactly once. A user who removed the shortcut in
/// Settings gets one dialog, not one at every login for the life of the
/// install - dismissing it spends the consent that put it there.
#[tokio::test(flavor = "multi_thread")]
async fn a_dismissed_sheet_is_not_raised_again_next_login() {
    skip_unless_dbus!();
    // `Answer::Never` - the sheet goes up and nobody answers it.
    let (portal, ledger) = fake_portal_with(&[], Answer::Never).await;
    let consent = Consent::given();
    let client = zbus::Connection::session().await.expect("client bus");

    let first = GlobalShortcutTrigger::attach_with_connection_timeout(
        client.clone(),
        "dictate",
        ActivationMode::Toggle,
        Duration::from_secs(2),
    )
    .await;
    assert!(first.is_err(), "an unanswered sheet is not a binding");
    assert!(
        !consent.is_given(),
        "an unanswered sheet has to spend the consent that raised it"
    );

    let second =
        GlobalShortcutTrigger::attach_with_connection(client, "dictate", ActivationMode::Toggle)
            .await;
    assert!(
        matches!(second, Err(TriggerError::NoShortcutBound(_))),
        "the next start asked again; got {:?}",
        second.err()
    );
    portal.shutdown().await;
    assert_eq!(
        ledger.lock().unwrap().binds,
        1,
        "the sheet went up more than once"
    );
}

/// ashpd resolves `BindShortcuts` once `Response` arrives, whatever it says. A
/// dismissed sheet is a refusal, and it spends the consent that raised it.
#[tokio::test(flavor = "multi_thread")]
async fn a_dismissed_re_bind_is_a_refusal_not_a_binding() {
    skip_unless_dbus!();
    let (portal, _ledger) = fake_portal_with(&[], Answer::Dismiss).await;
    let consent = Consent::given();
    let client = zbus::Connection::session().await.expect("client bus");

    let outcome =
        GlobalShortcutTrigger::attach_with_connection(client, "dictate", ActivationMode::Toggle)
            .await;
    portal.shutdown().await;

    assert!(
        matches!(outcome, Err(TriggerError::BindRejected(_))),
        "a dismissed sheet was taken as a binding: {:?}",
        outcome.err()
    );
    assert!(
        !consent.is_given(),
        "a dismissed sheet has to spend consent"
    );
}

/// The granted key is what `Shortcut` publishes, from either path to a binding.
#[tokio::test(flavor = "multi_thread")]
async fn attaching_reports_the_granted_trigger() {
    skip_unless_dbus!();
    let (portal, _ledger) = fake_portal_holding(&["dictate"]).await;
    let client = zbus::Connection::session().await.expect("client bus");
    let listed =
        GlobalShortcutTrigger::attach_with_connection(client, "dictate", ActivationMode::Toggle)
            .await
            .expect("attach to the listed binding");
    assert_eq!(listed.shortcut(), "Super+T");
    drop(listed);
    portal.shutdown().await;

    let (portal, _ledger) = fake_portal_with(&[], Answer::Grant).await;
    let _consent = Consent::given();
    let client = zbus::Connection::session().await.expect("client bus");
    let rebound =
        GlobalShortcutTrigger::attach_with_connection(client, "dictate", ActivationMode::Toggle)
            .await
            .expect("re-bind from the store");
    assert_eq!(rebound.shortcut(), "Super+T");
    portal.shutdown().await;
}

/// A rebind in Settings arrives as `ShortcutsChanged` on the live session and
/// has to reach `Shortcut` without a restart.
#[tokio::test(flavor = "multi_thread")]
async fn a_rebind_in_settings_reaches_the_published_shortcut() {
    skip_unless_dbus!();
    let (portal, ledger) = fake_portal_holding(&["dictate"]).await;
    let client = zbus::Connection::session().await.expect("client bus");
    let bus = FakeBus::new();
    let mut trigger =
        GlobalShortcutTrigger::attach_with_connection(client, "dictate", ActivationMode::Toggle)
            .await
            .expect("attach")
            .publish_shortcut_on(Arc::new(tokio::sync::Mutex::new(bus.clone())))
            .await;
    assert_eq!(
        bus.property("Shortcut"),
        Some(PropertyValue::Str("Super+T".into()))
    );

    let session = ledger
        .lock()
        .unwrap()
        .live_sessions
        .iter()
        .next()
        .cloned()
        .expect("a live session");
    let mut meta: HashMap<&str, Value> = HashMap::new();
    meta.insert("description", Value::from("myna dictation"));
    meta.insert("trigger_description", Value::from("Super+K"));
    let listener = tokio::spawn(async move {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            myna_orchestrator::Trigger::next_edge(&mut trigger),
        )
        .await;
    });
    // The signal match is installed before attach returns, so there is no race
    // with the listener starting.
    portal
        .conn
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.GlobalShortcuts",
            "ShortcutsChanged",
            &(
                OwnedObjectPath::try_from(session.as_str()).unwrap(),
                vec![("dictate", meta)],
            ),
        )
        .await
        .expect("emit ShortcutsChanged");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while bus.property("Shortcut") != Some(PropertyValue::Str("Super+K".into()))
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    listener.abort();
    portal.shutdown().await;
    assert_eq!(
        bus.property("Shortcut"),
        Some(PropertyValue::Str("Super+K".into()))
    );
}

/// A bind that the portal grants reports the key it granted.
#[tokio::test(flavor = "multi_thread")]
async fn binding_reports_the_granted_trigger() {
    skip_unless_dbus!();
    let (portal, ledger) = fake_portal_with(&[], Answer::Grant).await;
    let client = zbus::Connection::session().await.expect("client bus");

    let trigger = GlobalShortcutTrigger::bind_with_connection_timeout(
        client,
        "dictate",
        Some(DEFAULT_TRIGGER),
        ActivationMode::Toggle,
        Duration::from_secs(2),
    )
    .await
    .expect("a granted bind");
    portal.shutdown().await;

    assert_eq!(trigger.shortcut(), "Super+T");
    assert_eq!(ledger.lock().unwrap().preferred, vec![DEFAULT_TRIGGER]);
}

/// Myna Settings binds through the daemon. `BindShortcut("")` offers the
/// default trigger, publishes the grant as `Shortcut`, records consent, and
/// wakes the retry loop parked on the unbound recheck.
#[tokio::test(flavor = "multi_thread")]
async fn binding_through_the_daemon_offers_the_default_and_publishes_the_grant() {
    skip_unless_dbus!();
    let (portal, ledger) = fake_portal_with(&[], Answer::Grant).await;
    let consent = Consent::none();
    let bound = Arc::new(tokio::sync::Notify::new());
    let _daemon = ZbusBus::serve_for_portal(Some(ActivationMode::Toggle), Arc::clone(&bound))
        .await
        .expect("serve the daemon object");

    let (ok, message) = myna_desktop::dbus::status::bind_shortcut(None)
        .await
        .expect("BindShortcut answers");

    assert!(ok, "{message}");
    assert_eq!(message, "bound to Super+T");
    assert_eq!(ledger.lock().unwrap().preferred, vec![DEFAULT_TRIGGER]);
    assert!(consent.is_given(), "a granted bind is consent to re-bind");
    tokio::time::timeout(Duration::from_secs(1), bound.notified())
        .await
        .expect("the retry loop was not woken");

    let client = zbus::Connection::session().await.expect("client bus");
    let published: String = zbus::fdo::PropertiesProxy::builder(&client)
        .destination(BUS_NAME)
        .unwrap()
        .path(OBJECT_PATH)
        .unwrap()
        .build()
        .await
        .expect("properties proxy")
        .get(
            zbus::names::InterfaceName::try_from(BUS_NAME).unwrap(),
            "Shortcut",
        )
        .await
        .expect("Shortcut")
        .try_into()
        .expect("a string");
    portal.shutdown().await;
    assert_eq!(published, "Super+T");
}
