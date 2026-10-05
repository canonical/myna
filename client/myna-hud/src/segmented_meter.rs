//! segmented_meter — the classic segmented bar meter HUD view (feature 004).
//!
//! This is the `vumeter` alternative to the GPU wave ribbon
//! ([`crate::ribbon`]) and the accent bar ([`crate::bar::BarView`]),
//! selectable through the `hud-style` GSettings key. It is a direct port of
//! the pre-ribbon GJS `BarMeterActor`: fixed-height segments illuminate
//! left-to-right as the calibrated level rises, with conventional
//! green → yellow → red zones and a slight per-segment taper.
//!
//! The pure envelope math it drives lives in [`crate::vumeter`]
//! (the dBFS calibration + `levels_to_intensity` + the segment helpers) and
//! its state in [`crate::indicator`]; this module owns only the GTK drawing. The widget is a [`gtk::Widget`] subclass
//! painted through **Gsk** — it overrides
//! [`snapshot`](gtk::subclass::widget::WidgetImpl::snapshot) and appends one
//! coloured rectangle per segment. No cairo.
//!
//! Like the pill, the view is self-driving: `push_level` records the latest
//! level + arrival time, and the pill's frame clock queues a redraw while
//! visible, so a stalled publisher visibly falls to the floor rather than
//! freezing (R16a).

use std::rc::Rc;

use gtk::glib;
use gtk::graphene;
use gtk::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk4 as gtk;

use crate::indicator::Indicator;
use crate::vumeter::{intensity_to_active_segments, segment_color, SegmentColor};

/// The meter's height, matching the ribbon's (`crate::pill::RIBBON_HEIGHT`)
/// and the bar's, so the `hud-style` options occupy the same footprint.
pub const METER_HEIGHT: i32 = 32;

/// The number of segments in the classic meter (the GJS `BAR_COUNT`).
pub const BAR_COUNT: usize = 24;

mod imp {
    use super::*;
    use gtk::subclass::prelude::*;
    use gtk::subclass::widget::WidgetImpl;

    #[derive(Default)]
    pub struct SegmentedMeterView {
        pub(super) indicator: Indicator,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SegmentedMeterView {
        const NAME: &'static str = "MynaHudVumeter";
        type Type = super::SegmentedMeterView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for SegmentedMeterView {}

    impl WidgetImpl for SegmentedMeterView {
        /// Paint the segmented meter via Gtk: one coloured rectangle per
        /// segment. The state drives a fixed lit count (level) or a moving
        /// lit cluster (pulse), per
        /// [`crate::hud_logic::indicator_state`].
        #[allow(deprecated)]
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let w = widget.width() as f64;
            let h = widget.height() as f64;
            if w <= 0.0 || h <= 0.0 {
                return;
            }

            let frame = self.indicator.frame();
            let state = frame.state;
            // Which segments are lit: a pulse is a moving cluster around the
            // pong centre; a plain level lights from the left.
            let level_count = intensity_to_active_segments(state.fraction, BAR_COUNT);
            let (pulse_centre, pulse_half) = match state.pulse {
                Some(pulse) => {
                    let count = (pulse.width * BAR_COUNT as f64).round().max(1.0) as usize;
                    let centre = crate::hud_logic::pulse_position(
                        frame.state_ms % pulse.period_ms.max(1.0),
                        pulse.period_ms,
                    );
                    let centre_seg = (centre * BAR_COUNT as f64).round() as usize;
                    (Some(centre_seg), count / 2)
                }
                None => (None, 0),
            };
            let lit = move |i: usize| match pulse_centre {
                Some(centre) => i.abs_diff(centre) <= pulse_half,
                None => i < level_count,
            };

            let gap = w / BAR_COUNT as f64;
            let bar_width = gap * 0.55;
            // Resolve the gauge's zone colours from CSS once per frame (the
            // libadwaita accent variables via the @define-color aliases).
            let style = widget.style_context();
            for (i, position) in bar_positions().enumerate() {
                let is_lit = lit(i);
                let alpha: f32 = if is_lit { 1.0 } else { 0.16 };
                let color = zone_color(style.as_ref(), position, alpha);
                // Conventional VU: fixed-height segments light left-to-right;
                // a slight taper (taller at the loud end) keeps the row vital.
                let bar_h = h * (0.66 + 0.34 * position);
                let x = (i as f64 * gap + (gap - bar_width) / 2.0) as f32;
                let y = ((h - bar_h) / 2.0) as f32;
                let bounds = graphene::Rect::new(x, y, bar_width as f32, bar_h as f32);
                snapshot.append_color(&color, &bounds);
            }
        }
    }
}

glib::wrapper! {
    /// The classic segmented bar meter.
    pub struct SegmentedMeterView(ObjectSubclass<imp::SegmentedMeterView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SegmentedMeterView {
    /// Build the meter.
    pub fn new() -> Rc<Self> {
        let meter: SegmentedMeterView = glib::Object::builder().build();
        meter.add_css_class("myna-hud-vumeter");
        meter.set_height_request(METER_HEIGHT);
        meter.set_hexpand(true);
        meter.set_can_focus(false);
        Rc::new(meter)
    }

    /// The meter as a [`gtk::Widget`], to embed in the pill.
    pub fn widget(&self) -> &gtk::Widget {
        self.upcast_ref()
    }

    /// A level push from the publisher.
    pub fn push_level(&self, rms: f64, peak: f64) {
        let indicator = &self.imp().indicator;
        indicator.push_level(rms, peak);
        indicator.restart_easing();
        self.queue_draw();
    }

    /// Set the current dictation state (drives the state animation). The
    /// pill calls this on every state change.
    pub fn set_state(
        &self,
        key: crate::states::DictationState,
        severity: Option<crate::states::Severity>,
    ) {
        self.imp().indicator.set_state(key, severity);
        self.queue_draw();
    }

    /// Set the reduce-animation preference (a slower pulse).
    pub fn set_reduced_motion(&self, reduced: bool) {
        if self.imp().indicator.set_reduced_motion(reduced) {
            self.queue_draw();
        }
    }
}

/// The colour zone for a segment drawn at normalized place `position`,
/// resolved from CSS: the `@define-color myna-vu-<zone>` aliases of the
/// libadwaita `--accent-*` variables. Falls back to white when the name is
/// somehow unresolvable (a theme without the alias) rather than panicking.
///
/// Uses the now-deprecated `gtk_style_context_lookup_color` — the only way to
/// read a CSS custom-property value at runtime in this GTK; there is no
/// non-deprecated replacement, so it is explicitly allowed here.
#[allow(deprecated)]
fn zone_color(style: &gtk::StyleContext, position: f64, alpha: f32) -> gtk::gdk::RGBA {
    let name = match segment_color(position) {
        SegmentColor::Red => "myna-vu-red",
        SegmentColor::Yellow => "myna-vu-yellow",
        SegmentColor::Green => "myna-vu-green",
    };
    let resolved = style.lookup_color(name).unwrap_or_else(|| {
        // Unresolvable: a neutral grey (never a hardcoded zone colour).
        gtk::gdk::RGBA::new(1.0, 1.0, 1.0, 1.0)
    });
    resolved.with_alpha(alpha)
}

/// The normalized place (`(i+1)/BAR_COUNT`) of each segment, left to right.
fn bar_positions() -> impl Iterator<Item = f64> {
    (0..BAR_COUNT).map(move |i| (i + 1) as f64 / BAR_COUNT as f64)
}
