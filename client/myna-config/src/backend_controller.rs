//! GTK-independent controller/presenter for backend pages.
//!
//! The controller owns the state that the Backend and Diagnostics tabs
//! preferences pages render. It coordinates discovery and snapshot refreshes
//! through the async [`BackendRepository`], but does not itself perform any
//! I/O — callers drive it through `begin_*`/`complete_*` request pairs so the
//! GTK integration can spawn futures on the main context while the controller
//! stays synchronous and easy to test.
//!
//! Requests carry a monotonically increasing generation number so callers can
//! safely drop stale results that arrive out of order after a newer refresh
//! has been started.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::command::CancellationToken;
use crate::domain::{
    ActiveBackendState, BackendIdentity, BackendSnapshot, BackendSurfaceError, ConnectionSnapshot,
    TextOffer,
};
use crate::ports::BackendRepository;
use crate::presentation::{present_configuration, PresentationRow};

/// Whether the `myna:backend` plug is currently attached to this backend's
/// `inference-provider` slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionKind {
    /// The backend is installed but not connected to the Myna daemon.
    Disconnected,
    /// The backend is the sole `myna:backend` target.
    Active,
    /// Multiple `myna:backend` connections exist and this backend is one of
    /// them. Surfaced explicitly so the UI can warn the user.
    Contested,
}

/// Public, read-only view of a single backend page.
#[derive(Clone, Debug, PartialEq)]
pub struct BackendPage {
    identity: BackendIdentity,
    connection: ConnectionKind,
    loading: bool,
    partial: bool,
    snapshot: Option<BackendSnapshot>,
    rows: Vec<PresentationRow>,
    errors: Vec<BackendSurfaceError>,
}

impl BackendPage {
    pub fn identity(&self) -> &BackendIdentity {
        &self.identity
    }

    pub fn connection(&self) -> ConnectionKind {
        self.connection
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    /// True when the snapshot has at least one surface error and the backend
    /// is being shown in a degraded state.
    pub fn partial(&self) -> bool {
        self.partial
    }

    pub fn snapshot(&self) -> Option<&BackendSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn rows(&self) -> &[PresentationRow] {
        &self.rows
    }

    pub fn errors(&self) -> &[BackendSurfaceError] {
        &self.errors
    }

    /// Concise summary of a backend's connection, model and refresh state.
    pub fn short_status(&self) -> BackendShortStatus {
        BackendShortStatus {
            connection: self.connection,
            active_model: self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.models())
                .and_then(|models| models.active().map(str::to_owned)),
            active_engine: self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.engines())
                .and_then(|engines| engines.active().map(str::to_owned)),
            loading: self.loading,
            partial: self.partial,
        }
    }
}

/// Summary of a backend's high-level state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendShortStatus {
    pub connection: ConnectionKind,
    pub active_model: Option<String>,
    pub active_engine: Option<String>,
    pub loading: bool,
    pub partial: bool,
}

/// Events emitted whenever controller state changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControllerEvent {
    /// Discovery started; loading state should be rendered immediately.
    DiscoveryStarted,
    /// The list of pages changed (added, removed, or re-ordered), or the
    /// connection kind of an existing page changed.
    DiscoveryChanged,
    /// Discovery failed and the failure should be reported to the user.
    DiscoveryFailed(BackendSurfaceError),
    /// A backend page's snapshot changed.
    BackendChanged(BackendIdentity),
}

/// Least time from the start of any discovery to a rediscovery on focus, so
/// a window flapping in and out of focus costs one discovery per interval.
pub const FOCUS_REDISCOVERY_INTERVAL: Duration = Duration::from_secs(2);

/// Handle for an in-flight discovery request.
#[derive(Clone, Debug)]
pub struct DiscoveryRequest {
    generation: u64,
    token: CancellationToken,
    quiet: bool,
}

impl DiscoveryRequest {
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// Started by [`BackendController::begin_focus_discovery`].
    pub fn quiet(&self) -> bool {
        self.quiet
    }
}

/// Handle for an in-flight per-backend snapshot request.
#[derive(Clone, Debug)]
pub struct SnapshotRequest {
    snap: String,
    generation: u64,
    token: CancellationToken,
}

