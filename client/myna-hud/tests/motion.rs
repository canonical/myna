// tests/motion.rs — hermetic test for the reduced-motion resolution rules
// (R26, data-model E2b, FR-022a). The pure decision consumes raw readings
// from whichever sources the runtime probing found; the GTK-side probing
// (GtkSettings property lookup, GSettings schema guard) lives in the
// application layer.

use myna_hud::motion::{reduced_motion, MotionReadings};

#[test]
fn gtk_property_is_the_primary_source() {
    // Present and Reduce → reduced, regardless of what enable-animations says.
    assert!(reduced_motion(&MotionReadings {
        gtk_reduced_motion: Some(true),
        enable_animations: Some(true),
    }));
    // Present and NoPreference → full motion, even with animations disabled
    // (the newer, more specific source wins).
    assert!(!reduced_motion(&MotionReadings {
        gtk_reduced_motion: Some(false),
        enable_animations: Some(false),
    }));
}

#[test]
fn enable_animations_is_the_older_gtk_fallback() {
    // Property absent (GTK < 4.22 — the snap's 4.18, the 24.04 workshop):
    // the inverted enable-animations GSettings key decides.
    assert!(reduced_motion(&MotionReadings {
        gtk_reduced_motion: None,
        enable_animations: Some(false),
    }));
    assert!(!reduced_motion(&MotionReadings {
        gtk_reduced_motion: None,
        enable_animations: Some(true),
    }));
}

#[test]
fn no_sources_means_full_motion() {
    // Both absent (very old stack / schema-less environment): default to
    // full motion — never reduced-by-default, never a crash (E2b).
    assert!(!reduced_motion(&MotionReadings {
        gtk_reduced_motion: None,
        enable_animations: None,
    }));
}

// The crash-on-start guard (E2b): the new
// `org.gnome.desktop.a11y.interface reduced-motion` GSettings key is NEVER a
// source here — an unguarded read against the absent schema/key aborts the
// process. The resolution's inputs are the two safe sources above only;
// this is structural (MotionReadings has no field that could carry it).
#[test]
fn only_the_two_safe_sources_exist() {
    // Compile-time shape check: MotionReadings has exactly the two fields,
    // both Option<bool> — there is no a11y-key input to misuse.
    let readings = MotionReadings {
        gtk_reduced_motion: None,
        enable_animations: None,
    };
    let MotionReadings {
        gtk_reduced_motion,
        enable_animations,
    } = readings;
    let _: Option<bool> = gtk_reduced_motion;
    let _: Option<bool> = enable_animations;
}
