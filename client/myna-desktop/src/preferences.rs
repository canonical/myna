//! Read-only access to the accessibility preference set (announcement
//! verbosity, sound cues — spec Assumptions). This feature does not own or
//! persist these preferences; [`GSettingsPreferences`] reads the real,
//! already-shipped `com.canonical.Myna.Dictation` GSettings store
//! (`myna_core::settings`) that
//! `feat(client): Move settings into GSettings, with a snap-config default`
//! established for `streaming-mode`/`language`, now extended with
//! `announcement-verbosity`/`sound-cues-enabled` keys.
//! [`DefaultPreferences`] is the dependency-free, FR-004-mandated-defaults
//! implementor used directly by hermetic tests that should not touch
//! GSettings at all.
//!
//! **Silence auto-stop is deliberately not here.** FR-018/FR-021 (project-plan
//! T59) are served by `myna_desktop::AutoStop`, which the daemon resolves
//! from `myna_core::settings::Settings::silence_timeout` and hands to the
//! controller as a `Live<AutoStop>`. That path owns the whole behaviour —
//! measuring captured audio, warning about a noisy input, and finalizing the
//! session — so a second read seam here would be a parallel source of truth
//! for one setting.

pub use myna_core::settings::AnnouncementVerbosity as Verbosity;

/// Read-only accessibility preferences, consulted by the announcer and sound
/// logic. See module docs: this is a read seam, not a store.
pub trait Preferences: Send + Sync {
    fn verbosity(&self) -> Verbosity;
    fn sound_cues_enabled(&self) -> bool;

    /// Per-[`crate::sound::CueKind`] override, layered on top of the single
    /// global [`Preferences::sound_cues_enabled`] toggle (feature
    /// 011-accessible-dictation-ux, T054; FR-010's "individually
    /// disableable" cues). No per-cue GSettings key exists yet — this is
    /// deliberately a default trait method returning `true` ("no per-cue
    /// override applies"), not "the cue is enabled": callers must still AND
    /// this with `sound_cues_enabled()` (see
    /// `sound::GatedSoundCuePlayer::play`). Defaulting to `true` means every
    /// existing `Preferences` implementor (including ones outside this
    /// crate, if any) keeps compiling unchanged when a real per-cue key is
    /// added later — a documented, intentional scope decision, not an
    /// oversight (see `sound` module docs' "known scope gap").
    fn cue_enabled(&self, _cue: crate::sound::CueKind) -> bool {
        true
    }
}

/// The FR-004-mandated defaults as compile-time constants, with no GSettings
/// dependency at all — the seam hermetic tests (`accessibility::gate`,
/// `accessibility::recover`) use directly. [`GSettingsPreferences`] falls
/// back to the identical values (via `Settings::default()`) when the real
/// store's schema is not installed, so the two can never silently disagree
/// (asserted by `gsettings_preferences_matches_defaults_when_schema_absent`
/// below).
pub struct DefaultPreferences;

impl Preferences for DefaultPreferences {
    fn verbosity(&self) -> Verbosity {
        Verbosity::AllTransitions
    }

    fn sound_cues_enabled(&self) -> bool {
        true
    }
}

/// The real, production `Preferences` seam: reads the `com.canonical.Myna.Dictation`
/// GSettings store on every call (no cached handle — `gio::Settings` is not
/// `Send`, and this trait is, so nothing here can hold one across calls; see
/// `myna_core::settings::Store`'s own module docs). `Settings::load()`
/// already falls back to `Settings::default()` when the schema is not
/// installed, so this type needs no fallback logic of its own — it is a
/// pure adapter.
pub struct GSettingsPreferences;

impl Preferences for GSettingsPreferences {
    fn verbosity(&self) -> Verbosity {
        myna_core::settings::Settings::load().announcement_verbosity
    }

    fn sound_cues_enabled(&self) -> bool {
        myna_core::settings::Settings::load().sound_cues_enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T007: DefaultPreferences matches FR-004's mandated defaults ─────────

    #[test]
    fn default_verbosity_is_all_transitions() {
        assert_eq!(DefaultPreferences.verbosity(), Verbosity::AllTransitions);
    }

    #[test]
    fn default_sound_cues_are_enabled() {
        assert!(DefaultPreferences.sound_cues_enabled());
    }

    // ── GSettingsPreferences: falls back to the identical FR-004 defaults
    //    when com.canonical.Myna.Dictation is not installed (true in this hermetic
    //    test process — nothing here calls `make install-schema`) ──────────

    #[test]
    fn gsettings_preferences_matches_defaults_when_schema_absent() {
        let gsettings = GSettingsPreferences;
        let default = DefaultPreferences;
        assert_eq!(gsettings.verbosity(), default.verbosity());
        assert_eq!(gsettings.sound_cues_enabled(), default.sound_cues_enabled());
    }
}
