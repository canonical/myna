//! GTK-independent controller/presenter for backend pages.
//!
//! The controller owns the state that the adaptive sidebar and per-backend
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

use crate::command::CancellationToken;
use crate::domain::{
    ActiveBackendState, BackendIdentity, BackendSnapshot, BackendSurfaceError, ConfigValue,
    ConnectionSnapshot,
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

/// A single presentation row on a backend page, combined with any staged
/// dirty edit the user has made locally.
#[derive(Clone, Debug, PartialEq)]
pub struct BackendRow {
    presentation: PresentationRow,
    effective_value: ConfigValue,
    dirty: bool,
}

impl BackendRow {
    pub fn presentation(&self) -> &PresentationRow {
        &self.presentation
    }

    pub fn effective_value(&self) -> &ConfigValue {
        &self.effective_value
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }
}

/// Public, read-only view of a single backend page.
#[derive(Clone, Debug, PartialEq)]
pub struct BackendPage {
    identity: BackendIdentity,
    connection: ConnectionKind,
    loading: bool,
    partial: bool,
    snapshot: Option<BackendSnapshot>,
    rows: Vec<BackendRow>,
    errors: Vec<BackendSurfaceError>,
    dirty_keys: Vec<String>,
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

    pub fn rows(&self) -> &[BackendRow] {
        &self.rows
    }

    pub fn errors(&self) -> &[BackendSurfaceError] {
        &self.errors
    }

    pub fn dirty_keys(&self) -> &[String] {
        &self.dirty_keys
    }

    /// Concise summary suitable for the sidebar subtitle.
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

/// Sidebar-friendly summary of a backend's high-level state.
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
    /// A backend page's snapshot or dirty state changed.
    BackendChanged(BackendIdentity),
    /// A locally staged value changed without altering the snapshot structure.
    BackendDirtyChanged(BackendIdentity),
}

/// Handle for an in-flight discovery request.
#[derive(Clone, Debug)]
pub struct DiscoveryRequest {
    generation: u64,
    token: CancellationToken,
}

impl DiscoveryRequest {
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    pub fn generation(&self) -> u64 {
        self.generation
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

    pub fn snap(&self) -> &str {
        &self.snap
    }

    pub fn generation(&self) -> u64 {
        self.generation
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
    discovery_generation: u64,
    latest_completed_discovery: u64,
    discovery_token: Option<CancellationToken>,
    discovery_loading: bool,
    last_discovery_error: Option<BackendSurfaceError>,
}

struct PageEntry {
    identity: BackendIdentity,
    connection: ConnectionKind,
    snapshot: Option<BackendSnapshot>,
    dirty: BTreeMap<String, ConfigValue>,
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

    pub fn active_state(&self) -> ActiveBackendState {
        self.inner.borrow().active.clone()
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
    }

    pub fn begin_discovery(&self) -> DiscoveryRequest {
        let request = {
            let mut inner = self.inner.borrow_mut();
            if let Some(previous) = inner.discovery_token.take() {
                previous.cancel();
            }
            inner.discovery_generation += 1;
            inner.discovery_loading = true;
            let token = CancellationToken::new();
            inner.discovery_token = Some(token.clone());
            DiscoveryRequest {
                generation: inner.discovery_generation,
                token,
            }
        };
        self.emit(vec![ControllerEvent::DiscoveryStarted]);
        request
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
                    inner.last_discovery_error = None;
                    apply_discovery(&mut inner, snapshot, &mut events);
                    if !events.contains(&ControllerEvent::DiscoveryChanged) {
                        events.push(ControllerEvent::DiscoveryChanged);
                    }
                }
                Err(error) => {
                    inner.last_discovery_error = Some(error.clone());
                    events.push(ControllerEvent::DiscoveryFailed(error));
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
            prune_stale_dirty_edits(entry);
            events.push(ControllerEvent::BackendChanged(entry.identity.clone()));
        }
        self.emit(events);
    }

    pub fn stage_edit(&self, snap: &str, key: &str, value: ConfigValue) -> bool {
        let mut events = Vec::new();
        let changed = {
            let mut inner = self.inner.borrow_mut();
            let Some(entry) = inner.pages.get_mut(snap) else {
                return false;
            };
            let original = original_value(entry, key);
            if original.as_ref() == Some(&value) {
                if entry.dirty.remove(key).is_some() {
                    events.push(ControllerEvent::BackendDirtyChanged(entry.identity.clone()));
                    true
                } else {
                    false
                }
            } else if entry.dirty.get(key) == Some(&value) {
                false
            } else {
                entry.dirty.insert(key.to_owned(), value);
                events.push(ControllerEvent::BackendDirtyChanged(entry.identity.clone()));
                true
            }
        };
        self.emit(events);
        changed
    }

    pub fn revert_all_edits(&self, snap: &str) -> bool {
        let mut events = Vec::new();
        let changed = {
            let mut inner = self.inner.borrow_mut();
            let Some(entry) = inner.pages.get_mut(snap) else {
                return false;
            };
            if entry.dirty.is_empty() {
                false
            } else {
                entry.dirty.clear();
                events.push(ControllerEvent::BackendDirtyChanged(entry.identity.clone()));
                true
            }
        };
        self.emit(events);
        changed
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
            prune_stale_dirty_edits(entry);
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

impl Default for Inner {
    fn default() -> Self {
        Self {
            pages: BTreeMap::new(),
            order: Vec::new(),
            active: ActiveBackendState::Disconnected,
            discovery_generation: 0,
            latest_completed_discovery: 0,
            discovery_token: None,
            discovery_loading: false,
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
                    dirty: BTreeMap::new(),
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
        ActiveBackendState::Disconnected | ActiveBackendState::FailedSwitch { .. } => Vec::new(),
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

    if structure_changed {
        events.push(ControllerEvent::DiscoveryChanged);
    }
}

fn original_value(entry: &PageEntry, key: &str) -> Option<ConfigValue> {
    let snapshot = entry.snapshot.as_ref()?;
    presented_value(snapshot, key)
}

fn prune_stale_dirty_edits(entry: &mut PageEntry) {
    if entry.dirty.is_empty() {
        return;
    }
    let Some(snapshot) = entry.snapshot.as_ref() else {
        return;
    };
    entry.dirty.retain(|key, value| {
        presented_value(snapshot, key).map_or(true, |current| current != *value)
    });
}

fn presented_value(snapshot: &BackendSnapshot, key: &str) -> Option<ConfigValue> {
    if key == "model" {
        return snapshot
            .models()
            .and_then(|models| models.active())
            .map(|value| ConfigValue::Text(value.to_owned()));
    }
    if key == "engine" {
        return snapshot
            .engines()
            .and_then(|engines| engines.active())
            .map(|value| ConfigValue::Text(value.to_owned()));
    }
    snapshot.configuration().effective(key).cloned()
}

fn build_page(entry: &PageEntry) -> BackendPage {
    let (rows, errors, snapshot) = match &entry.snapshot {
        Some(snapshot) => {
            let presented = present_configuration(
                snapshot.configuration(),
                snapshot.models(),
                snapshot.engines(),
            );
            let rows = presented
                .into_iter()
                .map(|presentation| build_row(&entry.dirty, presentation))
                .collect();
            let errors = snapshot.errors().values().cloned().collect();
            (rows, errors, Some(snapshot.clone()))
        }
        None => (Vec::new(), Vec::new(), None),
    };
    let partial = snapshot
        .as_ref()
        .is_some_and(|snapshot| !snapshot.errors().is_empty());
    let dirty_keys = entry.dirty.keys().cloned().collect();
    BackendPage {
        identity: entry.identity.clone(),
        connection: entry.connection,
        loading: entry.loading,
        partial,
        snapshot,
        rows,
        errors,
        dirty_keys,
    }
}

fn build_row(dirty: &BTreeMap<String, ConfigValue>, presentation: PresentationRow) -> BackendRow {
    let dirty_value = dirty.get(presentation.key()).cloned();
    let effective_value = dirty_value
        .clone()
        .unwrap_or_else(|| presentation.value().clone());
    BackendRow {
        presentation,
        effective_value,
        dirty: dirty_value.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        parse_connections, parse_engine_options, parse_model_options, parse_modelctl_config,
        BackendSurface,
    };

    fn parakeet() -> BackendIdentity {
        BackendIdentity::new("myna-parakeet", "provider")
    }

    fn whisper() -> BackendIdentity {
        BackendIdentity::new("myna-whisper", "provider")
    }

    fn nemotron() -> BackendIdentity {
        BackendIdentity::new("myna-nemotron", "provider")
    }

    fn discovery_connected_parakeet() -> ConnectionSnapshot {
        parse_connections(
            "Interface Plug Slot Notes\n\
             content[inference-provider] myna:backend myna-parakeet:provider manual\n\
             content - myna-whisper:provider -\n",
            "name: content\nslots:\n  - myna-parakeet:provider:\n      content: inference-provider\n  - myna-whisper:provider:\n      content: inference-provider\n",
        )
        .expect("connections parse")
    }

    fn discovery_only_parakeet() -> ConnectionSnapshot {
        parse_connections(
            "Interface Plug Slot Notes\n\
             content[inference-provider] myna:backend myna-parakeet:provider manual\n",
            "name: content\nslots:\n  - myna-parakeet:provider:\n      content: inference-provider\n",
        )
        .expect("connections parse")
    }

    fn discovery_multi_connected() -> ConnectionSnapshot {
        parse_connections(
            "Interface Plug Slot Notes\n\
             content[inference-provider] myna:backend myna-parakeet:provider manual\n\
             content[inference-provider] myna:backend myna-whisper:provider manual\n",
            "name: content\nslots:\n  - myna-parakeet:provider:\n      content: inference-provider\n  - myna-whisper:provider:\n      content: inference-provider\n",
        )
        .expect("connections parse")
    }

    fn discovery_three() -> ConnectionSnapshot {
        parse_connections(
            "Interface Plug Slot Notes\n\
             content[inference-provider] myna:backend myna-parakeet:provider manual\n\
             content - myna-whisper:provider -\n\
             content - myna-nemotron:provider -\n",
            "name: content\nslots:\n  - myna-parakeet:provider:\n      content: inference-provider\n  - myna-whisper:provider:\n      content: inference-provider\n  - myna-nemotron:provider:\n      content: inference-provider\n",
        )
        .expect("connections parse")
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
            "modelctl.parakeet",
            ["status", "--format=json"].map(str::to_owned).to_vec(),
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
            Ok(parse_connections(
                include_str!("../tests/fixtures/snap-connections-empty.txt"),
                "name: content\n",
            )
            .unwrap()),
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
            page.rows()
                .iter()
                .any(|row| row.presentation().key() == "verbose"),
            "known data still shown"
        );
        assert!(!page.loading());
    }

    #[test]
    fn refresh_preserves_dirty_edits_over_new_snapshot() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));
        let snapshot_request = controller.begin_snapshot("myna-parakeet").unwrap();
        controller.complete_snapshot(snapshot_request, snap_get_verbose_false());

        assert!(controller.stage_edit("myna-parakeet", "verbose", ConfigValue::Boolean(true),));

        // Simulate a background refresh replacing the snapshot.
        let snapshot_request = controller.begin_snapshot("myna-parakeet").unwrap();
        controller.complete_snapshot(snapshot_request, snap_get_verbose_false());

        let page = controller.page("myna-parakeet").expect("page exists");
        let row = page
            .rows()
            .iter()
            .find(|row| row.presentation().key() == "verbose")
            .expect("verbose row");
        assert!(row.dirty(), "dirty flag survives refresh");
        assert_eq!(row.effective_value(), &ConfigValue::Boolean(true));
        assert_eq!(page.dirty_keys(), &["verbose".to_owned()]);
    }

    #[test]
    fn restaging_the_same_edit_is_silent() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));
        let snapshot_request = controller.begin_snapshot("myna-parakeet").unwrap();
        controller.complete_snapshot(snapshot_request, snap_get_verbose_false());
        assert!(controller.stage_edit("myna-parakeet", "verbose", ConfigValue::Boolean(true)));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let events = events.clone();
            controller.observe(move |event| events.borrow_mut().push(event.clone()));
        }

        assert!(!controller.stage_edit("myna-parakeet", "verbose", ConfigValue::Boolean(true)));

        assert!(events.borrow().is_empty());
        assert_eq!(
            controller.page("myna-parakeet").unwrap().dirty_keys(),
            &["verbose".to_owned()]
        );
    }

    #[test]
    fn dirty_edit_is_cleared_when_snapshot_agrees() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));
        let snapshot_request = controller.begin_snapshot("myna-parakeet").unwrap();
        controller.complete_snapshot(snapshot_request, snap_get_verbose_false());

        controller.stage_edit("myna-parakeet", "verbose", ConfigValue::Boolean(true));

        let snapshot_request = controller.begin_snapshot("myna-parakeet").unwrap();
        controller.complete_snapshot(snapshot_request, snap_get_verbose_true());

        let page = controller.page("myna-parakeet").expect("page exists");
        let row = page
            .rows()
            .iter()
            .find(|row| row.presentation().key() == "verbose")
            .expect("verbose row");
        assert!(!row.dirty());
        assert!(page.dirty_keys().is_empty());
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
            controller.active_state(),
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
            .find(|row| row.presentation().key() == "verbose")
            .expect("verbose row");
        assert_eq!(row.effective_value(), &ConfigValue::Boolean(true));
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
        assert!(pages.iter().any(|page| page.identity() == &nemotron()));
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
            "snap",
            ["connections", "--all"].map(str::to_owned).to_vec(),
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
            .find(|row| row.presentation().key() == "model")
            .expect("model row");
        assert_eq!(
            model_row.presentation().choices(),
            &["parakeet-ctc-en".to_owned(), "parakeet-tdt-en".to_owned(),]
        );
        let engine_row = page
            .rows()
            .iter()
            .find(|row| row.presentation().key() == "engine")
            .expect("engine row");
        assert_eq!(
            engine_row.presentation().choices(),
            &["onnxruntime".to_owned(), "tensorrt".to_owned()]
        );
    }

    #[test]
    fn model_and_engine_dirty_edits_clear_when_refresh_agrees() {
        let controller = BackendController::detached();
        let request = controller.begin_discovery();
        controller.complete_discovery(request, Ok(discovery_connected_parakeet()));

        let snapshot = || {
            let mut snapshot = BackendSnapshot::empty(parakeet());
            snapshot.set_models(
                parse_model_options(
                    r#"{"active-model":"parakeet","models":[{"name":"parakeet"}]}"#,
                )
                .unwrap(),
            );
            snapshot.set_engines(
                parse_engine_options(
                    r#"{"active-engine":"cpu","engines":[{"name":"cpu","compatible":true}]}"#,
                )
                .unwrap(),
            );
            snapshot
        };

        let request = controller.begin_snapshot("myna-parakeet").unwrap();
        controller.complete_snapshot(request, snapshot());
        assert!(!controller.stage_edit(
            "myna-parakeet",
            "model",
            ConfigValue::Text("parakeet".into())
        ));
        assert!(!controller.stage_edit("myna-parakeet", "engine", ConfigValue::Text("cpu".into())));
    }
}
