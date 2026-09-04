//! bar — the default HUD indicator (feature 004).
//!
//! This is the `bar` `hud-style`: a single horizontal level bar whose filled
//! portion tracks the calibrated level. The look of `vumeter.png` (the default
//! since 2026-09-03).
//!
//! Colours come from CSS, like the rest of the pill: the bar's `color` is
//! resolved by the theme — `@accent_bg_color` normally, `var(--warning-bg-color)`
//! under the recoverable (`notice`) class — and read back at snapshot time via
//! [`Widget::color`](gtk4::Widget::color). No hardcoded RGB and no colour
//! probing in the view.
//!
//! The pure envelope lives in [`crate::vumeter`]; the state-driven animation
//! (loading pulse, transcribing, finalizing, notice) comes from
//! [`crate::hud_logic::indicator_state_fraction`].

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gtk::glib;
use gtk::graphene;
use gtk::gsk;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::vumeter;

/// The bar's height, matching the ribbon's and the other views', so the
/// `hud-style` options occupy the same footprint.
pub const METER_HEIGHT: i32 = 32;

/// The bar's height within the widget, leaving vertical breathing room so it
/// reads as a thin bar, not a filled block.
const BAR_HEIGHT_FRACTION: f64 = 0.42;

/// Alpha of the dim track (the unfilled part of the bar).
const TRACK_ALPHA: f64 = 0.18;

/// The CSS class that switches the bar to the warning (recoverable) colour.
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
    pub struct BarView {
        pub(super) level: RefCell<Option<LevelSample>>,
        /// The current dictation state, for the state-driven animation.
        pub(super) key: RefCell<Option<crate::states::DictationState>>,
        pub(super) severity: RefCell<Option<crate::states::Severity>>,
        pub(super) state_since: RefCell<Option<Instant>>,
        /// The desktop's reduce-animation preference — makes the pulse static.
        pub(super) reduced_motion: RefCell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BarView {
        const NAME: &'static str = "MynaHudBar";
        type Type = super::BarView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for BarView {}

    impl WidgetImpl for BarView {
        /// Paint the bar via Gsk: a rounded clip over the bar's bounds, then a
        /// dim track and the fill up to the state-driven fraction. The colour
        /// is the widget's CSS-resolved `color` (accent, or warning under the
        /// recoverable class).
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let w = widget.width() as f64;
            let h = widget.height() as f64;
            if w <= 0.0 || h <= 0.0 {
                return;
            }

            let intensity = super::current_intensity(&self.level);
            let state = super::indicator_state(self, intensity);

            // The theme-resolved colour: accent, or warning when a notice.
            let color = widget.color();

            let bar_h = h * BAR_HEIGHT_FRACTION;
            let bar_y = (h - bar_h) / 2.0;
            let radius = (bar_h / 2.0) as f32;
            let bar_bounds = graphene::Rect::new(0.0, bar_y as f32, w as f32, bar_h as f32);
            let rounded = gsk::RoundedRect::from_rect(bar_bounds, radius);

            snapshot.push_rounded_clip(&rounded);
            let track = graphene::Rect::new(0.0, bar_y as f32, w as f32, bar_h as f32);
            snapshot.append_color(&with_alpha(&color, TRACK_ALPHA), &track);

            match state.pulse {
                // Indeterminate activity: a little block travelling back and
                // forth (pong), tinted with the accent at the pulse's alpha
                // (semi-transparent for loading). The block gets its OWN
                // rounded corners — the track clip alone would leave hard
                // vertical edges on it.
                Some(pulse) => {
                    // A pong back-and-forth; the block gets its OWN rounded
                    // corners — the track clip alone would leave hard
                    // vertical edges on it.
                    let (since, period) = super::state_elapsed(self, pulse.period_ms);
                    let centre = crate::hud_logic::pulse_position(since, period);
                    let half = pulse.width / 2.0;
                    let x0 = w * (centre - half).clamp(0.0, 1.0);
                    let x1 = w * (centre + half).clamp(0.0, 1.0);
                    let block = graphene::Rect::new(
                        x0 as f32,
                        bar_y as f32,
                        (x1 - x0) as f32,
                        bar_h as f32,
                    );
                    let block_rounded = gsk::RoundedRect::from_rect(block, radius);
                    snapshot.push_rounded_clip(&block_rounded);
                    snapshot.append_color(&with_alpha(&color, pulse.alpha), &block);
                    snapshot.pop();
                }
                // A plain level (or a full warning fill): fraction of the bar.
                None => {
                    let fraction = state.fraction.clamp(0.0, 1.0);
                    let fill_w = (w * fraction) as f32;
                    let fill = graphene::Rect::new(0.0, bar_y as f32, fill_w, bar_h as f32);
                    snapshot.append_color(&with_alpha(&color, 1.0), &fill);
                }
            }

            snapshot.pop();
        }
    }
}

/// A copy of `color` with the given alpha (the beta is a boxed RGBA).
fn with_alpha(color: &gtk::gdk::RGBA, alpha: f64) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::new(color.red(), color.green(), color.blue(), alpha as f32)
}

glib::wrapper! {
    /// A simple horizontal level bar.
    pub struct BarView(ObjectSubclass<imp::BarView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// The calibrated intensity for the current level, decaying with age.
fn current_intensity(level: &RefCell<Option<LevelSample>>) -> f64 {
    let sample = level.borrow();
    match *sample {
        Some(LevelSample { rms, peak, at }) => {
            let age_ms = at.elapsed().as_secs_f64() * 1000.0;
            vumeter::levels_to_intensity(rms, peak, age_ms)
        }
        None => 0.0,
    }
}

/// The state-driven animation for the current state.
fn indicator_state(imp: &imp::BarView, intensity: f64) -> crate::hud_logic::IndicatorState {
    let (key, severity, state_ms) = state_parts(imp);
    let reduced_motion = *imp.reduced_motion.borrow();
    crate::hud_logic::indicator_state(key, severity, intensity, state_ms, reduced_motion)
}

/// The `(key, severity, state_ms)` the current state decodes to.
fn state_parts(
    imp: &imp::BarView,
) -> (
    crate::states::DictationState,
    Option<crate::states::Severity>,
    f64,
) {
    let (key, severity, state_ms) = match *imp.key.borrow() {
        Some(key) => {
            let since = (*imp.state_since.borrow()).unwrap_or_else(Instant::now);
            (
                key,
                *imp.severity.borrow(),
                since.elapsed().as_secs_f64() * 1000.0,
            )
        }
        None => (crate::states::DictationState::Idle, None, 0.0),
    };
    (key, severity, state_ms)
}

/// The elapsed time in the current pulse state, cycling with `period_ms` so a
/// long-lived state keeps animating (the `state_ms` keeps growing, but the
/// pulse position wraps via `pulse_position`).
fn state_elapsed(imp: &imp::BarView, period_ms: f64) -> (f64, f64) {
    let (_, _, state_ms) = state_parts(imp);
    (state_ms % period_ms.max(1.0), period_ms)
}

impl BarView {
    /// Build the bar.
    pub fn new() -> Rc<Self> {
        let bar: BarView = glib::Object::builder().build();
        bar.add_css_class("myna-hud-bar");
        bar.set_height_request(METER_HEIGHT);
        bar.set_hexpand(true);
        bar.set_can_focus(false);
        Rc::new(bar)
    }

    /// The bar as a [`gtk::Widget`], to embed in the pill.
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
    /// `notice` warning colour via the CSS class). The pill calls this on
    /// every state change.
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
            // (warning vs accent) tracks the severity.
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
