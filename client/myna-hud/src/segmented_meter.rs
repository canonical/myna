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
//! (the dBFS calibration + `levels_to_intensity` + the segment helpers); this
//! module owns only the GTK drawing. The widget is a [`gtk::Widget`] subclass
//! painted through **Gsk** — it overrides
//! [`snapshot`](gtk::subclass::widget::WidgetImpl::snapshot) and appends one
//! coloured rectangle per segment. No cairo.
//!
//! Like the pill, the view is self-driving: `push_level` records the latest
//! level + arrival time, and the pill's frame clock queues a redraw while
//! visible, so a stalled publisher visibly falls to the floor rather than
//! freezing (R16a).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gtk::glib;
use gtk::graphene;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::vumeter::{self, intensity_to_active_segments, segment_color, SegmentColor};

/// The meter's height, matching the ribbon's (`crate::pill::RIBBON_HEIGHT`)
/// and the bar's, so the `hud-style` options occupy the same footprint.
pub const METER_HEIGHT: i32 = 32;

/// The number of segments in the classic meter (the GJS `BAR_COUNT`).
pub const BAR_COUNT: usize = 24;

/// The CSS class that switches the meter to the warning (recoverable) colour.
/// Mirrors the pill's own `.myna-hud-severity-recoverable`.
const WARNING_CLASS: &str = "myna-hud-severity-recoverable";

/// A level push and when it arrived — the VU decays by *arrival age* (R16a).
#[derive(Clone, Copy)]
struct LevelSample {
    rms: f64,
    peak: f64,
    at: Instant,
}

mod imp {
    use super::*;
    use gtk::subclass::prelude::*;
    use gtk::subclass::widget::WidgetImpl;

    #[derive(Default)]
    pub struct SegmentedMeterView {
        pub(super) level: RefCell<Option<LevelSample>>,
        /// The current dictation state, for the state-driven animation
        /// (loading pulse, notice amber, …).
        pub(super) key: RefCell<Option<crate::states::DictationState>>,
        pub(super) severity: RefCell<Option<crate::states::Severity>>,
        pub(super) state_since: RefCell<Option<Instant>>,
        /// The desktop's reduce-animation preference — makes the pulse static.
        pub(super) reduced_motion: RefCell<bool>,
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
        /// segment. The state drives a fixed lit count (level), a moving lit
        /// cluster (pulse), or a full warning fill, per
        /// [`crate::hud_logic::indicator_state`].
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let w = widget.width() as f64;
            let h = widget.height() as f64;
            if w <= 0.0 || h <= 0.0 {
                return;
            }

            let sample = self.level.borrow();
            let (rms, peak, age_ms) = match *sample {
                Some(LevelSample { rms, peak, at }) => {
                    (rms, peak, at.elapsed().as_secs_f64() * 1000.0)
                }
                None => (0.0, 0.0, vumeter::STALE_MS + 1.0),
            };
            let intensity = vumeter::levels_to_intensity(rms, peak, age_ms);

            let (key, severity, state_ms) = match *self.key.borrow() {
                Some(key) => {
                    let since = (*self.state_since.borrow()).unwrap_or_else(Instant::now);
                    (
                        key,
                        *self.severity.borrow(),
                        since.elapsed().as_secs_f64() * 1000.0,
                    )
                }
                None => (crate::states::DictationState::Idle, None, 0.0),
            };
            let state = crate::hud_logic::indicator_state(
                key,
                severity,
                intensity,
                state_ms,
                *self.reduced_motion.borrow(),
            );
            // The notice/warning colour comes from the widget's CSS-resolved
            // `color` (the recoverable class); the classic gauge still uses
            // its green/yellow/red scale. No hardcoded amber.
            let warning_color = if state.warning {
                Some(widget.color())
            } else {
                None
            };
            // Which segments are lit: a pulse is a moving cluster around the
            // pong centre; a plain level lights from the left.
            let level_count = intensity_to_active_segments(state.fraction, BAR_COUNT);
            let (pulse_centre, pulse_half) = match state.pulse {
                Some(pulse) => {
                    let count = (pulse.width * BAR_COUNT as f64).round().max(1.0) as usize;
                    let centre = crate::hud_logic::pulse_position(
                        state_ms % pulse.period_ms.max(1.0),
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
            for (i, position) in bar_positions().enumerate() {
                let is_lit = lit(i);
                let alpha = if is_lit { 1.0 } else { 0.16 };
                let color = match warning_color {
                    Some(c) => with_alpha(&c, alpha),
                    None => segment_rgba(position, alpha),
                };
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

    /// A level push from the publisher. Never deduplicated — the arrival time
    /// is what keeps a steady voice from decaying (R16a).
    pub fn push_level(&self, rms: f64, peak: f64) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let imp = self.imp();
        *imp.level.borrow_mut() = Some(LevelSample {
            rms,
            peak,
            at: Instant::now(),
        });
        self.queue_draw();
    }

    /// Set the current dictation state (drives the state animation and the
    /// `notice` warning tint). The pill calls this on every state change.
    pub fn set_state(
        &self,
        key: crate::states::DictationState,
        severity: Option<crate::states::Severity>,
    ) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let imp = self.imp();
        let changed = *imp.key.borrow() != Some(key) || *imp.severity.borrow() != severity;
        *imp.key.borrow_mut() = Some(key);
        *imp.severity.borrow_mut() = severity;
        if changed {
            *imp.state_since.borrow_mut() = Some(Instant::now());
            // Mirror the pill's recoverable CSS class so the theme colour
            // (warning vs gauge scale) tracks the severity.
            let is_warning = severity == Some(crate::states::Severity::Recoverable);
            let widget = self.widget();
            if is_warning {
                widget.add_css_class(WARNING_CLASS);
            } else {
                widget.remove_css_class(WARNING_CLASS);
            }
        }
        self.queue_draw();
    }

    /// Set the reduce-animation preference (static, unanimated pulse).
    pub fn set_reduced_motion(&self, reduced: bool) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let imp = self.imp();
        if *imp.reduced_motion.borrow() == reduced {
            return;
        }
        *imp.reduced_motion.borrow_mut() = reduced;
        self.queue_draw();
    }

    /// Queue a redraw — the pill's frame clock calls this while visible.
    pub fn queue_draw(&self) {
        WidgetExt::queue_draw(self);
    }
}

/// The colour zone for a segment drawn at normalized place `position`.
fn segment_rgba(position: f64, alpha: f64) -> gtk::gdk::RGBA {
    // Conventional VU colours (the GJS BarMeterActor RGBA values).
    let (r, g, b) = match segment_color(position) {
        SegmentColor::Red => (0.95, 0.24, 0.20),
        SegmentColor::Yellow => (0.98, 0.72, 0.18),
        SegmentColor::Green => (0.20, 0.82, 0.42),
    };
    gtk::gdk::RGBA::new(r as f32, g as f32, b as f32, alpha as f32)
}

/// A copy of `color` with the given alpha (the boxed RGBA).
fn with_alpha(color: &gtk::gdk::RGBA, alpha: f64) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::new(color.red(), color.green(), color.blue(), alpha as f32)
}

/// The normalized place (`(i+1)/BAR_COUNT`) of each segment, left to right.
fn bar_positions() -> impl Iterator<Item = f64> {
    (0..BAR_COUNT).map(move |i| (i + 1) as f64 / BAR_COUNT as f64)
}
