use std::path::{Path, PathBuf};

use gio::glib::prelude::ObjectExt;
use gio::glib::{self, variant::ToVariant};
use gio::prelude::{SettingsExt, SettingsExtManual};

use crate::domain::{
    ClientSetting, ClientSettingKey, ClientSettingMetadata, ClientSettingValue, SettingRange,
};
use crate::ports::{
    ClientSettings, ClientSettingsCallback, ClientSettingsError, ClientSettingsSubscription,
};
use myna_core::processing::{
    chain_of, script_of, with_chain, with_script, ChineseScript, Params, ProcessingChain,
};
use myna_core::settings::{KEY_PROCESSING_PARAMS, KEY_PROCESSING_STEPS, SCHEMA_ID};

/// What Myna types: the preset the stored steps read as. Not a schema key;
/// this adapter presents it over `processing-steps` and `processing-params`.
pub const KEY_PROCESSING_CHAIN: &str = "processing-chain";

/// How Chinese is written: `script.mode`, with the `script` step switched on
/// for any script but the recognised one. Presented like the chain.
pub const KEY_CHINESE_SCRIPT: &str = "chinese-script";

/// The keys the page presents in place of the stored step list.
const STORED_KEYS: [&str; 2] = [KEY_PROCESSING_STEPS, KEY_PROCESSING_PARAMS];

fn is_view(key: &str) -> bool {
    key == KEY_PROCESSING_CHAIN || key == KEY_CHINESE_SCRIPT
}

/// The stored steps and parameters, as the daemon reads them.
fn stored(settings: &gio::Settings) -> (Vec<String>, Params) {
    let ids = settings
        .strv(KEY_PROCESSING_STEPS)
        .iter()
        .map(|id| id.to_string())
        .collect();
    let params = settings
        .value(KEY_PROCESSING_PARAMS)
        .get::<Params>()
        .unwrap_or_default();
    (ids, params)
}

fn view_value(settings: &gio::Settings, key: &str) -> ClientSettingValue {
    let (ids, params) = stored(settings);
    ClientSettingValue::Choice(match key {
        KEY_PROCESSING_CHAIN => chain_of(&ids, &params).nick().to_owned(),
        _ => script_of(&ids, &params).nick().to_owned(),
    })
}

#[derive(Debug)]
pub struct GioClientSettings {
    schema: gio::SettingsSchema,
    settings: gio::Settings,
}

impl GioClientSettings {
    pub fn open() -> Result<Self, ClientSettingsError> {
        let source = gio::SettingsSchemaSource::default().ok_or_else(schema_unavailable)?;
        Self::open_with_source(&source, private_keyfile_path()?)
    }

