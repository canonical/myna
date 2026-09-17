//! Plain-language failure presentations (US4, FR-023–027,
//! `contracts/failure-mapping.md`): one `FailurePresentation` authored once
//! per known ad-hoc failure source, rendered identically by the indicator,
//! notification, terminal, and accessibility-announcement surfaces.

use crate::accessibility::Severity;
use std::collections::HashMap;

/// A single failure's plain-language presentation (data-model.md), authored
/// once and rendered identically everywhere (FR-024). `id` is the stable key
/// two different call sites for the same underlying concept both look up, so
/// they structurally cannot diverge in wording (FR-024a, contract F3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailurePresentation {
    pub id: &'static str,
    pub message: &'static str,
    pub recovery_action: &'static str,
    pub severity: Severity,
}

/// The keyed registry every failure source looks up into (contract F1/F3).
/// Empty for now — populated with real entries in US4 (T067); the scaffold
/// here only establishes the lookup-by-stable-id property.
#[derive(Debug, Default)]
pub struct FailureRegistry(HashMap<&'static str, FailurePresentation>);

impl FailureRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, presentation: FailurePresentation) {
        self.0.insert(presentation.id, presentation);
    }

    /// Looks up a failure by its stable id. Two call sites naming the same
    /// `id` always get the identical `FailurePresentation` value back — the
    /// registry, not per-call-site authoring, is what makes F3 hold.
    pub fn lookup(&self, id: &str) -> Option<&FailurePresentation> {
        self.0.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T028: repeated lookups of the same id never diverge (F3) ────────────

    #[test]
    fn repeated_lookups_of_the_same_id_return_the_identical_presentation() {
        let mut registry = FailureRegistry::new();
        registry.register(FailurePresentation {
            id: "no_microphone",
            message: "No microphone is available.",
            recovery_action: "Connect a microphone and try again.",
            severity: Severity::Critical,
        });

        let first = registry.lookup("no_microphone").cloned();
        let second = registry.lookup("no_microphone").cloned();
        assert_eq!(first, second);
        assert!(first.is_some());
    }

    #[test]
    fn unknown_id_returns_none() {
        let registry = FailureRegistry::new();
        assert_eq!(registry.lookup("nonexistent"), None);
    }
}
