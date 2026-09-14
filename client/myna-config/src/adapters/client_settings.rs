use std::path::{Path, PathBuf};

use gio::glib::prelude::ObjectExt;
use gio::glib::{self, variant::ToVariant};
use gio::prelude::SettingsExt;

use crate::domain::{
    ClientSetting, ClientSettingKey, ClientSettingMetadata, ClientSettingValue, SettingRange,
};
use crate::ports::{
    ClientSettings, ClientSettingsCallback, ClientSettingsError, ClientSettingsSubscription,
};
use myna_core::settings::SCHEMA_ID;

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
        keys.sort();
        keys.iter().map(|key| self.metadata(key)).collect()
    }

    fn get(&self, key: &str) -> Result<ClientSettingValue, ClientSettingsError> {
        let schema_key = self.schema_key(key)?;
        value_from_variant(&self.settings.value(key), &setting_range(&schema_key), key)
    }

    fn set(&self, key: &str, value: ClientSettingValue) -> Result<(), ClientSettingsError> {
        let schema_key = self.schema_key(key)?;
        self.ensure_writable(key)?;
        let range = setting_range(&schema_key);
        let variant = match (&range, &value) {
            (SettingRange::Choices(_), ClientSettingValue::Choice(value))
            | (SettingRange::Unrestricted, ClientSettingValue::Text(value)) => value.to_variant(),
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

    fn reset(&self, key: &str) -> Result<(), ClientSettingsError> {
        self.schema_key(key)?;
        self.ensure_writable(key)?;
        self.settings.reset(key);
        Ok(())
    }

    fn subscribe(
        &self,
        callback: ClientSettingsCallback,
    ) -> Result<Box<dyn ClientSettingsSubscription>, ClientSettingsError> {
        let settings = self.settings.clone();
        let schema = self.schema.clone();
        let handler = settings.connect_changed(None, move |settings, key| {
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
