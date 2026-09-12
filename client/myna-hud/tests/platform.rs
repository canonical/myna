// tests/platform.rs — the runtime-probing layer's decode rules (feature
// 004, T122; research R26, data-model E2b). The probes themselves need a
// display and live under `examples/platform_probe.rs`; what is pinned here
// is the part that silently mis-reads.

use gtk::glib;
use gtk4 as gtk;

use myna_hud::platform::decode_reduced_motion;

// `GtkSettings:gtk-interface-reduced-motion` is a `GtkReducedMotion` ENUM
// (no_preference = 0, reduce = 1), not the boolean its name suggests.
// Reading it as a bool yields None — indistinguishable from "GTK < 4.22
// does not have this property" — which silently forfeits the primary
// reduced-motion source on exactly the systems that ship it. That bug is
// invisible: the fallback produces a plausible answer.

#[test]
fn an_enum_valued_property_decodes_rather_than_looking_absent() {
    // Orientation stands in for GtkReducedMotion: any registered enum
    // exercises the same GValue path, and gtk4-rs exposes no ReducedMotion
    // type without a v4_22 compile-time feature (which the runtime matrix
    // forbids).
    let no_preference = glib::Value::from(gtk::Orientation::Horizontal); // 0
    let reduce = glib::Value::from(gtk::Orientation::Vertical); // 1

    assert_eq!(
        decode_reduced_motion(&no_preference),
        Some(false),
        "no_preference (0) means full motion — and must NOT read as absent"
    );
    assert_eq!(
        decode_reduced_motion(&reduce),
        Some(true),
        "reduce (1) means reduced motion"
    );
}

#[test]
fn additive_enum_values_err_toward_less_animation() {
    // A future stronger level (say 2) must not read as "no preference".
    // Orientation has only 0/1, so this checks the rule via the boundary
    // the decoder actually applies: anything but 0 is reduced motion.
    let reduce = glib::Value::from(gtk::Orientation::Vertical);
    assert_eq!(decode_reduced_motion(&reduce), Some(true));
}

#[test]
fn a_boolean_property_still_decodes() {
    // Defensive: if a stack ever exposes it as a plain gboolean.
    assert_eq!(decode_reduced_motion(&glib::Value::from(true)), Some(true));
    assert_eq!(
        decode_reduced_motion(&glib::Value::from(false)),
        Some(false)
    );
}

#[test]
fn an_unrelated_type_reads_as_absent() {
    // Neither bool nor enum: report absence so `motion` falls back rather
    // than inventing an answer.
    assert_eq!(decode_reduced_motion(&glib::Value::from("reduce")), None);
    assert_eq!(decode_reduced_motion(&glib::Value::from(1i32)), None);
}
