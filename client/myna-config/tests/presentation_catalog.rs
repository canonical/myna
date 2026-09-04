use myna_config::domain::{
    parse_engine_options, parse_model_options, parse_modelctl_config, ConfigValue,
};
use myna_config::presentation::{
    metadata_for, present_configuration, ControlType, PresentationGroup, RestartBehavior,
    Sensitivity, Units, Validation,
};

#[test]
fn every_shipped_backend_key_has_explicit_typed_metadata() {
    let cases = [
        (
            "model",
            ControlType::Choice,
            None,
            Validation::Any,
            PresentationGroup::General,
            RestartBehavior::Required,
        ),
        (
            "engine",
            ControlType::Choice,
            None,
            Validation::Any,
            PresentationGroup::General,
            RestartBehavior::Required,
        ),
        (
            "streaming",
            ControlType::Toggle,
            None,
            Validation::Boolean,
            PresentationGroup::General,
            RestartBehavior::Required,
        ),
        (
            "sleep-idle-seconds",
            ControlType::Number,
            Some(Units::Seconds),
            Validation::NonNegativeInteger,
            PresentationGroup::Runtime,
            RestartBehavior::NotRequired,
        ),
        (
            "verbose",
            ControlType::Toggle,
            None,
            Validation::Boolean,
            PresentationGroup::Advanced,
            RestartBehavior::Required,
        ),
        (
            "ws.unix-socket",
            ControlType::Text,
            None,
            Validation::Text,
            PresentationGroup::Advanced,
            RestartBehavior::Required,
        ),
        (
            "stream-arm-seconds",
            ControlType::Number,
            Some(Units::Seconds),
            Validation::PositiveNumber,
            PresentationGroup::Advanced,
            RestartBehavior::Required,
        ),
        (
            "stream-silence-cut-seconds",
            ControlType::Number,
            Some(Units::Seconds),
            Validation::PositiveNumber,
            PresentationGroup::Advanced,
            RestartBehavior::Required,
        ),
        (
            "stream-force-cut-seconds",
            ControlType::Number,
            Some(Units::Seconds),
            Validation::PositiveNumber,
            PresentationGroup::Advanced,
            RestartBehavior::Required,
        ),
        (
            "stream-partial-cadence-seconds",
            ControlType::Number,
            Some(Units::Seconds),
            Validation::NonNegativeNumber,
            PresentationGroup::Advanced,
            RestartBehavior::Required,
        ),
        (
            "stream-partial-tail-seconds",
            ControlType::Number,
            Some(Units::Seconds),
            Validation::NonNegativeNumber,
            PresentationGroup::Advanced,
            RestartBehavior::Required,
        ),
        (
            "compute-type",
            ControlType::Text,
            None,
            Validation::Text,
            PresentationGroup::Advanced,
            RestartBehavior::Required,
        ),
        (
            "att-context-size",
            ControlType::Text,
            None,
            Validation::Text,
            PresentationGroup::Advanced,
            RestartBehavior::Required,
        ),
    ];

    for (key, control, units, validation, group, restart_behavior) in cases {
        let value = match control {
            ControlType::Toggle => ConfigValue::Boolean(false),
            ControlType::Number => ConfigValue::Integer(0),
            _ => ConfigValue::Text(String::new()),
        };
        let metadata = metadata_for(key, &value);
        assert!(metadata.known(), "{key} must be in the explicit catalog");
        assert!(!metadata.title().is_empty(), "{key}");
        assert!(!metadata.explanation().is_empty(), "{key}");
        assert_eq!(metadata.control(), control, "{key}");
        assert_eq!(metadata.units(), units, "{key}");
        assert_eq!(metadata.validation(), &validation, "{key}");
        assert_eq!(metadata.group(), group, "{key}");
        assert_eq!(metadata.restart_behavior(), restart_behavior, "{key}");
    }
}

#[test]
fn validation_exactly_matches_current_configure_hooks() {
    let cases = [
        ("streaming", ConfigValue::Boolean(true), true),
        ("streaming", ConfigValue::Text("true".into()), false),
        ("stream-arm-seconds", ConfigValue::Number(0.1), true),
        ("stream-arm-seconds", ConfigValue::Integer(0), false),
        ("stream-arm-seconds", ConfigValue::Number(-0.1), false),
        ("stream-silence-cut-seconds", ConfigValue::Number(0.5), true),
        ("stream-force-cut-seconds", ConfigValue::Integer(60), true),
        (
            "stream-partial-cadence-seconds",
            ConfigValue::Integer(0),
            true,
        ),
        (
            "stream-partial-cadence-seconds",
            ConfigValue::Number(-0.1),
            false,
        ),
        (
            "stream-partial-tail-seconds",
            ConfigValue::Number(0.0),
            true,
        ),
        ("sleep-idle-seconds", ConfigValue::Integer(0), true),
        ("sleep-idle-seconds", ConfigValue::Number(0.5), false),
        ("sleep-idle-seconds", ConfigValue::Integer(-1), false),
        (
            "ws.unix-socket",
            ConfigValue::Text("/run/backend.sock".into()),
            true,
        ),
        ("ws.unix-socket", ConfigValue::Boolean(false), false),
        (
            "compute-type",
            ConfigValue::Text("backend-value".into()),
            true,
        ),
        ("compute-type", ConfigValue::Integer(1), false),
        ("att-context-size", ConfigValue::Text("70,0".into()), true),
        ("att-context-size", ConfigValue::List(Vec::new()), false),
    ];

    for (key, value, valid) in cases {
        assert_eq!(
            metadata_for(key, &value)
                .validation()
                .validate(&value)
                .is_ok(),
            valid,
            "{key} with {value:?}"
        );
    }
}

