use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use myna_core::language::ModelFamily;
use myna_core::provider_env::DeclaredRole;
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BackendIdentity {
    snap_name: String,
    slot_name: String,
    modelctl_app: Option<String>,
}

impl BackendIdentity {
    pub fn new(snap_name: impl Into<String>, slot_name: impl Into<String>) -> Self {
        Self {
            snap_name: snap_name.into(),
            slot_name: slot_name.into(),
            modelctl_app: None,
        }
    }

    pub fn with_modelctl_app(mut self, modelctl_app: impl Into<String>) -> Self {
        self.modelctl_app = Some(modelctl_app.into());
        self
    }

    pub fn snap_name(&self) -> &str {
        &self.snap_name
    }

    pub fn slot(&self) -> String {
        format!("{}:{}", self.snap_name, self.slot_name)
    }

    pub fn modelctl_app(&self) -> Option<&str> {
        self.modelctl_app.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientSettingKey(String);

impl ClientSettingKey {
    pub fn new(key: impl Into<String>) -> Result<Self, ValidationError> {
        let key = key.into();
        if key.trim().is_empty() {
            return Err(ValidationError::new("key", "settings key is empty"));
        }
        Ok(Self(key))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientSettingValue {
    Choice(String),
    Text(String),
    /// An integer-typed key (any GVariant integer width); the schema range
    /// bounds it.
    Integer(i64),
    Boolean(bool),
    /// A string-list (`as`) key, in stored order.
    List(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientSetting {
    key: ClientSettingKey,
    value: ClientSettingValue,
}

impl ClientSetting {
    pub fn new(key: ClientSettingKey, value: ClientSettingValue) -> Result<Self, ValidationError> {
        Ok(Self { key, value })
    }

    pub fn key(&self) -> &ClientSettingKey {
        &self.key
    }

    pub fn value(&self) -> &ClientSettingValue {
        &self.value
    }
}

impl ClientSettingValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Choice(value) | Self::Text(value) => Some(value),
            Self::Integer(_) | Self::Boolean(_) | Self::List(_) => None,
        }
    }

    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            Self::Choice(_) | Self::Text(_) | Self::Boolean(_) | Self::List(_) => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(value) => Some(*value),
            Self::Choice(_) | Self::Text(_) | Self::Integer(_) | Self::List(_) => None,
        }
    }

    pub fn as_list(&self) -> Option<&[String]> {
        match self {
            Self::List(values) => Some(values),
            Self::Choice(_) | Self::Text(_) | Self::Integer(_) | Self::Boolean(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingRange {
    Unrestricted,
    Choices(Vec<String>),
    Range {
        minimum: ClientSettingValue,
        maximum: ClientSettingValue,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientSettingMetadata {
    key: ClientSettingKey,
    summary: Option<String>,
    description: Option<String>,
    default_value: ClientSettingValue,
    range: SettingRange,
    current_value: ClientSettingValue,
    writable: bool,
}

impl ClientSettingMetadata {
    pub fn new(
        key: ClientSettingKey,
        summary: Option<String>,
        description: Option<String>,
        default_value: ClientSettingValue,
        range: SettingRange,
        current_value: ClientSettingValue,
        writable: bool,
    ) -> Self {
        Self {
            key,
            summary,
            description,
            default_value,
            range,
            current_value,
            writable,
        }
    }

    pub fn key(&self) -> &ClientSettingKey {
        &self.key
    }

    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn default_value(&self) -> &ClientSettingValue {
        &self.default_value
    }

    pub fn range(&self) -> &SettingRange {
        &self.range
    }

    pub fn current_value(&self) -> &ClientSettingValue {
        &self.current_value
    }

    pub fn writable(&self) -> bool {
        self.writable
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConfigValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Number(f64),
    Text(String),
}

/// The user layer of a backend's configuration, as `modelctl get` prints it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BackendConfiguration {
    values: BTreeMap<String, ConfigValue>,
}

impl BackendConfiguration {
    pub fn get(&self, key: &str) -> Option<&ConfigValue> {
        self.values.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ConfigValue)> {
        self.values.iter().map(|(key, value)| (key.as_str(), value))
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActiveBackendState {
    Disconnected,
    Connected(BackendIdentity),
    MultiplyConnected(Vec<BackendIdentity>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionSnapshot {
    backends: Vec<BackendIdentity>,
    active_state: ActiveBackendState,
    text_providers: Vec<BackendIdentity>,
    text_offers: Vec<TextOffer>,
    strays: Vec<BackendIdentity>,
}

/// One model a text-model snap carries, as its component's manifest says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextModel {
    task: String,
    language: String,
    to: Option<String>,
    license: String,
    attribution: String,
}

impl TextModel {
    pub fn new(task: impl Into<String>, language: impl Into<String>, to: Option<String>) -> Self {
        Self {
            task: task.into(),
            language: language.into(),
            to,
            license: String::new(),
            attribution: String::new(),
        }
    }

    /// The licence and credit the model's manifest declares.
    pub fn with_credit(
        mut self,
        license: impl Into<String>,
        attribution: impl Into<String>,
    ) -> Self {
        self.license = license.into();
        self.attribution = attribution.into();
        self
    }

    pub fn license(&self) -> &str {
        &self.license
    }

    pub fn attribution(&self) -> &str {
        &self.attribution
    }

    /// What the model does, as the daemon names tasks: `text.cleanup`.
    pub fn task(&self) -> &str {
        &self.task
    }

    /// The language it works on.
    pub fn language(&self) -> &str {
        &self.language
    }

    /// The language it translates into, for a translation model.
    pub fn to(&self) -> Option<&str> {
        self.to.as_deref()
    }
}

/// A text-model snap that Diagnostics describes: one that declares itself for
/// text, or any non-speech provider `myna:backend` is connected to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextOffer {
    identity: BackendIdentity,
    connected: bool,
    models: Vec<TextModel>,
}

impl TextOffer {
    pub fn new(identity: BackendIdentity, connected: bool, models: Vec<TextModel>) -> Self {
        Self {
            identity,
            connected,
            models,
        }
    }

    pub fn identity(&self) -> &BackendIdentity {
        &self.identity
    }

    pub fn connected(&self) -> bool {
        self.connected
    }

    /// The models its installed components provide.
    pub fn models(&self) -> &[TextModel] {
        &self.models
    }
}

impl ConnectionSnapshot {
    pub fn new(backends: Vec<BackendIdentity>, active_state: ActiveBackendState) -> Self {
        Self {
            backends,
            active_state,
            text_providers: Vec::new(),
            text_offers: Vec::new(),
            strays: Vec::new(),
        }
    }

    pub fn with_text_offers(mut self, offers: Vec<TextOffer>) -> Self {
        self.text_offers = offers;
        self
    }

    /// Reads the models each text offer's snap carries.
    pub fn reading_text_models(
        mut self,
        read: impl Fn(&BackendIdentity) -> Vec<TextModel>,
    ) -> Self {
        for offer in &mut self.text_offers {
            offer.models = read(&offer.identity);
        }
        self
    }

    /// The text-model snaps Diagnostics describes, connected or not.
    pub fn text_offers(&self) -> &[TextOffer] {
        &self.text_offers
    }

    pub fn with_companions(
        mut self,
        text_providers: Vec<BackendIdentity>,
        strays: Vec<BackendIdentity>,
    ) -> Self {
        self.text_providers = text_providers;
        self.strays = strays;
        self
    }

    /// Non-speech `inference-provider` slots connected to `myna:backend`
    /// (`MYNA_ROLE=text` snaps and third-party LLM providers), preserved across
    /// speech model switches.
    pub fn text_providers(&self) -> &[BackendIdentity] {
        &self.text_providers
    }

    /// Slots `myna:backend` is connected to whose content id is not
    /// [`PROVIDER_CONTENT_ID`] (stale connections disconnected on switch).
    pub fn strays(&self) -> &[BackendIdentity] {
        &self.strays
    }

    pub fn backends(&self) -> &[BackendIdentity] {
        &self.backends
    }

    pub fn active_state(&self) -> ActiveBackendState {
        self.active_state.clone()
    }
}

/// Content id of the slots Myna's `backend` plug can connect to.
pub const PROVIDER_CONTENT_ID: &str = "inference-provider";

/// The `task` slot attribute of a provider that transcribes speech. Other
/// inference snaps, LLMs such as gemma4, share [`PROVIDER_CONTENT_ID`].
pub const SPEECH_TO_TEXT_TASK: &str = "speech-to-text";

/// Myna's published speech snaps that are no [`ModelFamily`] Settings offers
/// to install. Their store revisions predate `MYNA_ROLE=speech`, so without
/// this a connected one passes for an LLM companion.
const OTHER_SPEECH_SNAPS: &[&str] = &["myna-nemotron"];

/// One content slot as `GET /v2/connections?select=all&interface=content`
/// lists it: its own attributes and the plugs connected to it.
#[derive(Clone, Debug, Deserialize)]
pub struct ContentSlot {
    snap: String,
    slot: String,
    #[serde(default)]
    attrs: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    connections: Vec<PlugReference>,
}

#[derive(Clone, Debug, Deserialize)]
struct PlugReference {
    snap: String,
    plug: String,
}

/// The `result` of that request. Its `established` and `plugs` lists say
/// nothing the slots do not.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ContentConnections {
    #[serde(default)]
    slots: Vec<ContentSlot>,
}

impl ContentConnections {
    pub fn slots(&self) -> &[ContentSlot] {
        &self.slots
    }
}

impl ContentSlot {
    pub fn snap_name(&self) -> &str {
        &self.snap
    }

    pub fn slot_name(&self) -> &str {
        &self.slot
    }

    pub fn is_inference_provider(&self) -> bool {
        self.attr_str("content") == Some(PROVIDER_CONTENT_ID)
    }

    pub fn share_source_dirs(&self) -> impl Iterator<Item = &str> {
        let list = |key: &str| {
            self.attrs
                .get("source")
                .and_then(|src| src.get(key))
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
        };
        list("write").chain(list("read"))
    }

    fn attr_str(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).and_then(serde_json::Value::as_str)
    }

    /// A [`PROVIDER_CONTENT_ID`] slot for speech: explicitly `MYNA_ROLE=speech`
    /// in `provider.env`, or without `MYNA_ROLE` a known Myna speech family or
    /// `task: speech-to-text` (carried by the unpublished fake backend).
    fn is_speech_model(&self, declared: Option<DeclaredRole>) -> bool {
        if !self.is_inference_provider() {
            return false;
        }
        match declared {
            Some(DeclaredRole::Speech) => true,
            Some(DeclaredRole::Text | DeclaredRole::Other) => false,
            None => {
                self.attr_str("task") == Some(SPEECH_TO_TEXT_TASK)
                    || ModelFamily::from_snap_name(&self.snap).is_some()
                    || OTHER_SPEECH_SNAPS.contains(&self.snap.as_str())
            }
        }
    }

    fn feeds_myna(&self) -> bool {
        self.connections
            .iter()
            .any(|plug| plug.snap == "myna" && plug.plug == "backend")
    }
}

/// The speech models on the machine, the ones `myna:backend` is connected to,
/// connected non-speech `inference-provider` companions, and stale strays.
///
/// A slot's content id is its own attribute, never the plug's: snapd keeps a
/// connection across a refresh that changed the plug's content id.
pub fn connection_snapshot(connections: &ContentConnections) -> ConnectionSnapshot {
    connection_snapshot_with_roles(connections, &BTreeMap::new())
}

pub fn connection_snapshot_with_roles(
    connections: &ContentConnections,
    roles: &BTreeMap<(String, String), DeclaredRole>,
) -> ConnectionSnapshot {
    let mut discovered = BTreeSet::new();
    let mut connected = BTreeSet::new();
    let mut text_providers = BTreeSet::new();
    let mut text_offers = BTreeSet::new();
    let mut strays = BTreeSet::new();
    for slot in &connections.slots {
        let backend = BackendIdentity::new(&slot.snap, &slot.slot);
        let declared = roles.get(&(slot.snap.clone(), slot.slot.clone())).copied();
        if !slot.is_inference_provider() {
            if slot.feeds_myna() {
                strays.insert(backend);
            }
            continue;
        }
        if slot.is_speech_model(declared) {
            if slot.feeds_myna() {
                connected.insert(backend.clone());
            }
            discovered.insert(backend);
        } else {
            if declared == Some(DeclaredRole::Text) || slot.feeds_myna() {
                text_offers.insert((backend.clone(), slot.feeds_myna()));
            }
            if slot.feeds_myna() {
                text_providers.insert(backend);
            }
        }
    }

    let connected: Vec<_> = connected.into_iter().collect();
    let active_state = match connected.as_slice() {
        [] => ActiveBackendState::Disconnected,
        [backend] => ActiveBackendState::Connected(backend.clone()),
        backends => ActiveBackendState::MultiplyConnected(backends.to_vec()),
    };
    ConnectionSnapshot {
        backends: discovered.into_iter().collect(),
        active_state,
        text_providers: text_providers.into_iter().collect(),
        text_offers: text_offers
            .into_iter()
            .map(|(backend, connected)| TextOffer::new(backend, connected, Vec::new()))
            .collect(),
        strays: strays.into_iter().collect(),
    }
}

pub fn parse_modelctl_config(input: &str) -> Result<BackendConfiguration, ParseError> {
    let mut output = BackendConfiguration::default();
    for (index, line) in input.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            return Err(ParseError::new(
                "modelctl get",
                format!("nested content on line {}", index + 1),
            ));
        }
        let (key, raw) = line.split_once(':').ok_or_else(|| {
            ParseError::new("modelctl get", format!("missing ':' on line {}", index + 1))
        })?;
        let key = key.trim();
        if key.is_empty() {
            return Err(ParseError::new(
                "modelctl get",
                format!("empty key on line {}", index + 1),
            ));
        }
        output
            .values
            .insert(key.to_owned(), parse_scalar(raw.trim()));
    }
    Ok(output)
}

fn parse_scalar(raw: &str) -> ConfigValue {
    match raw {
        "true" => ConfigValue::Boolean(true),
        "false" => ConfigValue::Boolean(false),
        _ => raw
            .parse::<i64>()
            .map(ConfigValue::Integer)
            .or_else(|_| raw.parse::<f64>().map(ConfigValue::Number))
            .unwrap_or_else(|_| ConfigValue::Text(raw.to_owned())),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceState {
    Active,
    Inactive,
    Failed,
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceHealth {
    name: String,
    state: ServiceState,
}

impl ServiceHealth {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_active(&self) -> bool {
        self.state == ServiceState::Active
    }

    pub fn state(&self) -> &ServiceState {
        &self.state
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackendStatus {
    services: Vec<ServiceHealth>,
}

impl BackendStatus {
    pub fn services(&self) -> &[ServiceHealth] {
        &self.services
    }
}

#[derive(Deserialize)]
struct RawStatus {
    engine: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    services: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "null_as_default")]
    entrypoints: BTreeMap<String, BTreeMap<String, String>>,
}

pub fn parse_status(input: &str) -> Result<BackendStatus, ParseError> {
    let raw: RawStatus =
        serde_json::from_str(input).map_err(|error| ParseError::json("modelctl status", error))?;
    let has_evidence = raw
        .engine
        .as_ref()
        .is_some_and(|engine| !engine.trim().is_empty())
        || !raw.services.is_empty()
        || !raw.entrypoints.is_empty();
    if !has_evidence {
        return Err(ParseError::new(
            "modelctl status",
            "missing engine, service, or entrypoint status evidence",
        ));
    }
    let services = raw
        .services
        .into_iter()
        .map(|(name, state)| ServiceHealth {
            name,
            state: match state.as_str() {
                "active" => ServiceState::Active,
                "inactive" => ServiceState::Inactive,
                "failed" => ServiceState::Failed,
                _ => ServiceState::Unknown(state),
            },
        })
        .collect();
    Ok(BackendStatus { services })
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelOption {
    name: String,
}

impl ModelOption {
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelOptions {
    active: Option<String>,
    options: Vec<ModelOption>,
}

impl ModelOptions {
    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }

    pub fn options(&self) -> &[ModelOption] {
        &self.options
    }
}

#[derive(Deserialize)]
struct RawModelOptions {
    #[serde(rename = "active-model")]
    active: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    models: Vec<RawModelOption>,
}

#[derive(Deserialize)]
struct RawModelOption {
    name: String,
}

pub fn parse_model_options(input: &str) -> Result<ModelOptions, ParseError> {
    let raw: RawModelOptions = serde_json::from_str(input)
        .map_err(|error| ParseError::json("modelctl list-models", error))?;
    Ok(ModelOptions {
        active: raw.active,
        options: raw
            .models
            .into_iter()
            .map(|model| ModelOption { name: model.name })
            .collect(),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct EngineOption {
    name: String,
    compatible: bool,
}

impl EngineOption {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn compatible(&self) -> bool {
        self.compatible
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineOptions {
    active: Option<String>,
    options: Vec<EngineOption>,
}

impl EngineOptions {
    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }

    pub fn options(&self) -> &[EngineOption] {
        &self.options
    }
}

#[derive(Deserialize)]
struct RawEngineOptions {
    #[serde(rename = "active-engine")]
    active: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    engines: Vec<RawEngineOption>,
}

#[derive(Deserialize)]
struct RawEngineOption {
    name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    compatible: bool,
}

/// modelctl writes `null` rather than `{}` or `[]` for anything empty, and
/// serde's `default` only covers a *missing* field.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

pub fn parse_engine_options(input: &str) -> Result<EngineOptions, ParseError> {
    let raw: RawEngineOptions = serde_json::from_str(input)
        .map_err(|error| ParseError::json("modelctl list-engines", error))?;
    Ok(EngineOptions {
        active: raw.active,
        options: raw
            .engines
            .into_iter()
            .map(|engine| EngineOption {
                name: engine.name,
                compatible: engine.compatible,
            })
            .collect(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BackendSurface {
    SnapInventory,
    Connections,
    ModelctlApp,
    ModelctlConfig,
    Status,
    Models,
    Engines,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendSurfaceError {
    surface: BackendSurface,
    message: String,
    stderr: String,
}

impl BackendSurfaceError {
    pub fn new(
        surface: BackendSurface,
        message: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        Self {
            surface,
            message: message.into(),
            stderr: stderr.into(),
        }
    }

    pub fn surface(&self) -> BackendSurface {
        self.surface
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn stderr(&self) -> &str {
        &self.stderr
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackendSnapshot {
    identity: BackendIdentity,
    configuration: BackendConfiguration,
    status: Option<BackendStatus>,
    models: Option<ModelOptions>,
    engines: Option<EngineOptions>,
    errors: BTreeMap<BackendSurface, BackendSurfaceError>,
}

impl BackendSnapshot {
    pub fn empty(identity: BackendIdentity) -> Self {
        Self {
            identity,
            configuration: BackendConfiguration::default(),
            status: None,
            models: None,
            engines: None,
            errors: BTreeMap::new(),
        }
    }

    pub fn identity(&self) -> &BackendIdentity {
        &self.identity
    }

    pub fn configuration(&self) -> &BackendConfiguration {
        &self.configuration
    }

    pub fn status(&self) -> Option<&BackendStatus> {
        self.status.as_ref()
    }

    pub fn models(&self) -> Option<&ModelOptions> {
        self.models.as_ref()
    }

    pub fn engines(&self) -> Option<&EngineOptions> {
        self.engines.as_ref()
    }

    pub fn errors(&self) -> &BTreeMap<BackendSurface, BackendSurfaceError> {
        &self.errors
    }

    pub fn error(&self, surface: BackendSurface) -> Option<&BackendSurfaceError> {
        self.errors.get(&surface)
    }

    /// The current value of a setting row, with the active model and engine
    /// standing in for the `model` and `engine` keys.
    pub fn value(&self, key: &str) -> Option<ConfigValue> {
        let selected = match key {
            "model" => self.models().and_then(ModelOptions::active),
            "engine" => self.engines().and_then(EngineOptions::active),
            _ => return self.configuration.get(key).cloned(),
        };
        selected.map(|value| ConfigValue::Text(value.to_owned()))
    }

    pub(crate) fn set_identity(&mut self, identity: BackendIdentity) {
        self.identity = identity;
    }

    pub(crate) fn set_modelctl_config(&mut self, configuration: BackendConfiguration) {
        self.configuration = configuration;
    }

    pub(crate) fn set_status(&mut self, status: BackendStatus) {
        self.status = Some(status);
    }

    pub(crate) fn set_models(&mut self, models: ModelOptions) {
        self.models = Some(models);
    }

    pub(crate) fn set_engines(&mut self, engines: EngineOptions) {
        self.engines = Some(engines);
    }

    pub(crate) fn add_error(&mut self, error: BackendSurfaceError) {
        self.errors.insert(error.surface(), error);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StagedChange {
    key: String,
    proposed: ConfigValue,
}

impl StagedChange {
    pub fn new(
        key: impl Into<String>,
        original: ConfigValue,
        proposed: ConfigValue,
    ) -> Result<Self, ValidationError> {
        let key = key.into();
        if key.trim().is_empty() {
            return Err(ValidationError::new("key", "configuration key is empty"));
        }
        if original == proposed {
            return Err(ValidationError::new(&key, "staged value is unchanged"));
        }
        Ok(Self { key, proposed })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn proposed(&self) -> &ConfigValue {
        &self.proposed
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    executable: String,
    arguments: Vec<String>,
    exit_status: Option<i32>,
    stdout: String,
    stderr: String,
}

impl CommandResult {
    pub fn new(
        executable: impl Into<String>,
        arguments: Vec<String>,
        exit_status: Option<i32>,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        Self {
            executable: executable.into(),
            arguments,
            exit_status,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    pub fn executable(&self) -> &str {
        &self.executable
    }

    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    pub fn exit_status(&self) -> Option<i32> {
        self.exit_status
    }

    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    pub fn stderr(&self) -> &str {
        &self.stderr
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    field: String,
    message: String,
}

impl ValidationError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ValidationError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    source: &'static str,
    message: String,
}

impl ParseError {
    fn new(source: &'static str, message: impl Into<String>) -> Self {
        Self {
            source,
            message: message.into(),
        }
    }

    fn json(source: &'static str, error: serde_json::Error) -> Self {
        Self::new(source, error.to_string())
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.source, self.message)
    }
}

impl std::error::Error for ParseError {}
