//! GTK-independent metadata used to render backend configuration.

use gettextrs::gettext;

use crate::domain::{BackendConfiguration, ConfigValue, EngineOptions, ModelOptions};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlType {
    Toggle,
    Number,
    Text,
    Choice,
    ReadOnly,
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
            Self::Any => gettext("value is invalid"),
            Self::Boolean => gettext("value must be true or false"),
            Self::Number => gettext("value must be a number"),
            Self::Text => gettext("value must be text"),
            Self::PositiveNumber => gettext("value must be a positive number"),
            Self::NonNegativeNumber => gettext("value must be zero or a positive number"),
            Self::NonNegativeInteger => gettext("value must be a non-negative whole number"),
            Self::Choices(choices) => {
                // TRANSLATORS: {choices} lists the allowed values, such as "cpu, cuda".
                let frame = gettext("value must be one of {choices}");
                frame.replace("{choices}", &choices.join(", "))
            }
            Self::ReadOnly => gettext("this value cannot be edited"),
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

#[derive(Clone, Debug, PartialEq)]
pub struct PresentationMetadata {
    title: String,
    explanation: String,
    control: ControlType,
    validation: Validation,
    group: PresentationGroup,
    sensitivity: Sensitivity,
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

    pub fn validation(&self) -> &Validation {
        &self.validation
    }

    pub fn group(&self) -> PresentationGroup {
        self.group
    }

    pub fn sensitivity(&self) -> Sensitivity {
        self.sensitivity
    }

    pub fn diagnostics_only(&self) -> bool {
        self.sensitivity != Sensitivity::UserFacing
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PresentationRow {
    key: String,
    value: ConfigValue,
    choices: Vec<String>,
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

    pub fn metadata(&self) -> &PresentationMetadata {
        &self.metadata
    }
}

pub fn metadata_for(key: &str, value: &ConfigValue) -> PresentationMetadata {
    match key {
        "model" => known(
            &gettext("Model"),
            &gettext("Which version of this model to run."),
            ControlType::Choice,
            Validation::Any,
            (PresentationGroup::General, Sensitivity::UserFacing, 0),
        ),
        "engine" => known(
            &gettext("Engine"),
            &gettext("The inference engine this model runs on."),
            ControlType::Choice,
            Validation::Any,
            (PresentationGroup::General, Sensitivity::UserFacing, 1),
        ),
        "streaming" => known(
            &gettext("Streaming output"),
            &gettext("Emit partial transcription results while speech is being processed."),
            ControlType::Toggle,
            Validation::Boolean,
            (PresentationGroup::General, Sensitivity::UserFacing, 10),
        ),
        "sleep-idle-seconds" => known(
            &gettext("Unload when idle (seconds)"),
            &gettext("Unload the inference service after this many idle seconds; zero disables the delay."),
            ControlType::Number,
            Validation::NonNegativeInteger,
            (PresentationGroup::Runtime, Sensitivity::UserFacing, 20),
        ),
        "verbose" => known(
            &gettext("Verbose logging"),
            &gettext("Write additional diagnostics to the system log."),
            ControlType::Toggle,
            Validation::Boolean,
            (PresentationGroup::Advanced, Sensitivity::Internal, 100),
        ),
        "ws.unix-socket" => known(
            &gettext("Socket path"),
            &gettext("Raw Unix socket path used by the transcription service."),
            ControlType::Text,
            Validation::Text,
            (PresentationGroup::Advanced, Sensitivity::Internal, 101),
        ),
        "stream-arm-seconds" => seconds(
            &gettext("Minimum speech before pause commit (seconds)"),
            &gettext("Speech required before a pause can commit a transcript chunk."),
            Validation::PositiveNumber,
            110,
        ),
        "stream-silence-cut-seconds" => seconds(
            &gettext("Pause length for commit (seconds)"),
            &gettext("Silence duration that commits the current transcript chunk."),
            Validation::PositiveNumber,
            111,
        ),
        "stream-force-cut-seconds" => seconds(
            &gettext("Maximum uncommitted audio (seconds)"),
            &gettext("Maximum speech window retained before a transcript chunk is forced to commit."),
            Validation::PositiveNumber,
            112,
        ),
        "stream-partial-cadence-seconds" => seconds(
            &gettext("Partial result interval (seconds)"),
            &gettext("Interval between unstable partial results; zero disables partial results."),
            Validation::NonNegativeNumber,
            113,
        ),
        "stream-partial-tail-seconds" => seconds(
            &gettext("Partial result audio window (seconds)"),
            &gettext("Recent uncommitted audio used for partial results; zero uses the whole window."),
            Validation::NonNegativeNumber,
            114,
        ),
        "compute-type" => known(
            &gettext("Compute type"),
            &gettext("Runtime numeric format override. Leave the model's own value unchanged unless required."),
            ControlType::Text,
            Validation::Text,
            (PresentationGroup::Advanced, Sensitivity::UserFacing, 120),
        ),
        "att-context-size" => known(
            &gettext("Attention context size"),
            &gettext("Model-specific latency and accuracy context. Empty uses the engine default."),
            ControlType::Text,
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
    validation: Validation,
    placement: (PresentationGroup, Sensitivity, u16),
) -> PresentationMetadata {
    let (group, sensitivity, order) = placement;
    PresentationMetadata {
        title: title.to_owned(),
        explanation: explanation.to_owned(),
        control,
        validation,
        group,
        sensitivity,
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
        ConfigValue::Null => (ControlType::ReadOnly, Validation::ReadOnly),
    };
    if sensitive {
        control = ControlType::ReadOnly;
        validation = Validation::ReadOnly;
    }
    PresentationMetadata {
        title: key.to_owned(),
        explanation: gettext("This model provides no description for this setting."),
        control,
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
                metadata,
            });
        }
    }

    rows.extend(configuration.iter().map(|(key, value)| PresentationRow {
        key: key.to_owned(),
        value: value.clone(),
        choices: Vec::new(),
        metadata: metadata_for(key, value),
    }));

    rows.sort_by(|left, right| {
        left.metadata
            .group
            .cmp(&right.metadata.group)
            .then_with(|| left.metadata.order.cmp(&right.metadata.order))
            .then_with(|| left.key.cmp(&right.key))
    });
    rows
}
