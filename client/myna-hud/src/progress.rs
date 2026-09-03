//! progress — a plain `GtkProgressBar` HUD indicator (feature 004).
//!
//! This is the `progress` `hud-style`: the simplest possible view, a stock
//! `gtk::ProgressBar` whose `fraction` tracks the calibrated level. The value
//! is **fixed** — it is set once when a level arrives and held until the next
//! push. There is deliberately no per-frame animation, no stale-decay and no
//! "breathing/vibrating" effect: the bar just shows the latest volume level
//! as a static filled portion. No custom drawing — the widget is whatever the
//! theme styles `GtkProgressBar` as.
//!
//! Pure logic lives in [`crate::vumeter`] (the shared dBFS-calibrated
//! envelope); this module only wires it onto the widget.

use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::vumeter;

/// The bar's height, matching the other indicator views, so the `hud-style`
/// options occupy the same footprint.
pub const METER_HEIGHT: i32 = 8;

/// A plain progress-bar level indicator.
pub struct ProgressView {
    bar: gtk::ProgressBar,
}

impl ProgressView {
    /// Build the bar.
    pub fn new() -> Rc<Self> {
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("myna-hud-progress");
        bar.set_height_request(METER_HEIGHT);
        bar.set_hexpand(true);
        bar.set_show_text(false);
        bar.set_fraction(0.0);
        Rc::new(Self { bar })
    }

    /// The bar as a [`gtk::Widget`], to embed in the pill.
    pub fn widget(&self) -> &gtk::Widget {
        self.bar.upcast_ref()
    }

    /// A level push from the publisher. Sets the fraction to the *fresh*
    /// intensity (no decay) and holds it until the next push — a fixed value
    /// based on the volume level, not an animated one.
    pub fn push_level(&self, rms: f64, peak: f64) {
        let intensity = vumeter::levels_to_intensity(rms, peak, 0.0);
        self.bar.set_fraction(intensity.clamp(0.0, 1.0));
        self.bar.queue_draw();
    }

    /// Queue a redraw (a no-op beyond the widget's own repaint).
    pub fn queue_draw(&self) {
        self.bar.queue_draw();
    }
}