impl SnapshotRequest {
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

type Observer = Box<dyn Fn(&ControllerEvent)>;

pub struct BackendController {
    repository: Option<Rc<dyn BackendRepository>>,
    inner: RefCell<Inner>,
    observers: RefCell<Vec<Observer>>,
}

struct Inner {
    pages: BTreeMap<String, PageEntry>,
    order: Vec<String>,
    active: ActiveBackendState,
    text_providers: Vec<BackendIdentity>,
    text_offers: Vec<TextOffer>,
    strays: Vec<BackendIdentity>,
    discovery_generation: u64,
    latest_completed_discovery: u64,
    discovery_token: Option<CancellationToken>,
    discovery_loading: bool,
    last_discovery_started: Option<Instant>,
    last_discovery_error: Option<BackendSurfaceError>,
}

struct PageEntry {
    identity: BackendIdentity,
    connection: ConnectionKind,
    snapshot: Option<BackendSnapshot>,
    snapshot_generation: u64,
    latest_completed_snapshot: u64,
    inflight_token: Option<CancellationToken>,
    loading: bool,
}

impl BackendController {
    pub fn new(repository: Rc<dyn BackendRepository>) -> Rc<Self> {
        Rc::new(Self {
            repository: Some(repository),
            inner: RefCell::new(Inner::default()),
            observers: RefCell::new(Vec::new()),
        })
    }

    /// Construct a controller without an attached repository. Useful for
    /// tests that drive `begin_*`/`complete_*` methods directly without going
    /// through the async layer.
    pub fn detached() -> Rc<Self> {
        Rc::new(Self {
            repository: None,
            inner: RefCell::new(Inner::default()),
            observers: RefCell::new(Vec::new()),
        })
    }

    pub fn repository(&self) -> Option<&Rc<dyn BackendRepository>> {
        self.repository.as_ref()
    }

    pub fn observe(&self, observer: impl Fn(&ControllerEvent) + 'static) {
        self.observers.borrow_mut().push(Box::new(observer));
    }

    pub fn discovery_loading(&self) -> bool {
        self.inner.borrow().discovery_loading
    }

    pub fn last_discovery_error(&self) -> Option<BackendSurfaceError> {
        self.inner.borrow().last_discovery_error.clone()
    }

    pub fn pages(&self) -> Vec<BackendPage> {
        let inner = self.inner.borrow();
        inner
            .order
            .iter()
            .filter_map(|snap| inner.pages.get(snap).map(build_page))
            .collect()
    }

    pub fn page(&self, snap: &str) -> Option<BackendPage> {
        self.inner.borrow().pages.get(snap).map(build_page)
    }

    /// The text-model snaps the last discovery found, connected or not.
    pub fn text_offers(&self) -> Vec<TextOffer> {
        self.inner.borrow().text_offers.clone()
    }

    pub fn connection_snapshot(&self) -> ConnectionSnapshot {
        let inner = self.inner.borrow();
        ConnectionSnapshot::new(
            inner
                .order
                .iter()
                .filter_map(|snap| inner.pages.get(snap).map(|page| page.identity.clone()))
                .collect(),
            inner.active.clone(),
        )
        .with_companions(inner.text_providers.clone(), inner.strays.clone())
        .with_text_offers(inner.text_offers.clone())
    }

    pub fn begin_discovery(&self) -> DiscoveryRequest {
        let request = self
            .inner
            .borrow_mut()
            .begin_discovery(Instant::now(), false);
        self.emit(vec![ControllerEvent::DiscoveryStarted]);
        request
    }

    /// Rediscover because the window regained focus, since the user may have
    /// installed or removed a model elsewhere. Refused while any discovery
    /// runs or within [`FOCUS_REDISCOVERY_INTERVAL`] of one starting. It shows
    /// no loading state, announces only what changed, and any other
    /// discovery supersedes it.
    pub fn begin_focus_discovery(&self, now: Instant) -> Option<DiscoveryRequest> {
        let mut inner = self.inner.borrow_mut();
        let recent = inner.last_discovery_started.is_some_and(|started| {
            now.saturating_duration_since(started) < FOCUS_REDISCOVERY_INTERVAL
        });
        if inner.discovery_token.is_some() || recent {
            return None;
        }
        Some(inner.begin_discovery(now, true))
    }

