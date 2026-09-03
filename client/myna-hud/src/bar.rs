//! bar — the default HUD indicator (feature 004).
//!
//! This is the `bar` `hud-style`: a single horizontal level bar whose filled
//! portion follows the **accent colour** as the calibrated level rises, over a
//! dim track. It is the look of `vumeter.png` (the default since 2026-09-03).
//!
//! Pure logic lives in [`crate::vumeter`] (the shared dBFS-calibrated
//! envelope); this module owns only the GTK drawing.
//!
//! The widget is a [`gtk::Widget`] subclass that paints through **Gsk** — it
//! overrides [`snapshot`](gtk::subclass::widget::WidgetImpl::snapshot) and
//! appends a rounded clip + two colour rects (track, then accent fill). No
//! cairo.
//!
//! Like the pill, the bar is self-driving: `push_level` records the latest
//! level + arrival time, and the pill's frame clock queues a redraw while
//! visible, so a stalled publisher visibly falls to the floor rather than
//! freezing (R16a).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gtk::glib;
use gtk::graphene;
use gtk::gsk;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::shader::Rgb;
use crate::vumeter;

/// The bar's height, matching the ribbon's (`crate::pill::RIBBON_HEIGHT`) and
/// the segmented meter's, so the `hud-style` options occupy the same
/// footprint.
pub const METER_HEIGHT: i32 = 32;

/// The bar's height within the widget, leaving vertical breathing room so it
/// reads as a thin bar, not a filled block.
const BAR_HEIGHT_FRACTION: f64 = 0.42;

/// Alpha of the dim track (the unfilled part of the bar).
const TRACK_ALPHA: f64 = 0.18;

/// A level push and when it arrived — the VU decays by *arrival age* (R16a).
#[derive(Clone, Copy)]
struct LevelSample {
    rms: f64,
    peak: f64,
    at: Instant,
}

/// The accent colour, as `(r, g, b)` in `[0,1]` (f64).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
struct Color {
    r: f64,
    g: f64,
    b: f64,
}

impl From<Rgb> for Color {
    fn from(rgb: Rgb) -> Self {
        Self {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        }
    }
}

impl Color {
    fn rgba(self, alpha: f64) -> gtk::gdk::RGBA {
        gtk::gdk::RGBA::new(self.r as f32, self.g as f32, self.b as f32, alpha as f32)
    }
}

mod imp {
    use super::*;
    use gtk::subclass::prelude::*;
    use gtk::subclass::widget::WidgetImpl;

    #[derive(Default)]
    pub struct BarView {
        pub(super) level: RefCell<Option<LevelSample>>,
        pub(super) accent: RefCell<Color>,
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
        /// dim track and the accent-coloured fill up to the current level.
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
            let accent = *self.accent.borrow();

            let bar_h = h * BAR_HEIGHT_FRACTION;
            let bar_y = (h - bar_h) / 2.0;
            let radius = (bar_h / 2.0) as f32;
            let bar_bounds = graphene::Rect::new(0.0, bar_y as f32, w as f32, bar_h as f32);
            let rounded = gsk::RoundedRect::from_rect(bar_bounds, radius);

            snapshot.push_rounded_clip(&rounded);
            let track = graphene::Rect::new(0.0, bar_y as f32, w as f32, bar_h as f32);
            snapshot.append_color(&accent.rgba(TRACK_ALPHA), &track);
            let fill_w = (w * intensity.clamp(0.0, 1.0)) as f32;
            let fill = graphene::Rect::new(0.0, bar_y as f32, fill_w, bar_h as f32);
            snapshot.append_color(&accent.rgba(1.0), &fill);
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    /// A simple horizontal level bar following the accent colour.
    pub struct BarView(ObjectSubclass<imp::BarView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl BarView {
    /// Build the bar with a neutral default accent (dim until the pill pushes
    /// the real one via [`set_accent`]).
    pub fn new() -> Rc<Self> {
        let bar: BarView = glib::Object::builder().build();
        bar.add_css_class("myna-hud-vumeter");
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

    /// Set the accent colour the filled portion follows. The pill calls this
    /// whenever the theme accent changes (see `Pill::sync_palette`).
    pub fn set_accent(&self, color: Rgb) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let imp = self.imp();
        if *imp.accent.borrow() == color.into() {
            return;
        }
        *imp.accent.borrow_mut() = color.into();
        self.queue_draw();
    }

    /// Queue a redraw — the pill's frame clock calls this while visible.
    pub fn queue_draw(&self) {
        WidgetExt::queue_draw(self);
    }
}
