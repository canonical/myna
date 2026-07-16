use crate::format::AudioFormat;

/// Opaque identifier for an audio-producing node, scoped to a backend session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeId(pub String);

impl NodeId {
    /// Create a node id from a backend-specific string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// An audio-producing node or source exposed by the audio server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputNode {
    /// Unique identifier within a backend enumeration snapshot.
    pub id: NodeId,
    /// Stable machine name (e.g. `alsa_input.pci-0000_00_1f.3.analog-stereo`).
    pub name: String,
    /// Human-readable label for UIs.
    pub description: String,
    /// Whether this is the server's current default input.
    pub is_default: bool,
    /// Rates/formats/channel layouts the node advertises.
    pub supported_formats: Vec<AudioFormat>,
}
