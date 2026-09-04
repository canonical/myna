//! progress — a plain `GtkProgressBar` HUD indicator (feature 004).
//!
//! This is the `progress` `hud-style`: the simplest possible view, a stock
//! `gtk::ProgressBar`. Its `fraction` is driven by the current state through
//! [`crate::hud_logic::indicator_state_fraction`] — a static level while
//! recording, a bounce while loading/transcribing, a settle while finalizing,
//! and a full, gentle pulse on a `notice`. Colour comes from CSS: the bar's
//! `progress` sub-node is styled `@accent_bg_color`, and the recoverable class
//! switches it to the warning colour. No hardcoded RGB, no cairo.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::vumeter;

/// The bar's height, matching the other indicator views, so the `hud-style`
/// options occupy the same footprint.
pub const METER_HEIGHT: i32 = 8;

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

/// A plain progress-bar level indicator, self-driving on the pill's clock.
pub struct ProgressView {
    bar: gtk::ProgressBar,
    level: RefCell<Option<LevelSample>>,
    key: RefCell<Option<crate::states::DictationState>>,
    severity: RefCell<Option<crate::states::Severity>>,
    state_since: RefCell<Option<Instant>>,
    reduced_motion: RefCell<bool>,
    smoothed_level: RefCell<f64>,
    last_frame: RefCell<Option<Instant>>,
}

impl ProgressView {
    /// Build the bar and start its frame clock.
    pub fn new() -> Rc<Self> {
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("myna-hud-progress");
        bar.set_height_request(METER_HEIGHT);
        bar.set_hexpand(true);
        bar.set_show_text(false);
        bar.set_fraction(0.0);

        let this = Rc::new(Self {
            bar,
            level: RefCell::new(None),
            key: RefCell::new(None),
            severity: RefCell::new(None),
            state_since: RefCell::new(None),
            reduced_motion: RefCell::new(false),
            smoothed_level: RefCell::new(0.0),
            last_frame: RefCell::new(None),
        });
        this.connect_clock();
        this
    }

    /// The bar as a [`gtk::Widget`], to embed in the pill.
    pub fn widget(&self) -> &gtk::Widget {
        self.bar.upcast_ref()
    }

    /// A level push from the publisher. The bar tracks this while in a plain
    /// (recording/active) state.
    pub fn push_level(&self, rms: f64, peak: f64) {
        *self.level.borrow_mut() = Some(LevelSample {
            rms,
            peak,
            at: Instant::now(),
        });
        *self.last_frame.borrow_mut() = None;
        self.update_fraction();
    }

    /// Set the current dictation state (drives the fraction animation and the
    /// `notice` warning CSS class). The pill calls this on every state change.
    pub fn set_state(
        &self,
        key: crate::states::DictationState,
        severity: Option<crate::states::Severity>,
    ) {
        let changed = *self.key.borrow() != Some(key) || *self.severity.borrow() != severity;
        *self.key.borrow_mut() = Some(key);
        *self.severity.borrow_mut() = severity;
        if changed {
            *self.state_since.borrow_mut() = Some(Instant::now());
            let is_warning = severity == Some(crate::states::Severity::Recoverable);
            if is_warning {
                self.bar.add_css_class(WARNING_CLASS);
            } else {
                self.bar.remove_css_class(WARNING_CLASS);
            }
        }
        self.update_fraction();
    }

    /// Set the reduce-animation preference (static, unanimated pulse).
    pub fn set_reduced_motion(&self, reduced: bool) {
        if *self.reduced_motion.borrow() == reduced {
            return;
        }
        *self.reduced_motion.borrow_mut() = reduced;
        self.update_fraction();
    }

    /// Queue a redraw (no-op beyond the widget's own repaint).
    pub fn queue_draw(&self) {
        self.bar.queue_draw();
    }

    /// Recompute and apply the fraction from the current state + level: a plain
    /// level (or full warning fill) sets the fraction; a pulse state advances
    /// the stock `GtkProgressBar`'s indeterminate block.
    fn update_fraction(&self) {
        let state = self.indicator_state();
        match state.pulse {
            // A little block travelling back and forth (GTK's pulse). The
            // step controls the block width; GTK animates it internally.
            Some(pulse) => {
                self.bar.set_pulse_step(pulse.width.clamp(0.01, 1.0));
                self.bar.pulse();
            }
            None => {
                self.bar.set_fraction(state.fraction.clamp(0.0, 1.0));
            }
        }
        self.bar.queue_draw();
    }

    /// The state-driven animation.
    fn indicator_state(&self) -> crate::hud_logic::IndicatorState {
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
        let intensity = self.smoothed_intensity();
        crate::hud_logic::indicator_state(
            key,
            severity,
            intensity,
            state_ms,
            *self.reduced_motion.borrow(),
        )
    }

    /// The smoothed level for this frame, advancing the easing state.
    fn smoothed_intensity(&self) -> f64 {
        let now = Instant::now();
        let dt_ms = match *self.last_frame.borrow() {
            Some(prev) => now.duration_since(prev).as_secs_f64() * 1000.0,
            None => 0.0,
        };
        let smoothed = crate::hud_logic::smooth_level(
            *self.smoothed_level.borrow(),
            current_intensity(&self.level),
            dt_ms,
            *self.reduced_motion.borrow(),
        );
        *self.smoothed_level.borrow_mut() = smoothed;
        *self.last_frame.borrow_mut() = Some(now);
        smoothed
    }

    /// Drive the fraction from the frame clock while visible, so the bounce
    /// states animate and everything else holds a static value. Hidden (idle)
    /// costs nothing.
    fn connect_clock(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.bar.add_tick_callback(move |_widget, _clock| {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !this.bar.is_visible() {
                return glib::ControlFlow::Continue;
            }
            this.update_fraction();
            glib::ControlFlow::Continue
        });
    }
}

/// The calibrated intensity for the current sample, decaying with age.
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