    pub fn open_with_source(
        source: &gio::SettingsSchemaSource,
        keyfile: impl AsRef<Path>,
    ) -> Result<Self, ClientSettingsError> {
        let keyfile = keyfile.as_ref();
        if let Some(parent) = keyfile.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                ClientSettingsError::StoreUnavailable {
                    message: format!("cannot create {}: {error}", parent.display()),
                }
            })?;
        }
        let path = keyfile
            .to_str()
            .ok_or_else(|| ClientSettingsError::StoreUnavailable {
                message: format!("{} is not valid UTF-8", keyfile.display()),
            })?;
        let backend = gio::functions::keyfile_settings_backend_new(path, "/", None);
        Self::open_with_backend(source, &backend)
    }

    pub fn open_with_backend(
        source: &gio::SettingsSchemaSource,
        backend: &gio::SettingsBackend,
    ) -> Result<Self, ClientSettingsError> {
        let schema = source
            .lookup(SCHEMA_ID, true)
            .ok_or_else(schema_unavailable)?;
        let settings = gio::Settings::new_full(&schema, Some(backend), None);
        Ok(Self { schema, settings })
    }

    /// Whether the schema in use holds the stored step keys; one from before
    /// them has no chain or script to present.
    fn has_views(&self) -> bool {
        STORED_KEYS.iter().all(|key| self.schema.has_key(key))
    }

    fn view_metadata(&self, key: &str) -> ClientSettingMetadata {
        let (summary, description, choices, default) = if key == KEY_PROCESSING_CHAIN {
            (
                gettextrs::gettext("What Myna types"),
                gettextrs::gettext(
                    "Exactly what you say, typed as you speak; a tidied version, without the hesitations and repeats; a French translation of it; or what you say fitted to the field you type in. Anything but the first is shown on screen while you speak and typed a sentence at a time.",
                ),
                ProcessingChain::ALL.map(|chain| chain.nick().to_owned()).to_vec(),
                ProcessingChain::EXACT.nick(),
            )
        } else {
            (
                gettextrs::gettext("Chinese writing"),
                gettextrs::gettext(
                    "Speech models write Chinese in whichever script they like. Choose simplified or traditional characters, with the words used in Taiwan or the characters used in Hong Kong, and what you say is typed that way. Other languages are left as they are.",
                ),
                ChineseScript::ALL.map(|script| script.nick().to_owned()).to_vec(),
                ChineseScript::AsRecognised.nick(),
            )
        };
        ClientSettingMetadata::new(
            ClientSettingKey::new(key).expect("view keys are non-empty"),
            Some(summary),
            Some(description),
            ClientSettingValue::Choice(default.to_owned()),
            SettingRange::Choices(choices),
            view_value(&self.settings, key),
            STORED_KEYS.iter().all(|key| self.settings.is_writable(key)),
        )
    }

    fn set_view(&self, key: &str, value: ClientSettingValue) -> Result<(), ClientSettingsError> {
        let invalid = |message: &str| ClientSettingsError::InvalidValue {
            key: key.to_owned(),
            message: message.into(),
        };
        for stored_key in STORED_KEYS {
            self.ensure_writable(stored_key)?;
        }
        let ClientSettingValue::Choice(nick) = value else {
            return Err(invalid("value has the wrong schema type"));
        };
        let (ids, params) = stored(&self.settings);
        let (next_ids, next_params) = if key == KEY_PROCESSING_CHAIN {
            let chain = ProcessingChain::from_nick(&nick)
                .ok_or_else(|| invalid("value is outside the schema range"))?;
            with_chain(&ids, &params, chain)
        } else {
            let script = ChineseScript::from_nick(&nick)
                .ok_or_else(|| invalid("value is outside the schema range"))?;
            with_script(&ids, &params, script)
        };
        let write = |stored_key: &str, variant: glib::Variant| {
            self.settings
                .set_value(stored_key, &variant)
                .map_err(|error| ClientSettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: error.to_string(),
                })
        };
        // Parameters first: a daemon that reads between the two writes sees
        // a parameter no step uses yet, never a step without its parameter.
        if next_params != params {
            write(KEY_PROCESSING_PARAMS, next_params.to_variant())?;
        }
        if next_ids != ids {
            write(KEY_PROCESSING_STEPS, next_ids.to_variant())?;
        }
        Ok(())
    }

    fn schema_key(&self, key: &str) -> Result<gio::SettingsSchemaKey, ClientSettingsError> {
        if !self.schema.has_key(key) {
            return Err(ClientSettingsError::UnknownKey {
                key: key.to_owned(),
            });
        }
        Ok(self.schema.key(key))
    }

    fn metadata(&self, key: &str) -> Result<ClientSettingMetadata, ClientSettingsError> {
        let schema_key = self.schema_key(key)?;
        let range = setting_range(&schema_key);
        Ok(ClientSettingMetadata::new(
            ClientSettingKey::new(key).expect("schema keys are non-empty"),
            schema_key.summary().map(Into::into),
            schema_key.description().map(Into::into),
            value_from_variant(&schema_key.default_value(), &range, key)?,
            range.clone(),
            value_from_variant(&self.settings.value(key), &range, key)?,
            self.settings.is_writable(key),
        ))
    }

    fn ensure_writable(&self, key: &str) -> Result<(), ClientSettingsError> {
        if self.settings.is_writable(key) {
            Ok(())
        } else {
            Err(ClientSettingsError::NotWritable {
                key: key.to_owned(),
            })
        }
    }
}

