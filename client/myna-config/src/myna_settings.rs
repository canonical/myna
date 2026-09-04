//! GTK-independent state and behavior for the Myna preferences page.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use crate::domain::{ClientSettingMetadata, ClientSettingValue, SettingRange};
use crate::ports::{ClientSettings, ClientSettingsError, ClientSettingsSubscription};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetKind {
    Choice,
    Text,
    Shortcut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WidgetPlan {
    pub key: String,
    pub title: String,
    pub description: String,
    pub kind: WidgetKind,
    pub choices: Vec<String>,
    pub writable: bool,
}

pub fn widget_plan(metadata: &ClientSettingMetadata) -> WidgetPlan {
    let kind = match metadata.range() {
        SettingRange::Choices(_) => WidgetKind::Choice,
        _ if metadata.key().as_str() == "hotkey" => WidgetKind::Shortcut,
        _ => WidgetKind::Text,
    };
    WidgetPlan {
        key: metadata.key().as_str().to_owned(),
        title: metadata
            .summary()
            .unwrap_or(metadata.key().as_str())
            .to_owned(),
        description: metadata.description().unwrap_or_default().trim().to_owned(),
        kind,
        choices: match metadata.range() {
            SettingRange::Choices(choices) => choices.clone(),
            _ => Vec::new(),
        },
        writable: metadata.writable(),
    }
}

pub fn choice_display_label(choice: &str) -> String {
    match choice {
        "auto" => gettextrs::gettext("Automatic"),
        "streaming" => gettextrs::gettext("Streaming"),
        "batch" => gettextrs::gettext("Batch"),
        "portal" => gettextrs::gettext("Portal"),
        "control" => gettextrs::gettext("Control socket"),
        "ribbon" => gettextrs::gettext("Ribbon"),
        "vumeter" => gettextrs::gettext("VU meter"),
        "bar" => gettextrs::gettext("Bar"),
        "progress" => gettextrs::gettext("Progress"),
        unknown => unknown.to_owned(),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct DebouncedTextCommit {
    committed: String,
    pending: Option<(u64, String)>,
    revision: u64,
}

impl DebouncedTextCommit {
    pub fn new(committed: impl Into<String>) -> Self {
        Self {
            committed: committed.into(),
            pending: None,
            revision: 0,
        }
    }

    pub fn changed(&mut self, value: &str) -> Option<u64> {
        self.revision = self.revision.wrapping_add(1);
        if value == self.committed {
            self.pending = None;
            return None;
        }
        self.pending = Some((self.revision, value.to_owned()));
        Some(self.revision)
    }

    pub fn take(&mut self, revision: u64) -> Option<String> {
        match self.pending.take() {
            Some((pending_revision, value)) if pending_revision == revision => Some(value),
            Some(pending) => {
                self.pending = Some(pending);
                None
            }
            None => None,
        }
    }

    pub fn apply(&mut self, value: &str) -> Option<String> {
        self.revision = self.revision.wrapping_add(1);
        self.pending = None;
        (value != self.committed).then(|| value.to_owned())
    }

    pub fn flush(&mut self) -> Option<String> {
        self.revision = self.revision.wrapping_add(1);
        self.pending.take().map(|(_, value)| value)
    }

    pub fn committed(&mut self, value: impl Into<String>) {
        self.committed = value.into();
        self.pending = None;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingRow {
    metadata: ClientSettingMetadata,
    value: ClientSettingValue,
    pending: bool,
}

impl SettingRow {
    pub fn metadata(&self) -> &ClientSettingMetadata {
        &self.metadata
    }

    pub fn value(&self) -> &ClientSettingValue {
        &self.value
    }

    pub fn pending(&self) -> bool {
        self.pending
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageState {
    Loading,
    Empty,
    Ready(Vec<SettingRow>),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsEvent {
    RowChanged {
        key: String,
        value: ClientSettingValue,
        pending: bool,
    },
    SaveFailed {
        key: String,
        detail: String,
    },
}

type Observer = Box<dyn Fn(SettingsEvent)>;

#[derive(Clone, Debug)]
enum PersistenceOperation {
    Set(ClientSettingValue),
    Reset,
}

#[derive(Debug)]
struct PersistenceGate {
    revision: AtomicU64,
    persisted: Mutex<ClientSettingValue>,
}

#[derive(Clone, Debug)]
pub struct PersistenceRequest {
    key: String,
    revision: u64,
    original: ClientSettingValue,
    requested: ClientSettingValue,
    operation: PersistenceOperation,
    gate: Arc<PersistenceGate>,
    writer: Arc<Mutex<()>>,
}

#[derive(Clone, Debug)]
pub enum PersistenceCompletion {
    Written(ClientSettingValue),
    Superseded(ClientSettingValue),
}

impl PersistenceRequest {
    pub fn persist(
        &self,
        settings: &dyn ClientSettings,
    ) -> Result<PersistenceCompletion, ClientSettingsError> {
        let _writer = self
            .writer
            .lock()
            .map_err(|_| ClientSettingsError::StoreUnavailable {
                message: "the settings persistence queue is unavailable".into(),
            })?;
        if self.gate.revision.load(Ordering::Acquire) != self.revision {
            return Ok(PersistenceCompletion::Superseded(self.requested.clone()));
        }
        match &self.operation {
            PersistenceOperation::Set(value) => {
                settings.set(&self.key, value.clone())?;
                *self.gate.persisted.lock().map_err(|_| {
                    ClientSettingsError::StoreUnavailable {
                        message: "the settings persistence state is unavailable".into(),
                    }
                })? = value.clone();
                Ok(PersistenceCompletion::Written(value.clone()))
            }
            PersistenceOperation::Reset => {
                settings.reset(&self.key)?;
                let value = settings.get(&self.key)?;
                *self.gate.persisted.lock().map_err(|_| {
                    ClientSettingsError::StoreUnavailable {
                        message: "the settings persistence state is unavailable".into(),
                    }
                })? = value.clone();
                Ok(PersistenceCompletion::Written(value))
            }
        }
    }

    fn rollback_value(&self) -> ClientSettingValue {
        self.gate
            .persisted
            .lock()
            .map(|value| value.clone())
            .unwrap_or_else(|_| self.original.clone())
    }
}

type PersistenceResult = Result<PersistenceCompletion, ClientSettingsError>;

struct PersistenceWork {
    request: PersistenceRequest,
    completion: mpsc::SyncSender<PersistenceResult>,
}

struct PersistenceWriterInner {
    sender: Mutex<Option<mpsc::Sender<PersistenceWork>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Drop for PersistenceWriterInner {
    fn drop(&mut self) {
        self.sender.get_mut().ok().and_then(Option::take);
        if let Some(worker) = self.worker.get_mut().ok().and_then(Option::take) {
            std::thread::spawn(move || {
                let _ = worker.join();
            });
        }
    }
}

#[derive(Clone)]
pub struct PersistenceWriter {
    inner: Arc<PersistenceWriterInner>,
}

pub struct PersistenceJob {
    completion: mpsc::Receiver<PersistenceResult>,
}

impl PersistenceWriter {
    pub fn spawn<F, S>(open: F) -> Self
    where
        F: FnOnce() -> Result<S, ClientSettingsError> + Send + 'static,
        S: ClientSettings + 'static,
    {
        let (sender, receiver) = mpsc::channel::<PersistenceWork>();
        let worker = std::thread::spawn(move || {
            let settings = open();
            while let Ok(work) = receiver.recv() {
                let result = match &settings {
                    Ok(settings) => work.request.persist(settings),
                    Err(error) => Err(error.clone()),
                };
                let _ = work.completion.send(result);
            }
        });
        Self {
            inner: Arc::new(PersistenceWriterInner {
                sender: Mutex::new(Some(sender)),
                worker: Mutex::new(Some(worker)),
            }),
        }
    }

    pub fn submit(
        &self,
        request: PersistenceRequest,
    ) -> Result<PersistenceJob, ClientSettingsError> {
        let (completion, receiver) = mpsc::sync_channel(1);
        self.inner
            .sender
            .lock()
            .map_err(|_| persistence_queue_unavailable())?
            .as_ref()
            .ok_or_else(persistence_queue_unavailable)?
            .send(PersistenceWork {
                request,
                completion,
            })
            .map_err(|_| persistence_queue_unavailable())?;
        Ok(PersistenceJob {
            completion: receiver,
        })
    }
}

impl PersistenceJob {
    pub fn wait(self) -> PersistenceResult {
        self.completion
            .recv()
            .unwrap_or_else(|_| Err(persistence_queue_unavailable()))
    }
}

fn persistence_queue_unavailable() -> ClientSettingsError {
    ClientSettingsError::StoreUnavailable {
        message: "the settings persistence queue is unavailable".into(),
    }
}

pub struct MynaSettingsController {
    settings: Rc<dyn ClientSettings>,
    state: RefCell<PageState>,
    persistence_gates: RefCell<BTreeMap<String, Arc<PersistenceGate>>>,
    persistence_writer: Arc<Mutex<()>>,
    observed_echoes: RefCell<BTreeMap<String, u64>>,
    expected_echoes: RefCell<BTreeMap<String, Vec<ClientSettingValue>>>,
    observers: RefCell<Vec<Observer>>,
    _subscription: RefCell<Option<Box<dyn ClientSettingsSubscription>>>,
}

impl MynaSettingsController {
    pub fn load(settings: Rc<dyn ClientSettings>) -> Rc<Self> {
        let controller = Rc::new(Self {
            settings,
            state: RefCell::new(PageState::Loading),
            persistence_gates: RefCell::new(BTreeMap::new()),
            persistence_writer: Arc::new(Mutex::new(())),
            observed_echoes: RefCell::new(BTreeMap::new()),
            expected_echoes: RefCell::new(BTreeMap::new()),
            observers: RefCell::new(Vec::new()),
            _subscription: RefCell::new(None),
        });

        let state = match controller.settings.list() {
            Ok(rows) if rows.is_empty() => PageState::Empty,
            Ok(rows) => PageState::Ready(
                rows.into_iter()
                    .map(|metadata| SettingRow {
                        value: metadata.current_value().clone(),
                        metadata,
                        pending: false,
                    })
                    .collect(),
            ),
            Err(error) => PageState::Error(error.to_string()),
        };
        *controller.state.borrow_mut() = state;

        if matches!(*controller.state.borrow(), PageState::Ready(_)) {
            let weak = Rc::downgrade(&controller);
            match controller.settings.subscribe(Box::new(move |change| {
                if let Some(controller) = Weak::upgrade(&weak) {
                    controller.apply_external(change.key().as_str(), change.value().clone());
                }
            })) {
                Ok(subscription) => {
                    *controller._subscription.borrow_mut() = Some(subscription);
                }
                Err(error) => {
                    *controller.state.borrow_mut() = PageState::Error(error.to_string());
                }
            }
        }

        controller
    }

    pub fn state(&self) -> PageState {
        self.state.borrow().clone()
    }

    pub fn row(&self, key: &str) -> Option<SettingRow> {
        self.with_rows(|rows| {
            rows.iter()
                .find(|row| row.metadata.key().as_str() == key)
                .cloned()
        })
        .flatten()
    }

    pub fn observe(&self, observer: impl Fn(SettingsEvent) + 'static) {
        self.observers.borrow_mut().push(Box::new(observer));
    }

    pub fn set(
        &self,
        key: &str,
        value: ClientSettingValue,
    ) -> Result<PersistenceRequest, ClientSettingsError> {
        self.ensure_writable(key)?;
        let original = self
            .row(key)
            .ok_or_else(|| ClientSettingsError::UnknownKey {
                key: key.to_owned(),
            })?
            .value;
        let (revision, gate) = self.next_revision(key);
        self.update_row(key, value.clone(), true);
        Ok(PersistenceRequest {
            key: key.to_owned(),
            revision,
            original,
            requested: value.clone(),
            operation: PersistenceOperation::Set(value),
            gate,
            writer: Arc::clone(&self.persistence_writer),
        })
    }

    pub fn reset(&self, key: &str) -> Result<PersistenceRequest, ClientSettingsError> {
        self.ensure_writable(key)?;
        let row = self
            .row(key)
            .ok_or_else(|| ClientSettingsError::UnknownKey {
                key: key.to_owned(),
            })?;
        let original = row.value;
        let target = row.metadata.default_value().clone();
        let (revision, gate) = self.next_revision(key);
        self.update_row(key, target.clone(), true);
        Ok(PersistenceRequest {
            key: key.to_owned(),
            revision,
            original,
            requested: target,
            operation: PersistenceOperation::Reset,
            gate,
            writer: Arc::clone(&self.persistence_writer),
        })
    }

    pub fn complete(
        &self,
        request: PersistenceRequest,
        result: Result<PersistenceCompletion, ClientSettingsError>,
    ) {
        if let Ok(PersistenceCompletion::Written(value)) = &result {
            let current = self.revision(&request.key);
            let newer_local_save = current != request.revision
                && self.row(&request.key).is_some_and(|row| row.pending());
            if (current == request.revision || newer_local_save)
                && self.observed_echoes.borrow_mut().remove(&request.key) != Some(request.revision)
            {
                self.expected_echoes
                    .borrow_mut()
                    .entry(request.key.clone())
                    .or_default()
                    .push(value.clone());
            }
        }
        if self.revision(&request.key) != request.revision {
            return;
        }
        match result {
            Ok(PersistenceCompletion::Written(value)) => {
                self.update_row(&request.key, value, false)
            }
            Ok(PersistenceCompletion::Superseded(_)) => {}
            Err(error) => {
                self.update_row(&request.key, request.rollback_value(), false);
                self.emit(SettingsEvent::SaveFailed {
                    key: request.key,
                    detail: error.to_string(),
                });
            }
        }
    }

    fn ensure_writable(&self, key: &str) -> Result<(), ClientSettingsError> {
        match self.row(key) {
            Some(row) if row.metadata.writable() => Ok(()),
            Some(_) => Err(ClientSettingsError::NotWritable {
                key: key.to_owned(),
            }),
            None => Err(ClientSettingsError::UnknownKey {
                key: key.to_owned(),
            }),
        }
    }

    fn apply_external(&self, key: &str, value: ClientSettingValue) {
        if self.consume_expected_echo(key, &value) {
            return;
        }
        if let Some(row) = self.row(key) {
            if row.pending && row.value == value {
                self.observed_echoes
                    .borrow_mut()
                    .insert(key.to_owned(), self.revision(key));
                return;
            }
            let (_, gate) = self.next_revision(key);
            if let Ok(mut persisted) = gate.persisted.lock() {
                *persisted = value.clone();
            }
            self.update_row(key, value, false);
        }
    }

    fn consume_expected_echo(&self, key: &str, value: &ClientSettingValue) -> bool {
        let mut echoes = self.expected_echoes.borrow_mut();
        let Some(values) = echoes.get_mut(key) else {
            return false;
        };
        let Some(index) = values.iter().position(|candidate| candidate == value) else {
            return false;
        };
        values.remove(index);
        if values.is_empty() {
            echoes.remove(key);
        }
        true
    }

    fn next_revision(&self, key: &str) -> (u64, Arc<PersistenceGate>) {
        let initial = self
            .row(key)
            .map(|row| row.value)
            .unwrap_or_else(|| ClientSettingValue::Text(String::new()));
        let gate = self
            .persistence_gates
            .borrow_mut()
            .entry(key.to_owned())
            .or_insert_with(|| {
                Arc::new(PersistenceGate {
                    revision: AtomicU64::new(0),
                    persisted: Mutex::new(initial),
                })
            })
            .clone();
        let revision = gate.revision.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        (revision, gate)
    }

    fn revision(&self, key: &str) -> u64 {
        self.persistence_gates
            .borrow()
            .get(key)
            .map(|gate| gate.revision.load(Ordering::Acquire))
            .unwrap_or_default()
    }

    fn update_row(&self, key: &str, value: ClientSettingValue, pending: bool) {
        self.with_rows_mut(|rows| {
            if let Some(row) = rows
                .iter_mut()
                .find(|row| row.metadata.key().as_str() == key)
            {
                row.value = value.clone();
                row.pending = pending;
            }
        });
        self.emit(SettingsEvent::RowChanged {
            key: key.to_owned(),
            value,
            pending,
        });
    }

    fn emit(&self, event: SettingsEvent) {
        for observer in self.observers.borrow().iter() {
            observer(event.clone());
        }
    }

    fn with_rows<T>(&self, operation: impl FnOnce(&[SettingRow]) -> T) -> Option<T> {
        match &*self.state.borrow() {
            PageState::Ready(rows) => Some(operation(rows)),
            _ => None,
        }
    }

    fn with_rows_mut(&self, operation: impl FnOnce(&mut [SettingRow])) {
        if let PageState::Ready(rows) = &mut *self.state.borrow_mut() {
            operation(rows);
        }
    }
}
