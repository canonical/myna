//! GTK-independent metadata used to render backend configuration.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::{BackendConfiguration, ConfigScope, ConfigValue, EngineOptions, ModelOptions};

const METADATA_UNAVAILABLE: &str =
    "This backend does not provide presentation metadata for this setting.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlType {
    Toggle,
    Number,
    Text,
    Choice,
    ReadOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Units {
    Seconds,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Validation {
    Any,
    Boolean,
    Number,
    Text,
    PositiveNumber,
    NonNegativeNumber,
    NonNegativeInteger,
    Choices(Vec<String>),
    ReadOnly,
}

impl Validation {
    pub fn validate(&self, value: &ConfigValue) -> Result<(), String> {
        let valid = match self {
            Self::Any => true,
            Self::Boolean => matches!(value, ConfigValue::Boolean(_)),
            Self::Number => numeric_value(value).is_some(),
            Self::Text => matches!(value, ConfigValue::Text(_)),
            Self::PositiveNumber => numeric_value(value).is_some_and(|number| number > 0.0),
            Self::NonNegativeNumber => numeric_value(value).is_some_and(|number| number >= 0.0),
            Self::NonNegativeInteger => {
                matches!(value, ConfigValue::Integer(number) if *number >= 0)
            }
            Self::Choices(choices) => {
                matches!(value, ConfigValue::Text(choice) if choices.contains(choice))
            }
            Self::ReadOnly => false,
        };
        valid.then_some(()).ok_or_else(|| match self {
            Self::Any => "value is invalid".to_owned(),
            Self::Boolean => "value must be true or false".to_owned(),
            Self::Number => "value must be a number".to_owned(),
            Self::Text => "value must be text".to_owned(),
            Self::PositiveNumber => "value must be a positive number".to_owned(),
            Self::NonNegativeNumber => "value must be zero or a positive number".to_owned(),
            Self::NonNegativeInteger => "value must be a non-negative whole number".to_owned(),
            Self::Choices(choices) => format!("value must be one of {}", choices.join(", ")),
            Self::ReadOnly => "this value cannot be edited".to_owned(),
        })
    }
}

fn numeric_value(value: &ConfigValue) -> Option<f64> {
    match value {
        ConfigValue::Integer(value) => Some(*value as f64),
        ConfigValue::Number(value) if value.is_finite() => Some(*value),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PresentationGroup {
    General,
    Runtime,
    Advanced,
    Sensitive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sensitivity {
    UserFacing,
    Internal,
    Sensitive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartBehavior {
    NotRequired,
    Required,
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PresentationMetadata {
    title: String,
    explanation: String,
    control: ControlType,
    units: Option<Units>,
    validation: Validation,
    group: PresentationGroup,
    sensitivity: Sensitivity,
    restart_behavior: RestartBehavior,
    known: bool,
    order: u16,
}

impl PresentationMetadata {
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn explanation(&self) -> &str {
        &self.explanation
    }

    pub fn control(&self) -> ControlType {
        self.control
    }

    pub fn units(&self) -> Option<Units> {
        self.units
    }

    pub fn validation(&self) -> &Validation {
        &self.validation
    }

    pub fn group(&self) -> PresentationGroup {
        self.group
    }

    pub fn sensitivity(&self) -> Sensitivity {
        self.sensitivity
    }

    pub fn restart_behavior(&self) -> RestartBehavior {
        self.restart_behavior
    }

    pub fn known(&self) -> bool {
        self.known
    }

    pub fn diagnostics_only(&self) -> bool {
        self.sensitivity != Sensitivity::UserFacing
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentationSource {
    Model,
    Engine,
    Configuration(ConfigScope),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PresentationRow {
    key: String,
    value: ConfigValue,
    choices: Vec<String>,
    source: PresentationSource,
    metadata: PresentationMetadata,
}

impl PresentationRow {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn value(&self) -> &ConfigValue {
        &self.value
    }

    pub fn choices(&self) -> &[String] {
        &self.choices
    }

    pub fn source(&self) -> PresentationSource {
        self.source
    }

    pub fn metadata(&self) -> &PresentationMetadata {
        &self.metadata
    }
}

pub fn metadata_for(key: &str, value: &ConfigValue) -> PresentationMetadata {
    match key {
        "model" => known(
            "Model",
            "The model served by this backend.",
            ControlType::Choice,
            None,
            Validation::Any,
            (PresentationGroup::General, Sensitivity::UserFacing, 0),
        ),
        "engine" => known(
            "Engine",
            "The inference engine used by this backend.",
            ControlType::Choice,
            None,
            Validation::Any,
            (PresentationGroup::General, Sensitivity::UserFacing, 1),
        ),
        "streaming" => known(
            "Streaming output",
            "Emit partial transcription results while speech is being processed.",
            ControlType::Toggle,
            None,
            Validation::Boolean,
            (PresentationGroup::General, Sensitivity::UserFacing, 10),
        ),
        "sleep-idle-seconds" => known_with_restart(
            "Unload when idle",
            "Unload the inference service after this many idle seconds; zero disables the delay.",
            ControlType::Number,
            Some(Units::Seconds),
            Validation::NonNegativeInteger,
            (PresentationGroup::Runtime, Sensitivity::UserFacing, 20),
            RestartBehavior::NotRequired,
        ),
        "verbose" => known(
            "Verbose logging",
            "Write additional backend diagnostics to the system log.",
            ControlType::Toggle,
            None,
            Validation::Boolean,
            (PresentationGroup::Advanced, Sensitivity::Internal, 100),
        ),
        "ws.unix-socket" => known(
            "Socket path",
            "Raw Unix socket path used by the transcription service.",
            ControlType::Text,
            None,
            Validation::Text,
            (PresentationGroup::Advanced, Sensitivity::Internal, 101),
        ),
        "stream-arm-seconds" => seconds(
            "Minimum speech before pause commit",
            "Speech required before a pause can commit a transcript chunk.",
            Validation::PositiveNumber,
            110,
        ),
        "stream-silence-cut-seconds" => seconds(
            "Pause length for commit",
            "Silence duration that commits the current transcript chunk.",
            Validation::PositiveNumber,
            111,
        ),
        "stream-force-cut-seconds" => seconds(
            "Maximum uncommitted audio",
            "Maximum speech window retained before a transcript chunk is forced to commit.",
            Validation::PositiveNumber,
            112,
        ),
        "stream-partial-cadence-seconds" => seconds(
            "Partial result interval",
            "Interval between unstable partial results; zero disables partial results.",
            Validation::NonNegativeNumber,
            113,
        ),
        "stream-partial-tail-seconds" => seconds(
            "Partial result audio window",
            "Recent uncommitted audio used for partial results; zero uses the whole window.",
            Validation::NonNegativeNumber,
            114,
        ),
        "compute-type" => known(
            "Compute type",
            "Runtime numeric format override. Leave the backend-provided value unchanged unless required.",
            ControlType::Text,
            None,
            Validation::Text,
            (PresentationGroup::Advanced, Sensitivity::UserFacing, 120),
        ),
        "att-context-size" => known(
            "Attention context size",
            "Backend-specific latency and accuracy context. Empty uses the engine default.",
            ControlType::Text,
            None,
            Validation::Text,
            (PresentationGroup::Advanced, Sensitivity::UserFacing, 121),
        ),
        _ => fallback(key, value),
    }
}

fn known(
    title: &str,
    explanation: &str,
    control: ControlType,
    units: Option<Units>,
    validation: Validation,
    placement: (PresentationGroup, Sensitivity, u16),
) -> PresentationMetadata {
    known_with_restart(
        title,
        explanation,
        control,
        units,
        validation,
        placement,
        RestartBehavior::Required,
    )
}

fn known_with_restart(
    title: &str,
    explanation: &str,
    control: ControlType,
    units: Option<Units>,
    validation: Validation,
    placement: (PresentationGroup, Sensitivity, u16),
    restart_behavior: RestartBehavior,
) -> PresentationMetadata {
    let (group, sensitivity, order) = placement;
    PresentationMetadata {
        title: title.to_owned(),
        explanation: explanation.to_owned(),
        control,
        units,
        validation,
        group,
        sensitivity,
        restart_behavior,
        known: true,
        order,
    }
}

fn seconds(
    title: &str,
    explanation: &str,
    validation: Validation,
    order: u16,
) -> PresentationMetadata {
    known(
        title,
        explanation,
        ControlType::Number,
        Some(Units::Seconds),
        validation,
        (PresentationGroup::Advanced, Sensitivity::UserFacing, order),
    )
}

fn fallback(key: &str, value: &ConfigValue) -> PresentationMetadata {
    let sensitive = is_sensitive_key(key);
    let (mut control, mut validation) = match value {
        ConfigValue::Boolean(_) => (ControlType::Toggle, Validation::Boolean),
        ConfigValue::Integer(_) | ConfigValue::Number(_) => {
            (ControlType::Number, Validation::Number)
        }
        ConfigValue::Text(_) => (ControlType::Text, Validation::Text),
        ConfigValue::Null | ConfigValue::List(_) => (ControlType::ReadOnly, Validation::ReadOnly),
    };
    if sensitive {
        control = ControlType::ReadOnly;
        validation = Validation::ReadOnly;
    }
    PresentationMetadata {
        title: key.to_owned(),
        explanation: METADATA_UNAVAILABLE.to_owned(),
        control,
        units: None,
        validation,
        group: if sensitive {
            PresentationGroup::Sensitive
        } else {
            PresentationGroup::Advanced
        },
        sensitivity: if sensitive {
            Sensitivity::Sensitive
        } else {
            Sensitivity::UserFacing
        },
        restart_behavior: RestartBehavior::Unknown,
        known: false,
        order: u16::MAX,
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let mut normalized = String::with_capacity(key.len() * 2);
    for (index, character) in key.chars().enumerate() {
        if index > 0 && character.is_ascii_uppercase() {
            normalized.push('.');
        }
        normalized.push(character.to_ascii_lowercase());
    }
    let segments: Vec<_> = normalized.split(['.', '-', '_']).collect();
    segments.iter().any(|segment| {
        matches!(
            *segment,
            "credential" | "credentials" | "password" | "secret" | "token"
        )
    }) || segments.windows(2).any(|pair| {
        pair[1] == "key"
            && matches!(
                pair[0],
                "access" | "api" | "encryption" | "private" | "signing" | "ssh"
            )
    })
}

pub fn present_configuration(
    configuration: &BackendConfiguration,
    models: Option<&ModelOptions>,
    engines: Option<&EngineOptions>,
) -> Vec<PresentationRow> {
    let mut rows = Vec::new();

    if let Some(models) = models {
        let choices: Vec<_> = models
            .options()
            .iter()
            .map(|model| model.name().to_owned())
            .collect();
        if !choices.is_empty() {
            let mut metadata = metadata_for("model", &ConfigValue::Null);
            metadata.validation = Validation::Choices(choices.clone());
            rows.push(PresentationRow {
                key: "model".to_owned(),
                value: models.active().map_or(ConfigValue::Null, |active| {
                    ConfigValue::Text(active.to_owned())
                }),
                choices,
                source: PresentationSource::Model,
                metadata,
            });
        }
    }

    if let Some(engines) = engines {
        let choices: Vec<_> = engines
            .options()
            .iter()
            .filter(|engine| engine.compatible() || Some(engine.name()) == engines.active())
            .map(|engine| engine.name().to_owned())
            .collect();
        if !choices.is_empty() {
            let mut metadata = metadata_for("engine", &ConfigValue::Null);
            metadata.validation = Validation::Choices(choices.clone());
            rows.push(PresentationRow {
                key: "engine".to_owned(),
                value: engines.active().map_or(ConfigValue::Null, |active| {
                    ConfigValue::Text(active.to_owned())
                }),
                choices,
                source: PresentationSource::Engine,
                metadata,
            });
        }
    }

    let mut effective = BTreeMap::new();
    let keys: BTreeSet<_> = configuration.iter().map(|(_, key, _)| key).collect();
    for key in keys {
        for scope in [ConfigScope::User, ConfigScope::Engine, ConfigScope::Package] {
            if let Some(value) = configuration.get(scope, key) {
                effective.insert(key, (scope, value));
                break;
            }
        }
    }
    rows.extend(
        effective
            .into_iter()
            .map(|(key, (scope, value))| PresentationRow {
                key: key.to_owned(),
                value: value.clone(),
                choices: Vec::new(),
                source: PresentationSource::Configuration(scope),
                metadata: metadata_for(key, value),
            }),
    );

    rows.sort_by(|left, right| {
        left.metadata
            .group
            .cmp(&right.metadata.group)
            .then_with(|| left.metadata.order.cmp(&right.metadata.order))
            .then_with(|| left.key.cmp(&right.key))
    });
    rows
}
