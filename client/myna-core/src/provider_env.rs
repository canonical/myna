//! Parser and role vocabulary for `provider.env` in `inference-provider` shares.

use std::collections::HashMap;

/// Filename written into each `inference-provider` content share directory.
pub const PROVIDER_ENV_FILENAME: &str = "provider.env";

/// Key in `provider.env` that identifies a Myna snap's role on the shared
/// `inference-provider` interface (`speech` or `text`). Unknown keys are
/// ignored by `canonical/inference` (`internal/providers/connected.go`).
pub const MYNA_ROLE_KEY: &str = "MYNA_ROLE";

/// Value of [`MYNA_ROLE_KEY`] written by `myna-server` in speech snaps.
pub const ROLE_SPEECH: &str = "speech";

/// Value of [`MYNA_ROLE_KEY`] written by `myna-text` in text snaps.
pub const ROLE_TEXT: &str = "text";

/// Default Unix socket filename used by Myna speech snaps, including releases
/// written before [`MYNA_ROLE_KEY`] was introduced.
pub const SPEECH_SOCKET_FILENAME: &str = "myna.sock";

/// Declared role from [`MYNA_ROLE_KEY`] in `provider.env`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclaredRole {
    Speech,
    Text,
    Other,
}

impl DeclaredRole {
    pub fn from_env(env: &HashMap<String, String>) -> Option<Self> {
        match env.get(MYNA_ROLE_KEY).map(String::as_str) {
            None | Some("") => None,
            Some(ROLE_SPEECH) => Some(Self::Speech),
            Some(ROLE_TEXT) => Some(Self::Text),
            Some(_) => Some(Self::Other),
        }
    }
}

/// Parse a `provider.env` file: `KEY=value` lines, blank lines and `#`
/// comments ignored, one pair of matching outer quotes stripped. No
/// interpolation, no escapes.
pub fn parse_env(text: &str) -> Option<HashMap<String, String>> {
    let mut env = HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        let value = value.trim();
        let unquoted = ['"', '\'']
            .iter()
            .find_map(|&q| value.strip_prefix(q).and_then(|rest| rest.strip_suffix(q)))
            .unwrap_or(value);
        env.insert(key.to_string(), unquoted.to_string());
    }
    Some(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_role_parses_known_empty_and_unknown_values() {
        let role_of = |text: &str| {
            let env = parse_env(text).expect("valid env");
            DeclaredRole::from_env(&env)
        };
        assert_eq!(
            role_of("# comment\n\nSNAP_NAME = \"myna-parakeet\"\nSINGLE='v'\n"),
            None
        );
        assert_eq!(parse_env("NO_EQUALS\n"), None);
        assert_eq!(parse_env("=missing_key\n"), None);
        assert_eq!(role_of("SNAP_NAME=myna-parakeet\nMYNA_ROLE=\n"), None);
        assert_eq!(
            role_of("SNAP_NAME=myna-parakeet\nMYNA_ROLE=speech\n"),
            Some(DeclaredRole::Speech)
        );
        assert_eq!(
            role_of("SNAP_NAME=myna-text\nMYNA_ROLE=text\n"),
            Some(DeclaredRole::Text)
        );
        assert_eq!(
            role_of("SNAP_NAME=myna-tts\nMYNA_ROLE=tts\n"),
            Some(DeclaredRole::Other)
        );
    }
}
