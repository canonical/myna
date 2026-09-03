//! progress — a plain `GtkProgressBar` HUD indicator (feature 004).
//!
//! This is the `progress` `hud-style`: the simplest possible view, a stock
//! `gtk::ProgressBar` whose `fraction` tracks the calibrated level (with
//! stale-decay, so it falls to the floor when the publisher stalls). No custom
//! drawing — the widget is whatever the theme styles `GtkProgressBar` as.
//!
//! Pure logic lives in [`crate::vumeter`] (the shared dBFS-calibrated
//! envelope); this module only wires it onto the widget.

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

/// A level push and when it arrived — the VU decays by *arrival age* (R16a).
#[derive(Clone, Copy)]
struct LevelSample {
    rms: f64,
    peak: f64,
    at: Instant,
}

/// A plain progress-bar level indicator.
pub struct ProgressView {
    bar: gtk::ProgressBar,
    level: RefCell<Option<LevelSample>>,
}

impl ProgressView {
    /// Build the bar, start its frame clock.
    pub fn new() -> Rc<Self> {
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("myna-hud-progress");
        bar.set_height_request(METER_HEIGHT);
        bar.set_hexpand(true);
        bar.set_show_text(false);

        let this = Rc::new(Self {
            bar,
            level: RefCell::new(None),
        });

        this.connect_clock();
        this
    }

    /// The bar as a [`gtk::Widget`], to embed in the pill.
    pub fn widget(&self) -> &gtk::Widget {
        self.bar.upcast_ref()
    }

    /// A level push from the publisher. Never deduplicated — the arrival time
    /// is what keeps a steady voice from decaying (R16a).
    pub fn push_level(&self, rms: f64, peak: f64) {
        *self.level.borrow_mut() = Some(LevelSample {
            rms,
            peak,
            at: Instant::now(),
        });
        self.queue_draw();
    }

    /// Queue a redraw — the pill's frame clock calls this while visible.
    pub fn queue_draw(&self) {
        let intensity = self.current_intensity();
        self.bar.set_fraction(intensity);
        self.bar.queue_draw();
    }

    /// The calibrated intensity for the current sample, decaying with age.
    fn current_intensity(&self) -> f64 {
        let sample = self.level.borrow();
        match *sample {
            Some(LevelSample { rms, peak, at }) => {
                let age_ms = at.elapsed().as_secs_f64() * 1000.0;
                vumeter::levels_to_intensity(rms, peak, age_ms)
            }
            None => 0.0,
        }
    }

    /// Drive the animation from the frame clock, exactly like the pill, so the
    /// bar tracks the envelope while visible and costs nothing when hidden.
    fn connect_clock(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.bar.add_tick_callback(move |_widget, _clock| {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !this.bar.is_visible() {
                // Hidden: no repaint (idle costs nothing).
                return glib::ControlFlow::Continue;
            }
            this.queue_draw();
            glib::ControlFlow::Continue
        });
    }
}
