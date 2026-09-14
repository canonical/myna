use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

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
            Self::Integer(_) => None,
        }
    }

    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            Self::Choice(_) | Self::Text(_) => None,
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
    List(Vec<ConfigValue>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigScope {
    Engine,
    Package,
    User,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BackendConfiguration {
    values: BTreeMap<(ConfigScope, String), ConfigValue>,
}

impl BackendConfiguration {
    pub fn get(&self, scope: ConfigScope, key: &str) -> Option<&ConfigValue> {
        self.values.get(&(scope, key.to_owned()))
    }

    pub fn iter(&self) -> impl Iterator<Item = (ConfigScope, &str, &ConfigValue)> {
        self.values
            .iter()
            .map(|((scope, key), value)| (*scope, key.as_str(), value))
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Returns the value a backend will observe when scopes overlap.
    ///
    /// User overrides from modelctl are most specific, followed by engine and
    /// package configuration.
    pub fn effective(&self, key: &str) -> Option<&ConfigValue> {
        [ConfigScope::User, ConfigScope::Engine, ConfigScope::Package]
            .into_iter()
            .find_map(|scope| self.get(scope, key))
    }

    fn insert(&mut self, scope: ConfigScope, key: String, value: ConfigValue) {
        self.values.insert((scope, key), value);
    }

    pub(crate) fn merge(&mut self, other: BackendConfiguration) {
        self.values.extend(other.values);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwitchFailure {
    Disconnect { message: String },
    Connect { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActiveBackendState {
    Disconnected,
    Connected(BackendIdentity),
    MultiplyConnected(Vec<BackendIdentity>),
    FailedSwitch {
        previous: Option<BackendIdentity>,
        requested: BackendIdentity,
        observed: ConnectionState,
        failure: SwitchFailure,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connected(BackendIdentity),
    MultiplyConnected(Vec<BackendIdentity>),
}

impl ActiveBackendState {
    pub fn failed_switch(
        previous: Option<BackendIdentity>,
        requested: BackendIdentity,
        observed: ConnectionState,
        failure: SwitchFailure,
    ) -> Self {
        Self::FailedSwitch {
            previous,
            requested,
            observed,
            failure,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionSnapshot {
    backends: Vec<BackendIdentity>,
    active_state: ActiveBackendState,
}

impl ConnectionSnapshot {
    pub fn new(backends: Vec<BackendIdentity>, active_state: ActiveBackendState) -> Self {
        Self {
            backends,
            active_state,
        }
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

/// Builds the snapshot from `snap connections --all` and
/// `snap interface content --attrs`.
///
/// A slot's content id comes only from the interface listing. The
/// `content[<id>]` label on an established row is the plug's, and snapd keeps
/// a connection across a refresh that changed the plug's content id.
pub fn parse_connections(
    connections: &str,
    content_interface: &str,
) -> Result<ConnectionSnapshot, ParseError> {
    let mut lines = connections.lines();
    let header = lines.next().unwrap_or_default();
    if !header
        .split_whitespace()
        .eq(["Interface", "Plug", "Slot", "Notes"])
        && !header.split_whitespace().eq(["Interface", "Plug", "Slot"])
    {
        return Err(ParseError::new(
            "snap connections",
            "missing Interface/Plug/Slot header",
        ));
    }
    let providers = parse_provider_slots(content_interface)?;

    let mut discovered = BTreeSet::new();
    let mut connected = BTreeSet::new();
    for line in lines {
        let columns: Vec<_> = line.split_whitespace().collect();
        if columns.len() < 3 {
            continue;
        }
        let Some((snap_name, slot_name)) = columns[2].split_once(':') else {
            continue;
        };
        if snap_name.is_empty() || slot_name.is_empty() {
            continue;
        }
        let backend = BackendIdentity::new(snap_name, slot_name);
        if !providers.contains(&backend) {
            continue;
        }
        discovered.insert(backend.clone());
        if columns[1] == "myna:backend" {
            connected.insert(backend);
        }
    }

    let connected: Vec<_> = connected.into_iter().collect();
    let active_state = match connected.as_slice() {
        [] => ActiveBackendState::Disconnected,
        [backend] => ActiveBackendState::Connected(backend.clone()),
        backends => ActiveBackendState::MultiplyConnected(backends.to_vec()),
    };
    Ok(ConnectionSnapshot {
        backends: discovered.into_iter().collect(),
        active_state,
    })
}

/// Slots whose own `content` attribute is [`PROVIDER_CONTENT_ID`]. Only the
/// item lines of the `slots:` section and their direct attributes (six-space
/// indent) are read; nested attribute maps and lists are skipped.
fn parse_provider_slots(input: &str) -> Result<BTreeSet<BackendIdentity>, ParseError> {
    let mut lines = input.lines();
    let header = lines.next().unwrap_or_default();
    if !header
        .split_once(':')
        .is_some_and(|(key, name)| key == "name" && name.trim() == "content")
    {
        return Err(ParseError::new(
            "snap interface",
            "missing content interface name",
        ));
    }

    let mut providers = BTreeSet::new();
    let mut in_slots = false;
    let mut current: Option<BackendIdentity> = None;
    for line in lines {
        if !line.starts_with(' ') {
            in_slots = line == "slots:";
            current = None;
            continue;
        }
        if !in_slots {
            continue;
        }
        if let Some(item) = line.strip_prefix("  - ") {
            let reference = item.split_whitespace().next().unwrap_or_default();
            let reference = reference.strip_suffix(':').unwrap_or(reference);
            // snapd prints a slot named after the interface as the bare snap.
            let (snap_name, slot_name) =
                reference.split_once(':').unwrap_or((reference, "content"));
            current = Some(BackendIdentity::new(snap_name, slot_name));
            continue;
        }
        let Some(attribute) = line.strip_prefix("      ") else {
            continue;
        };
        if attribute.starts_with(' ') {
            continue;
        }
        if let (Some(slot), Some((key, value))) = (&current, attribute.split_once(':')) {
            if key == "content" && value.trim() == PROVIDER_CONTENT_ID {
                providers.insert(slot.clone());
            }
        }
    }
    Ok(providers)
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
        output.insert(ConfigScope::User, key.to_owned(), parse_scalar(raw.trim()));
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
    engine: Option<String>,
    services: Vec<ServiceHealth>,
    entrypoints: BTreeMap<String, BTreeMap<String, String>>,
}

impl BackendStatus {
    pub fn engine(&self) -> Option<&str> {
        self.engine.as_deref()
    }

    pub fn services(&self) -> &[ServiceHealth] {
        &self.services
    }

    pub fn entrypoints(&self) -> &BTreeMap<String, BTreeMap<String, String>> {
        &self.entrypoints
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
    Ok(BackendStatus {
        engine: raw.engine,
        services,
        entrypoints: raw.entrypoints,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelOption {
    name: String,
    description: Option<String>,
    model_card_url: Option<String>,
    quantization: Option<String>,
    disk_size: Option<String>,
    components: Vec<String>,
}

impl ModelOption {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn model_card_url(&self) -> Option<&str> {
        self.model_card_url.as_deref()
    }

    pub fn quantization(&self) -> Option<&str> {
        self.quantization.as_deref()
    }

    pub fn disk_size(&self) -> Option<&str> {
        self.disk_size.as_deref()
    }

    pub fn components(&self) -> &[String] {
        &self.components
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
    description: Option<String>,
    #[serde(rename = "model-card-url")]
    model_card_url: Option<String>,
    quantization: Option<String>,
    #[serde(rename = "disk-size")]
    disk_size: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    components: Vec<String>,
}

pub fn parse_model_options(input: &str) -> Result<ModelOptions, ParseError> {
    let raw: RawModelOptions = serde_json::from_str(input)
        .map_err(|error| ParseError::json("modelctl list-models", error))?;
    Ok(ModelOptions {
        active: raw.active,
        options: raw
            .models
            .into_iter()
            .map(|model| ModelOption {
                name: model.name,
                description: model.description,
                model_card_url: model.model_card_url,
                quantization: model.quantization,
                disk_size: model.disk_size,
                components: model.components,
            })
            .collect(),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct EngineOption {
    name: String,
    summary: Option<String>,
    description: Option<String>,
    vendor: Option<String>,
    runtime: Option<String>,
    compatible: bool,
    score: Option<i64>,
    model_default: Option<String>,
    model_options: Vec<String>,
    configuration: BTreeMap<String, ConfigValue>,
}

impl EngineOption {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn compatible(&self) -> bool {
        self.compatible
    }

    pub fn configuration(&self) -> &BTreeMap<String, ConfigValue> {
        &self.configuration
    }

    pub fn model_default(&self) -> Option<&str> {
        self.model_default.as_deref()
    }

    pub fn model_options(&self) -> &[String] {
        &self.model_options
    }

    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn vendor(&self) -> Option<&str> {
        self.vendor.as_deref()
    }

    pub fn runtime(&self) -> Option<&str> {
        self.runtime.as_deref()
    }

    pub fn score(&self) -> Option<i64> {
        self.score
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
    summary: Option<String>,
    description: Option<String>,
    vendor: Option<String>,
    runtime: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    compatible: bool,
    score: Option<i64>,
    model: Option<RawEngineModels>,
    #[serde(default, deserialize_with = "null_as_default")]
    configurations: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct RawEngineModels {
    default: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    options: Vec<String>,
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
    let options = raw
        .engines
        .into_iter()
        .map(|engine| {
            let configuration = engine
                .configurations
                .iter()
                .map(|(key, value)| Ok((key.clone(), config_value(value)?)))
                .collect::<Result<_, ParseError>>()?;
            let (model_default, model_options) = engine
                .model
                .map(|model| (model.default, model.options))
                .unwrap_or_default();
            Ok(EngineOption {
                name: engine.name,
                summary: engine.summary,
                description: engine.description,
                vendor: engine.vendor,
                runtime: engine.runtime,
                compatible: engine.compatible,
                score: engine.score,
                model_default,
                model_options,
                configuration,
            })
        })
        .collect::<Result<_, ParseError>>()?;
    Ok(EngineOptions {
        active: raw.active,
        options,
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
    executable: String,
    arguments: Vec<String>,
    message: String,
    stderr: String,
}

impl BackendSurfaceError {
    pub fn new(
        surface: BackendSurface,
        executable: impl Into<String>,
        arguments: Vec<String>,
        message: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        Self {
            surface,
            executable: executable.into(),
            arguments,
            message: message.into(),
            stderr: stderr.into(),
        }
    }

    pub fn surface(&self) -> BackendSurface {
        self.surface
    }

    pub fn executable(&self) -> &str {
        &self.executable
    }

    pub fn arguments(&self) -> &[String] {
        &self.arguments
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
    pub fn new(identity: BackendIdentity) -> Self {
        Self::empty(identity)
    }

    pub(crate) fn empty(identity: BackendIdentity) -> Self {
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

    pub(crate) fn set_identity(&mut self, identity: BackendIdentity) {
        self.identity = identity;
    }

    pub(crate) fn set_modelctl_config(&mut self, configuration: BackendConfiguration) {
        self.configuration.merge(configuration);
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

fn config_value(value: &serde_json::Value) -> Result<ConfigValue, ParseError> {
    match value {
        serde_json::Value::Null => Ok(ConfigValue::Null),
        serde_json::Value::Bool(value) => Ok(ConfigValue::Boolean(*value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(ConfigValue::Integer(value))
            } else {
                value
                    .as_f64()
                    .map(ConfigValue::Number)
                    .ok_or_else(|| ParseError::new("configuration", "number is out of range"))
            }
        }
        serde_json::Value::String(value) => Ok(ConfigValue::Text(value.clone())),
        serde_json::Value::Array(values) => values
            .iter()
            .map(config_value)
            .collect::<Result<_, _>>()
            .map(ConfigValue::List),
        serde_json::Value::Object(_) => Err(ParseError::new(
            "configuration",
            "nested objects must be flattened before conversion",
        )),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StagedChange {
    scope: ConfigScope,
    key: String,
    original: ConfigValue,
    proposed: ConfigValue,
    restart_required: bool,
}

impl StagedChange {
    pub fn new(
        scope: ConfigScope,
        key: impl Into<String>,
        original: ConfigValue,
        proposed: ConfigValue,
        restart_required: bool,
    ) -> Result<Self, ValidationError> {
        let key = key.into();
        if key.trim().is_empty() {
            return Err(ValidationError::new("key", "configuration key is empty"));
        }
        if original == proposed {
            return Err(ValidationError::new(&key, "staged value is unchanged"));
        }
        Ok(Self {
            scope,
            key,
            original,
            proposed,
            restart_required,
        })
    }

    pub fn scope(&self) -> ConfigScope {
        self.scope
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn original(&self) -> &ConfigValue {
        &self.original
    }

    pub fn proposed(&self) -> &ConfigValue {
        &self.proposed
    }

    pub fn restart_required(&self) -> bool {
        self.restart_required
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

    pub fn field(&self) -> &str {
        &self.field
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

    pub fn source_name(&self) -> &str {
        self.source
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.source, self.message)
    }
}

impl std::error::Error for ParseError {}