#[test]
fn choices_come_only_from_the_snapshot_and_rows_are_stably_ordered() {
    let configuration =
        parse_modelctl_config(include_str!("fixtures/modelctl-get-multi.txt")).unwrap();
    let models =
        parse_model_options(include_str!("fixtures/modelctl-list-models-multi.json")).unwrap();
    let engines =
        parse_engine_options(include_str!("fixtures/modelctl-list-engines-multi.json")).unwrap();

    let rows = present_configuration(&configuration, Some(&models), Some(&engines));
    let keys: Vec<_> = rows.iter().map(|row| row.key()).collect();
    assert_eq!(
        keys,
        [
            "model",
            "engine",
            "compute.precision",
            "nested.socket.path",
            "shared"
        ]
    );
    assert_eq!(rows[0].choices(), ["tiny", "base", "large"]);
    assert_eq!(rows[1].choices(), ["cpu", "gpu"]);
    assert_eq!(rows[0].value(), &ConfigValue::Text("large".into()));
    assert_eq!(rows[1].value(), &ConfigValue::Text("gpu".into()));
}

#[test]
fn unknown_scalars_and_nested_keys_remain_visible_with_honest_fallbacks() {
    let configuration = parse_modelctl_config(
        "future-bool: true\n\
         future-count: 3\n\
         future-ratio: 0.25\n\
         future-text: value\n\
         nested.tuning.mode: careful\n",
    )
    .unwrap();

    let rows = present_configuration(&configuration, None, None);
    assert_eq!(rows.len(), 5);
    let expected = [
        ("future-bool", ControlType::Toggle, Validation::Boolean),
        ("future-count", ControlType::Number, Validation::Number),
        ("future-ratio", ControlType::Number, Validation::Number),
        ("future-text", ControlType::Text, Validation::Text),
        ("nested.tuning.mode", ControlType::Text, Validation::Text),
    ];
    for (row, (key, control, validation)) in rows.iter().zip(expected) {
        assert_eq!(row.key(), key);
        assert!(!row.metadata().known());
        assert_eq!(row.metadata().group(), PresentationGroup::Advanced);
        assert_eq!(row.metadata().control(), control);
        assert_eq!(row.metadata().validation(), &validation);
        assert!(row
            .metadata()
            .explanation()
            .contains("does not provide presentation metadata"));
    }
}

#[test]
fn internal_and_sensitive_mechanisms_are_never_promoted() {
    for key in ["verbose", "ws.unix-socket"] {
        let metadata = metadata_for(key, &ConfigValue::Text(String::new()));
        assert_eq!(metadata.group(), PresentationGroup::Advanced);
        assert_eq!(metadata.sensitivity(), Sensitivity::Internal);
        assert!(metadata.diagnostics_only());
    }

    for key in [
        "api-token",
        "api-key",
        "private-key",
        "credentials.password",
        "client-secret",
        "apiToken",
        "apiKey",
        "clientSecret",
    ] {
        let metadata = metadata_for(key, &ConfigValue::Text("hidden".into()));
        assert_eq!(metadata.group(), PresentationGroup::Sensitive);
        assert_eq!(metadata.sensitivity(), Sensitivity::Sensitive);
        assert_eq!(metadata.control(), ControlType::ReadOnly);
        assert_eq!(metadata.validation(), &Validation::ReadOnly);
        assert!(metadata.diagnostics_only());
        assert!(!metadata.known());
    }
}

#[test]
fn unavailable_options_do_not_create_aspirational_selectors() {
    let configuration = parse_modelctl_config("verbose: false\n").unwrap();
    let empty_models = parse_model_options("{}").unwrap();
    let empty_engines = parse_engine_options("{}").unwrap();

    let rows = present_configuration(&configuration, Some(&empty_models), Some(&empty_engines));
    assert_eq!(
        rows.iter().map(|row| row.key()).collect::<Vec<_>>(),
        ["verbose"]
    );
    assert!(!rows.iter().any(|row| row.key().contains("microphone")));
}
