//! Wire-protocol versioning — the Rust mirror of Python `myna.core.protocol`.
//!
//! A single number versions the whole client↔service contract over a transport:
//! the handshake, the event vocabulary, and the config/capabilities wire shapes.
//! It travels in band in the opening `session.start` (transport-agnostic, not a
//! WebSocket subprotocol token). Adding or renaming any of those is a breaking
//! change — bump the version.

/// The protocol version this build speaks.
pub const PROTOCOL_VERSION: &str = "1";

/// Versions this build can serve.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[PROTOCOL_VERSION];

/// Whether a peer-declared version is one we can serve. A missing version
/// (`None`) is compatible: clients predating the version field necessarily speak
/// the only version that existed then (matches Python `is_supported`).
pub fn is_supported(version: Option<&str>) -> bool {
    match version {
        None => true,
        Some(v) => SUPPORTED_PROTOCOL_VERSIONS.contains(&v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_is_supported() {
        assert!(is_supported(Some(PROTOCOL_VERSION)));
    }

    #[test]
    fn missing_version_is_compatible() {
        assert!(is_supported(None));
    }

    #[test]
    fn unknown_version_rejected() {
        assert!(!is_supported(Some("99")));
        assert!(!is_supported(Some("")));
    }
}