impl ClientSettings for GioClientSettings {
    fn list(&self) -> Result<Vec<ClientSettingMetadata>, ClientSettingsError> {
        let mut keys = self.schema.list_keys();
        keys.retain(|key| {
            holds(&self.schema.key(key).value_type()) && !STORED_KEYS.contains(&key.as_str())
        });
        if self.has_views() {
            keys.extend([KEY_PROCESSING_CHAIN.into(), KEY_CHINESE_SCRIPT.into()]);
        }
        keys.sort();
        keys.iter()
            .map(|key| match is_view(key) {
                true => Ok(self.view_metadata(key)),
                false => self.metadata(key),
            })
            .collect()
    }

    fn get(&self, key: &str) -> Result<ClientSettingValue, ClientSettingsError> {
        if is_view(key) && self.has_views() {
            return Ok(view_value(&self.settings, key));
        }
        let schema_key = self.schema_key(key)?;
        value_from_variant(&self.settings.value(key), &setting_range(&schema_key), key)
    }

    fn set(&self, key: &str, value: ClientSettingValue) -> Result<(), ClientSettingsError> {
        if is_view(key) && self.has_views() {
            return self.set_view(key, value);
        }
        let schema_key = self.schema_key(key)?;
        self.ensure_writable(key)?;
        let range = setting_range(&schema_key);
        let variant = match (&range, &value) {
            (SettingRange::Choices(_), ClientSettingValue::Choice(value))
            | (SettingRange::Unrestricted, ClientSettingValue::Text(value)) => value.to_variant(),
            (_, ClientSettingValue::Boolean(value)) => value.to_variant(),
            (SettingRange::Unrestricted, ClientSettingValue::List(values)) => values.to_variant(),
            (
                SettingRange::Range { .. } | SettingRange::Unrestricted,
                ClientSettingValue::Integer(value),
            ) => integer_variant(*value, &schema_key.value_type()).ok_or_else(|| {
                ClientSettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: "value is outside the schema range".into(),
                }
            })?,
            _ => {
                return Err(ClientSettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: "value has the wrong schema type".into(),
                });
            }
        };
        if variant.type_() != schema_key.value_type() || !schema_key.range_check(&variant) {
            return Err(ClientSettingsError::InvalidValue {
                key: key.to_owned(),
                message: "value is outside the schema range".into(),
            });
        }
        self.settings.set_value(key, &variant).map_err(|error| {
            ClientSettingsError::InvalidValue {
                key: key.to_owned(),
                message: error.to_string(),
            }
        })?;
        Ok(())
    }

    fn has_user_value(&self, key: &str) -> Result<bool, ClientSettingsError> {
        if is_view(key) && self.has_views() {
            return Ok(STORED_KEYS
                .iter()
                .any(|stored_key| self.settings.user_value(stored_key).is_some()));
        }
        self.schema_key(key)?;
        Ok(self.settings.user_value(key).is_some())
    }

    fn subscribe(
        &self,
        callback: ClientSettingsCallback,
    ) -> Result<Box<dyn ClientSettingsSubscription>, ClientSettingsError> {
        let settings = self.settings.clone();
        let schema = self.schema.clone();
        let handler = settings.connect_changed(None, move |settings, key| {
            if STORED_KEYS.contains(&key) {
                for view in [KEY_PROCESSING_CHAIN, KEY_CHINESE_SCRIPT] {
                    callback(
                        ClientSetting::new(
                            ClientSettingKey::new(view).expect("view keys are non-empty"),
                            view_value(settings, view),
                        )
                        .expect("view setting is valid"),
                    );
                }
                return;
            }
            if !schema.has_key(key) {
                return;
            }
            let schema_key = schema.key(key);
            let range = setting_range(&schema_key);
            if let Ok(value) = value_from_variant(&settings.value(key), &range, key) {
                callback(
                    ClientSetting::new(
                        ClientSettingKey::new(key).expect("schema keys are non-empty"),
                        value,
                    )
                    .expect("schema-derived setting is valid"),
                );
            }
        });
        Ok(Box::new(GioSubscription {
            settings: self.settings.clone(),
            handler: Some(handler),
        }))
    }
}

struct GioSubscription {
    settings: gio::Settings,
    handler: Option<glib::SignalHandlerId>,
}

