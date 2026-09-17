//! Read-only access to the accessibility preference set (announcement
//! verbosity, sound cues, silence auto-stop — spec Assumptions,
//! project-plan T54). This feature does not own or persist these
//! preferences; [`GSettingsPreferences`] reads the real, already-shipped
//! `com.canonical.Myna.Dictation` GSettings store (`myna_core::settings`) that
//! `feat(client): Move settings into GSettings, with a snap-config default`
//! established for `streaming-mode`/`language`/`activation`/`hotkey`, now
//! extended with `announcement-verbosity`/`sound-cues-enabled`/
//! `silence-auto-stop-seconds` keys. [`DefaultPreferences`] is the
//! dependency-free, FR-004-mandated-defaults implementor used directly by
//! hermetic tests that should not touch GSettings at all.

pub use myna_core::settings::AnnouncementVerbosity as Verbosity;

/// Read-only accessibility preferences, consulted by the announcer/sound/
/// auto-stop logic. See module docs: this is a read seam, not a store.
pub trait Preferences: Send + Sync {
    fn verbosity(&self) -> Verbosity;
    fn sound_cues_enabled(&self) -> bool;
    fn silence_auto_stop_seconds(&self) -> Option<u32>;
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

    fn silence_auto_stop_seconds(&self) -> Option<u32> {
        // project-plan T59: "~15 s of no voice input".
        Some(15)
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

    fn silence_auto_stop_seconds(&self) -> Option<u32> {
        Some(myna_core::settings::Settings::load().silence_timeout)
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

    #[test]
    fn default_silence_auto_stop_matches_t59() {
        assert_eq!(DefaultPreferences.silence_auto_stop_seconds(), Some(15));
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
        assert_eq!(
            gsettings.silence_auto_stop_seconds(),
            default.silence_auto_stop_seconds()
        );
    }
}