    pub fn complete_discovery(
        &self,
        request: DiscoveryRequest,
        result: Result<ConnectionSnapshot, BackendSurfaceError>,
    ) -> bool {
        let mut events = Vec::new();
        {
            let mut inner = self.inner.borrow_mut();
            if request.generation <= inner.latest_completed_discovery
                || request.generation != inner.discovery_generation
            {
                return false;
            }
            inner.latest_completed_discovery = request.generation;
            inner.discovery_loading = false;
            inner.discovery_token = None;
            match result {
                Ok(snapshot) => {
                    let recovered = inner.last_discovery_error.take().is_some();
                    apply_discovery(&mut inner, snapshot, &mut events);
                    let announce = !request.quiet || recovered;
                    if announce && !events.contains(&ControllerEvent::DiscoveryChanged) {
                        events.push(ControllerEvent::DiscoveryChanged);
                    }
                }
                Err(error) => {
                    let repeated = inner.last_discovery_error.as_ref() == Some(&error);
                    inner.last_discovery_error = Some(error.clone());
                    if !(request.quiet && repeated) {
                        events.push(ControllerEvent::DiscoveryFailed(error));
                    }
                }
            }
        }
        self.emit(events);
        true
    }

    pub fn begin_snapshot(&self, snap: &str) -> Option<SnapshotRequest> {
        let (request, identity) = {
            let mut inner = self.inner.borrow_mut();
            let entry = inner.pages.get_mut(snap)?;
            if let Some(previous) = entry.inflight_token.take() {
                previous.cancel();
            }
            entry.snapshot_generation += 1;
            entry.loading = true;
            let token = CancellationToken::new();
            entry.inflight_token = Some(token.clone());
            (
                SnapshotRequest {
                    snap: snap.to_owned(),
                    generation: entry.snapshot_generation,
                    token,
                },
                entry.identity.clone(),
            )
        };
        self.emit(vec![ControllerEvent::BackendChanged(identity)]);
        Some(request)
    }

    pub fn complete_snapshot(&self, request: SnapshotRequest, snapshot: BackendSnapshot) {
        let mut events = Vec::new();
        {
            let mut inner = self.inner.borrow_mut();
            let Some(entry) = inner.pages.get_mut(&request.snap) else {
                return;
            };
            if request.generation <= entry.latest_completed_snapshot
                || request.generation != entry.snapshot_generation
            {
                return;
            }
            entry.latest_completed_snapshot = request.generation;
            entry.loading = false;
            entry.inflight_token = None;
            entry.snapshot = Some(snapshot);
            events.push(ControllerEvent::BackendChanged(entry.identity.clone()));
        }
        self.emit(events);
    }

    pub fn apply_readback(&self, snap: &str, snapshot: BackendSnapshot) {
        let mut events = Vec::new();
        {
            let mut inner = self.inner.borrow_mut();
            let Some(entry) = inner.pages.get_mut(snap) else {
                return;
            };
            if let Some(token) = entry.inflight_token.take() {
                token.cancel();
            }
            entry.loading = false;
            entry.snapshot = Some(snapshot);
            events.push(ControllerEvent::BackendChanged(entry.identity.clone()));
        }
        self.emit(events);
    }

    /// Cancel every in-flight request (page/window disappearing).
    pub fn cancel_all(&self) {
        let mut inner = self.inner.borrow_mut();
        if let Some(token) = inner.discovery_token.take() {
            token.cancel();
        }
        inner.discovery_loading = false;
        for entry in inner.pages.values_mut() {
            if let Some(token) = entry.inflight_token.take() {
                token.cancel();
            }
            entry.loading = false;
        }
    }

    /// Cancel refreshes for one backend (page destroyed).
    pub fn cancel_page(&self, snap: &str) {
        let mut inner = self.inner.borrow_mut();
        if let Some(entry) = inner.pages.get_mut(snap) {
            if let Some(token) = entry.inflight_token.take() {
                token.cancel();
            }
            entry.loading = false;
        }
    }

    fn emit(&self, events: Vec<ControllerEvent>) {
        if events.is_empty() {
            return;
        }
        let observers = self.observers.borrow();
        for event in &events {
            for observer in observers.iter() {
                observer(event);
            }
        }
    }
}

impl Inner {
    fn begin_discovery(&mut self, now: Instant, quiet: bool) -> DiscoveryRequest {
        if let Some(previous) = self.discovery_token.take() {
            previous.cancel();
        }
        self.discovery_generation += 1;
        self.discovery_loading = !quiet;
        self.last_discovery_started = Some(now);
        let token = CancellationToken::new();
        self.discovery_token = Some(token.clone());
        DiscoveryRequest {
            generation: self.discovery_generation,
            token,
            quiet,
        }
    }
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            pages: BTreeMap::new(),
            order: Vec::new(),
            active: ActiveBackendState::Disconnected,
            text_providers: Vec::new(),
            text_offers: Vec::new(),
            strays: Vec::new(),
            discovery_generation: 0,
            latest_completed_discovery: 0,
            discovery_token: None,
            discovery_loading: false,
            last_discovery_started: None,
            last_discovery_error: None,
        }
    }
}