impl ClientSettingsSubscription for GioSubscription {}

impl Drop for GioSubscription {
    fn drop(&mut self) {
        if let Some(handler) = self.handler.take() {
            self.settings.disconnect(handler);
        }
    }
}

fn setting_range(key: &gio::SettingsSchemaKey) -> SettingRange {
    let range = key.range();
    let kind = range.child_value(0).get::<String>();
    let detail = range.child_value(1).get::<glib::Variant>();
    match (kind.as_deref(), detail) {
        (Some("enum"), Some(detail)) => detail
            .get::<Vec<String>>()
            .map(SettingRange::Choices)
            .unwrap_or(SettingRange::Unrestricted),
        (Some("range"), Some(detail)) if detail.n_children() == 2 => {
            let minimum = value_from_variant(
                &detail.child_value(0),
                &SettingRange::Unrestricted,
                key.name().as_str(),
            );
            let maximum = value_from_variant(
                &detail.child_value(1),
                &SettingRange::Unrestricted,
                key.name().as_str(),
            );
            match (minimum, maximum) {
                (Ok(minimum), Ok(maximum)) => SettingRange::Range { minimum, maximum },
                _ => SettingRange::Unrestricted,
            }
        }
        _ => SettingRange::Unrestricted,
    }
}

fn value_from_variant(
    value: &glib::Variant,
    range: &SettingRange,
    key: &str,
) -> Result<ClientSettingValue, ClientSettingsError> {
    if let Some(integer) = integer_from_variant(value) {
        return Ok(ClientSettingValue::Integer(integer));
    }
    if let Some(boolean) = value.get::<bool>() {
        return Ok(ClientSettingValue::Boolean(boolean));
    }
    if let Some(values) = value.get::<Vec<String>>() {
        return Ok(ClientSettingValue::List(values));
    }
    let value = value
        .get::<String>()
        .ok_or_else(|| ClientSettingsError::InvalidValue {
            key: key.to_owned(),
            message: format!("unsupported GVariant type {}", value.type_()),
        })?;
    Ok(match range {
        SettingRange::Choices(_) => ClientSettingValue::Choice(value),
        _ => ClientSettingValue::Text(value),
    })
}

/// Whether the page has a row for a key of this type. A newer schema can add
/// a type this build predates; that key is left off the page, not the page.
fn holds(kind: &glib::VariantTy) -> bool {
    [
        glib::VariantTy::INT32,
        glib::VariantTy::UINT32,
        glib::VariantTy::BOOLEAN,
        glib::VariantTy::STRING,
        glib::VariantTy::STRING_ARRAY,
    ]
    .contains(&kind)
}

/// The integer widths a settings row holds (`i` and `u`, the two GSettings
/// schemas use), widened to `i64`. Any other width stays a string to the
/// page, which then refuses it the way it refuses any unknown type.
fn integer_from_variant(value: &glib::Variant) -> Option<i64> {
    value
        .get::<i32>()
        .map(i64::from)
        .or_else(|| value.get::<u32>().map(i64::from))
}

/// `value` in the key's own integer width, or `None` when it does not fit -
/// which the schema's range check would have refused anyway.
fn integer_variant(value: i64, kind: &glib::VariantTy) -> Option<glib::Variant> {
    if *kind == *glib::VariantTy::INT32 {
        i32::try_from(value).ok().map(|v| v.to_variant())
    } else if *kind == *glib::VariantTy::UINT32 {
        u32::try_from(value).ok().map(|v| v.to_variant())
    } else {
        None
    }
}

fn private_keyfile_path() -> Result<PathBuf, ClientSettingsError> {
    myna_core::settings::store_path().ok_or_else(|| ClientSettingsError::StoreUnavailable {
        message: "neither SNAP_USER_COMMON nor HOME is set".into(),
    })
}

fn schema_unavailable() -> ClientSettingsError {
    ClientSettingsError::SchemaUnavailable {
        schema_id: SCHEMA_ID,
        guidance: "The com.canonical.Myna.Dictation schema is not installed. Reinstall the myna-config package, then restart Myna Settings.",
    }
}