fn apply_discovery(
    inner: &mut Inner,
    snapshot: ConnectionSnapshot,
    events: &mut Vec<ControllerEvent>,
) {
    let discovered: Vec<BackendIdentity> = snapshot.backends().to_vec();
    let active = snapshot.active_state();
    let discovered_names: Vec<String> = discovered
        .iter()
        .map(|identity| identity.snap_name().to_owned())
        .collect();

    let removed: Vec<String> = inner
        .order
        .iter()
        .filter(|name| !discovered_names.contains(name))
        .cloned()
        .collect();

    let mut structure_changed = !removed.is_empty();
    for snap in &removed {
        if let Some(mut entry) = inner.pages.remove(snap) {
            if let Some(token) = entry.inflight_token.take() {
                token.cancel();
            }
        }
    }

    for identity in &discovered {
        let name = identity.snap_name().to_owned();
        if !inner.pages.contains_key(&name) {
            inner.pages.insert(
                name.clone(),
                PageEntry {
                    identity: identity.clone(),
                    connection: ConnectionKind::Disconnected,
                    snapshot: None,
                    snapshot_generation: 0,
                    latest_completed_snapshot: 0,
                    inflight_token: None,
                    loading: false,
                },
            );
            structure_changed = true;
        } else if let Some(entry) = inner.pages.get_mut(&name) {
            // Preserve the existing entry but keep its identity up to date so
            // resolved modelctl app names propagate.
            if entry.identity != *identity {
                entry.identity = identity.clone();
                structure_changed = true;
                events.push(ControllerEvent::BackendChanged(entry.identity.clone()));
            }
        }
    }

    inner.order = discovered_names.clone();

    let active_names: Vec<String> = match &active {
        ActiveBackendState::Connected(identity) => vec![identity.snap_name().to_owned()],
        ActiveBackendState::MultiplyConnected(identities) => identities
            .iter()
            .map(|identity| identity.snap_name().to_owned())
            .collect(),
        ActiveBackendState::Disconnected => Vec::new(),
    };
    let contested = matches!(active, ActiveBackendState::MultiplyConnected(_));

    for (name, entry) in inner.pages.iter_mut() {
        let previous = entry.connection;
        entry.connection = if active_names.iter().any(|active| active == name) {
            if contested {
                ConnectionKind::Contested
            } else {
                ConnectionKind::Active
            }
        } else {
            ConnectionKind::Disconnected
        };
        if entry.connection != previous {
            structure_changed = true;
            events.push(ControllerEvent::BackendChanged(entry.identity.clone()));
        }
    }

    inner.active = active;
    let text_providers = snapshot.text_providers().to_vec();
    if inner.text_providers != text_providers {
        inner.text_providers = text_providers;
        structure_changed = true;
    }
    let text_offers = snapshot.text_offers().to_vec();
    if inner.text_offers != text_offers {
        inner.text_offers = text_offers;
        structure_changed = true;
    }
    let strays = snapshot.strays().to_vec();
    if inner.strays != strays {
        inner.strays = strays;
        structure_changed = true;
    }

    if structure_changed {
        events.push(ControllerEvent::DiscoveryChanged);
    }
}

fn build_page(entry: &PageEntry) -> BackendPage {
    let (rows, errors, snapshot) = match &entry.snapshot {
        Some(snapshot) => {
            let rows = present_configuration(
                snapshot.configuration(),
                snapshot.models(),
                snapshot.engines(),
            );
            let errors = snapshot.errors().values().cloned().collect();
            (rows, errors, Some(snapshot.clone()))
        }
        None => (Vec::new(), Vec::new(), None),
    };
    let partial = snapshot
        .as_ref()
        .is_some_and(|snapshot| !snapshot.errors().is_empty());
    BackendPage {
        identity: entry.identity.clone(),
        connection: entry.connection,
        loading: entry.loading,
        partial,
        snapshot,
        rows,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        parse_engine_options, parse_model_options, parse_modelctl_config, ActiveBackendState,
        BackendSurface, ConfigValue,
    };

    fn parakeet() -> BackendIdentity {
        BackendIdentity::new("myna-parakeet", "provider")
    }

    fn whisper() -> BackendIdentity {
        BackendIdentity::new("myna-whisper", "provider")
    }

    fn funasr() -> BackendIdentity {
        BackendIdentity::new("myna-funasr", "provider")
    }

    fn discovery_connected_parakeet() -> ConnectionSnapshot {
        ConnectionSnapshot::new(
            vec![
                BackendIdentity::new("myna-parakeet", "provider"),
                BackendIdentity::new("myna-whisper", "provider"),
            ],
            ActiveBackendState::Connected(BackendIdentity::new("myna-parakeet", "provider")),
        )
    }

    fn discovery_only_parakeet() -> ConnectionSnapshot {
        ConnectionSnapshot::new(
            vec![BackendIdentity::new("myna-parakeet", "provider")],
            ActiveBackendState::Connected(BackendIdentity::new("myna-parakeet", "provider")),
        )
    }

    fn discovery_multi_connected() -> ConnectionSnapshot {
        ConnectionSnapshot::new(
            vec![
                BackendIdentity::new("myna-parakeet", "provider"),
                BackendIdentity::new("myna-whisper", "provider"),
            ],
            ActiveBackendState::MultiplyConnected(vec![
                BackendIdentity::new("myna-parakeet", "provider"),
                BackendIdentity::new("myna-whisper", "provider"),
            ]),
        )
    }

    fn discovery_three() -> ConnectionSnapshot {
        ConnectionSnapshot::new(
            vec![
                BackendIdentity::new("myna-funasr", "provider"),
                BackendIdentity::new("myna-parakeet", "provider"),
                BackendIdentity::new("myna-whisper", "provider"),
            ],
            ActiveBackendState::Connected(BackendIdentity::new("myna-parakeet", "provider")),
        )
    }

    fn snap_get_verbose_true() -> BackendSnapshot {
        let mut snapshot = BackendSnapshot::empty(parakeet());
        snapshot
            .set_modelctl_config(parse_modelctl_config("verbose: true\n").expect("modelctl parse"));
        snapshot
    }

    fn snap_get_verbose_false() -> BackendSnapshot {
        let mut snapshot = BackendSnapshot::empty(parakeet());
        snapshot.set_modelctl_config(
            parse_modelctl_config("verbose: false\n").expect("modelctl parse"),
        );
        snapshot
    }

    fn partial_snapshot() -> BackendSnapshot {
        let mut snapshot = BackendSnapshot::empty(parakeet());
        snapshot
            .set_modelctl_config(parse_modelctl_config("verbose: true\n").expect("modelctl parse"));
        snapshot.add_error(BackendSurfaceError::new(
            BackendSurface::Status,
            "modelctl status failed",
            "connection refused",
        ));
        snapshot
    }

    #[test]
    fn discovery_populates_pages_and_emits_events() {
        let controller = BackendController::detached();
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let events = events.clone();
            controller.observe(move |event| events.borrow_mut().push(event.clone()));
        }

        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));

        let pages = controller.pages();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].identity(), &parakeet());
        assert_eq!(pages[0].connection(), ConnectionKind::Active);
        assert_eq!(pages[1].identity(), &whisper());
        assert_eq!(pages[1].connection(), ConnectionKind::Disconnected);
        assert!(events
            .borrow()
            .iter()
            .any(|event| matches!(event, ControllerEvent::DiscoveryChanged)));
    }

    #[test]
    fn successful_empty_discovery_emits_completion() {
        let controller = BackendController::detached();
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let events = events.clone();
            controller.observe(move |event| events.borrow_mut().push(event.clone()));
        }

        let request = controller.begin_discovery();
        controller.complete_discovery(
            request,
            Ok(ConnectionSnapshot::new(
                vec![],
                ActiveBackendState::Disconnected,
            )),
        );

        assert!(!controller.discovery_loading());
        assert!(controller.pages().is_empty());
        assert!(events
            .borrow()
            .iter()
            .any(|event| matches!(event, ControllerEvent::DiscoveryChanged)));
    }

    #[test]
    fn removed_backend_page_disappears_and_inflight_snapshot_is_cancelled() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));
        let snapshot_request = controller
            .begin_snapshot("myna-whisper")
            .expect("whisper snapshot request");
        let token = snapshot_request.token();

        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_only_parakeet()));

        assert!(token.is_cancelled(), "whisper snapshot token cancelled");
        let pages = controller.pages();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].identity(), &parakeet());
        assert!(controller.page("myna-whisper").is_none());
    }

    #[test]
    fn partial_snapshot_is_visible_with_errors() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));

        let snapshot_request = controller.begin_snapshot("myna-parakeet").unwrap();
        controller.complete_snapshot(snapshot_request, partial_snapshot());

        let page = controller.page("myna-parakeet").expect("page exists");
        assert!(page.partial(), "partial snapshots must be flagged");
        assert!(!page.errors().is_empty(), "surface errors surfaced");
        assert!(
            page.rows().iter().any(|row| row.key() == "verbose"),
            "known data still shown"
        );
        assert!(!page.loading());
    }

    #[test]
    fn multiple_connections_are_flagged_as_contested() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_multi_connected()));

        let pages = controller.pages();
        assert_eq!(pages.len(), 2);
        assert!(pages
            .iter()
            .all(|page| page.connection() == ConnectionKind::Contested));
        assert!(matches!(
            controller.connection_snapshot().active_state(),
            ActiveBackendState::MultiplyConnected(_)
        ));
    }

    #[test]
    fn connection_changes_emit_backend_page_rebuild_events() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let events = events.clone();
            controller.observe(move |event| events.borrow_mut().push(event.clone()));
        }

        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_multi_connected()));

        assert!(events.borrow().iter().any(
            |event| matches!(event, ControllerEvent::BackendChanged(identity)
                if identity == &whisper())
        ));
    }

    #[test]
    fn refresh_start_emits_loading_event() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_only_parakeet()));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let events = events.clone();
            controller.observe(move |event| events.borrow_mut().push(event.clone()));
        }

        let _request = controller.begin_snapshot("myna-parakeet").unwrap();

        assert!(controller.page("myna-parakeet").unwrap().loading());
        assert_eq!(
            events.borrow().as_slice(),
            &[ControllerEvent::BackendChanged(parakeet())]
        );
    }

    #[test]
    fn stale_snapshot_response_is_ignored_when_arriving_after_newer_one() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));

        let first = controller.begin_snapshot("myna-parakeet").unwrap();
        let second = controller.begin_snapshot("myna-parakeet").unwrap();
        assert!(first.token().is_cancelled(), "prior token cancelled");

        controller.complete_snapshot(second, snap_get_verbose_true());
        // Stale earlier response arrives after the newer one.
        controller.complete_snapshot(first, snap_get_verbose_false());

        let page = controller.page("myna-parakeet").expect("page exists");
        let row = page
            .rows()
            .iter()
            .find(|row| row.key() == "verbose")
            .expect("verbose row");
        assert_eq!(row.value(), &ConfigValue::Boolean(true));
    }

    #[test]
    fn stale_discovery_response_is_ignored_when_arriving_after_newer_one() {
        let controller = BackendController::detached();
        let first = controller.begin_discovery();
        let second = controller.begin_discovery();
        assert!(first.token().is_cancelled());

        controller.complete_discovery(second, Ok(discovery_three()));
        controller.complete_discovery(first, Ok(discovery_only_parakeet()));

        let pages = controller.pages();
        assert_eq!(pages.len(), 3);
        assert!(pages.iter().any(|page| page.identity() == &parakeet()));
        assert!(pages.iter().any(|page| page.identity() == &whisper()));
        assert!(pages.iter().any(|page| page.identity() == &funasr()));
    }

    #[test]
    fn cancel_all_cancels_discovery_and_snapshot_tokens() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));

        let snapshot_request = controller.begin_snapshot("myna-parakeet").unwrap();
        let snapshot_token = snapshot_request.token();
        let discovery_request = controller.begin_discovery();
        let discovery_token = discovery_request.token();

        controller.cancel_all();

        assert!(snapshot_token.is_cancelled());
        assert!(discovery_token.is_cancelled());
        assert!(!controller.discovery_loading());
    }

    #[test]
    fn cancel_page_only_cancels_that_backends_token() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));

        let parakeet_request = controller.begin_snapshot("myna-parakeet").unwrap();
        let whisper_request = controller.begin_snapshot("myna-whisper").unwrap();

        controller.cancel_page("myna-parakeet");

        assert!(parakeet_request.token().is_cancelled());
        assert!(!whisper_request.token().is_cancelled());
    }

    #[test]
    fn discovery_failure_reports_error_without_removing_existing_pages() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));

        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let events = events.clone();
            controller.observe(move |event| events.borrow_mut().push(event.clone()));
        }

        let request = controller.begin_discovery();
        let error = BackendSurfaceError::new(
            BackendSurface::Connections,
            "snap connections failed",
            "authorization required",
        );
        controller.complete_discovery(request, Err(error.clone()));

        assert_eq!(controller.pages().len(), 2, "prior pages preserved");
        assert_eq!(controller.last_discovery_error(), Some(error.clone()));
        assert!(events.borrow().iter().any(
            |event| matches!(event, ControllerEvent::DiscoveryFailed(actual) if actual == &error)
        ));

        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));
        assert_eq!(controller.last_discovery_error(), None);
    }

    #[test]
    fn model_and_engine_choices_come_from_modelctl_options() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));

        let mut snapshot = BackendSnapshot::empty(parakeet());
        snapshot
            .set_modelctl_config(parse_modelctl_config("verbose: true\n").expect("modelctl parse"));
        let models = parse_model_options(
            r#"{"active-model":"parakeet-ctc-en","models":[{"name":"parakeet-ctc-en"},{"name":"parakeet-tdt-en"}]}"#,
        )
        .unwrap();
        snapshot.set_models(models);
        let engines = parse_engine_options(
            r#"{"active-engine":"onnxruntime","engines":[{"name":"onnxruntime","compatible":true,"model":{"default":"parakeet-ctc-en","options":["parakeet-ctc-en"]}},{"name":"tensorrt","compatible":true,"model":{"default":"parakeet-ctc-en","options":["parakeet-ctc-en"]}}]}"#,
        )
        .unwrap();
        snapshot.set_engines(engines);

        let snapshot_request = controller.begin_snapshot("myna-parakeet").unwrap();
        controller.complete_snapshot(snapshot_request, snapshot);

        let page = controller.page("myna-parakeet").expect("page exists");
        let model_row = page
            .rows()
            .iter()
            .find(|row| row.key() == "model")
            .expect("model row");
        assert_eq!(
            model_row.choices(),
            &["parakeet-ctc-en".to_owned(), "parakeet-tdt-en".to_owned(),]
        );
        let engine_row = page
            .rows()
            .iter()
            .find(|row| row.key() == "engine")
            .expect("engine row");
        assert_eq!(
            engine_row.choices(),
            &["onnxruntime".to_owned(), "tensorrt".to_owned()]
        );
    }

    fn recorded(controller: &BackendController) -> Rc<RefCell<Vec<ControllerEvent>>> {
        let events = Rc::new(RefCell::new(Vec::new()));
        controller.observe({
            let events = events.clone();
            move |event| events.borrow_mut().push(event.clone())
        });
        events
    }

    fn discovered_parakeet() -> Rc<BackendController> {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));
        controller
    }

    fn later() -> std::time::Instant {
        std::time::Instant::now() + FOCUS_REDISCOVERY_INTERVAL
    }

    #[test]
    fn focus_rediscovery_waits_out_the_interval_after_any_discovery() {
        let controller = discovered_parakeet();
        assert!(controller
            .begin_focus_discovery(std::time::Instant::now())
            .is_none());
        let request = controller
            .begin_focus_discovery(later())
            .expect("admitted once the interval passed");
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));
        assert!(
            controller.begin_focus_discovery(later()).is_none(),
            "a focus rediscovery starts the interval again"
        );
    }

    #[test]
    fn focus_rediscovery_is_refused_while_any_discovery_runs() {
        let controller = discovered_parakeet();
        let _explicit = controller.begin_discovery();
        let far = later() + FOCUS_REDISCOVERY_INTERVAL * 100;
        assert!(controller.begin_focus_discovery(far).is_none());

        let controller = discovered_parakeet();
        assert!(controller.begin_focus_discovery(later()).is_some());
        assert!(
            controller.begin_focus_discovery(far).is_none(),
            "a second focus rediscovery joins the first"
        );
    }

    #[test]
    fn an_unchanged_focus_rediscovery_announces_nothing() {
        let controller = discovered_parakeet();
        let events = recorded(&controller);
        let request = controller.begin_focus_discovery(later()).expect("admitted");
        assert!(!controller.discovery_loading(), "it shows no loading state");
        assert!(controller.complete_discovery(request, Ok(discovery_connected_parakeet())));
        assert!(events.borrow().is_empty(), "{:?}", events.borrow());
        assert!(!controller.discovery_loading());
    }

    #[test]
    fn a_focus_rediscovery_announces_a_newly_installed_backend() {
        let controller = discovered_parakeet();
        let events = recorded(&controller);
        let request = controller.begin_focus_discovery(later()).expect("admitted");
        controller.complete_discovery(request, Ok(discovery_three()));
        assert!(controller.page("myna-funasr").is_some());
        assert!(events
            .borrow()
            .iter()
            .any(|event| matches!(event, ControllerEvent::DiscoveryChanged)));
        assert!(!events
            .borrow()
            .iter()
            .any(|event| matches!(event, ControllerEvent::DiscoveryStarted)));
    }

    #[test]
    fn an_explicit_discovery_supersedes_a_focus_one() {
        let controller = discovered_parakeet();
        let focus = controller.begin_focus_discovery(later()).expect("admitted");
        let explicit = controller.begin_discovery();
        assert!(focus.token().is_cancelled());
        assert!(controller.discovery_loading());
        assert!(!controller.complete_discovery(focus, Ok(discovery_three())));
        assert!(controller.complete_discovery(explicit, Ok(discovery_connected_parakeet())));
        assert!(controller.page("myna-funasr").is_none());
    }

    #[test]
    fn a_focus_rediscovery_reports_a_failure_once() {
        let controller = discovered_parakeet();
        let events = recorded(&controller);
        let error = BackendSurfaceError::new(
            BackendSurface::Connections,
            "snap connections failed",
            "snapd is not running",
        );
        let failed = |controller: &BackendController, at| {
            let request = controller.begin_focus_discovery(at).expect("admitted");
            controller.complete_discovery(request, Err(error.clone()));
        };
        failed(&controller, later());
        failed(&controller, later() + FOCUS_REDISCOVERY_INTERVAL);
        let failures = events
            .borrow()
            .iter()
            .filter(|event| matches!(event, ControllerEvent::DiscoveryFailed(_)))
            .count();
        assert_eq!(failures, 1);

        let request = controller
            .begin_focus_discovery(later() + FOCUS_REDISCOVERY_INTERVAL * 2)
            .expect("admitted");
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));
        assert_eq!(controller.last_discovery_error(), None);
        assert!(
            matches!(
                events.borrow().last(),
                Some(ControllerEvent::DiscoveryChanged)
            ),
            "recovering clears the failure"
        );
    }

    #[test]
    fn focus_rediscovery_preserves_and_announces_text_provider_and_stray_changes() {
        let controller = discovered_parakeet();
        let events = recorded(&controller);
        let with_text = discovery_connected_parakeet().with_companions(
            vec![BackendIdentity::new("myna-text", "provider")],
            vec![BackendIdentity::new("legacy", "ubustt-socket")],
        );
        let request = controller.begin_focus_discovery(later()).expect("admitted");
        assert!(controller.complete_discovery(request, Ok(with_text.clone())));
        assert_eq!(controller.connection_snapshot(), with_text);
        assert_eq!(*events.borrow(), [ControllerEvent::DiscoveryChanged]);
    }

    #[test]
    fn a_text_snap_that_gains_a_model_is_announced_and_kept() {
        let controller = discovered_parakeet();
        let events = recorded(&controller);
        let offer = |models| {
            discovery_connected_parakeet().with_text_offers(vec![TextOffer::new(
                BackendIdentity::new("myna-text", "provider"),
                true,
                models,
            )])
        };
        let cleanup = crate::domain::TextModel::new("text.cleanup", "en", None);
        for (round, models) in [Vec::new(), vec![cleanup]].into_iter().enumerate() {
            let at = later() + FOCUS_REDISCOVERY_INTERVAL * (round as u32 + 1);
            let request = controller.begin_focus_discovery(at).expect("admitted");
            assert!(controller.complete_discovery(request, Ok(offer(models.clone()))));
            assert_eq!(controller.text_offers()[0].models(), models.as_slice());
        }
        assert_eq!(
            *events.borrow(),
            [
                ControllerEvent::DiscoveryChanged,
                ControllerEvent::DiscoveryChanged
            ]
        );
    }
}
