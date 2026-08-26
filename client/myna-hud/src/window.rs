//! window — the HUD pill window (feature 004, T112/T114; R21–R23).
//!
//! The visible half of the renderer: a borderless, transparent toplevel
//! holding the pill — mic icon, status label, GPU wave ribbon, and (only
//! for a critical error) a dismiss control. Every *decision* it makes is
//! delegated to the pure modules ([`crate::states`], [`crate::hud_logic`],
//! [`crate::vumeter`], [`crate::ribbon`], [`crate::notice_slot`],
//! [`crate::input_region`]); this module owns only widgets, the frame
//! clock, and the surface.
//!
//! ## What this window deliberately does NOT do
//!
//! It never positions, sizes-to-monitor, raises, or types itself. Under
//! GNOME the `myna-shell` extension launches it through a
//! `Meta.WaylandClient`, adopts the window, makes it a DOCK, and places it
//! (R21) — a renderer that also positioned itself would fight its host.
//! In [`lab`](crate::window::HudWindow::present_standalone) mode there is
//! no host, so it presents as an ordinary window.
//!
//! ## Click-through (R22/T114)
//!
//! The surface's input region is emptied so pointer events reach whatever
//! is underneath — the HUD is an overlay, not a target. The single
//! exception is the dismiss control during a critical error, whose
//! rectangle is punched back in. The region is re-applied whenever the
//! state or the layout changes, because both move that rectangle.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gettextrs::gettext;
use gtk::cairo;
use gtk::gdk;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::gl::RibbonRenderer;
use crate::hud_logic::{
    icon_for_severity, pill_color_class, ribbon_phase_for_state_key, ribbon_visible_for_severity,
    PILL_COLOR_CLASSES,
};
use crate::input_region::{input_region_rects, Rect};
use crate::notice_slot::NoticeSlot;
use crate::platform;
use crate::ribbon::{compute_ribbon_model, RibbonInput, RibbonPhase};
use crate::states::{Descriptor, Severity};
use crate::vumeter::levels_to_intensity;

/// The pill's fixed width, matching the extension's `PILL_WIDTH`.
pub const PILL_WIDTH: i32 = 360;
/// The ribbon's height, matching the extension's `RIBBON_HEIGHT`.
pub const RIBBON_HEIGHT: i32 = 32;

/// The most recent level push and when it arrived — the vumeter decays by
/// *arrival age*, so a stalled publisher visibly falls to the floor instead
/// of freezing mid-wave (R16a).
#[derive(Clone, Copy, Debug)]
struct LevelSample {
    rms: f64,
    peak: f64,
    at: Instant,
}

/// The mutable state the frame clock reads.
struct HudState {
    descriptor: Descriptor,
    notice: NoticeSlot,
    level: Option<LevelSample>,
    /// Forced by a state transition; `None` lets the ribbon manage itself.
    phase: RibbonPhase,
    phase_since: Instant,
    started: Instant,
    reduced_motion: bool,
    palette: crate::shader::RibbonPalette,
}

/// The HUD pill window.
pub struct HudWindow {
    window: gtk::ApplicationWindow,
    pill: gtk::Box,
    icon: gtk::Image,
    label: gtk::Label,
    ribbon: gtk::GLArea,
    dismiss: gtk::Button,
    state: Rc<RefCell<HudState>>,
    renderer: Rc<RefCell<Option<RibbonRenderer>>>,
    /// Owns the accent/reduced-motion subscriptions; dropped with the
    /// window, so no preference callback can outlive it.
    preferences: RefCell<Option<platform::PreferenceWatch>>,
}

impl HudWindow {
    /// Build the window and wire its clock, renderer and controls.
    pub fn new(app: &adw::Application) -> Rc<Self> {
        load_css();

        // A plain GtkApplicationWindow, deliberately NOT
        // adw::ApplicationWindow: the libadwaita window imposes a 200 px
        // minimum height (it is built for adaptive app windows), which
        // would leave ~130 px of dead transparent surface below a 66 px
        // pill — surface that still counts as the overlay's extent for the
        // host's placement (R21) and its input region (R22). libadwaita is
        // still used for the style manager's accent (R26); nothing here
        // needs an adw window.
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .resizable(false)
            .decorated(false)
            .build();

        window.add_css_class("myna-hud-window");
        // The HUD must never take focus away from the app being dictated
        // into. The host also enforces this by DOCK-typing the window
        // (mutter forces takes_focus = FALSE), but a renderer that asked
        // for focus would still steal it in lab mode.
        window.set_can_focus(false);

        let icon = gtk::Image::from_icon_name("audio-input-microphone-symbolic");
        icon.add_css_class("myna-hud-icon");
        icon.set_pixel_size(20);
        icon.set_valign(gtk::Align::Center);

        let label = gtk::Label::new(None);
        label.add_css_class("myna-hud-label");
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let ribbon = gtk::GLArea::new();
        ribbon.add_css_class("myna-hud-ribbon");
        ribbon.set_height_request(RIBBON_HEIGHT);
        ribbon.set_hexpand(true);

        let dismiss = gtk::Button::from_icon_name("window-close-symbolic");
        dismiss.add_css_class("myna-hud-dismiss");
        dismiss.set_valign(gtk::Align::Center);
        dismiss.set_visible(false);
        dismiss.set_tooltip_text(Some(&gettext("Dismiss")));

        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.set_hexpand(true);
        content.append(&label);
        content.append(&ribbon);

        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        pill.add_css_class("myna-hud-pill");
        // The pill's own request drives the WINDOW's size: a non-resizable
        // toplevel takes its content's natural size, whereas a default
        // height would leave the window taller than the pill — dead,
        // transparent surface that still counts as the overlay's extent for
        // the host's placement (R21) and for the input region (R22).
        pill.set_size_request(PILL_WIDTH, -1);
        pill.append(&icon);
        pill.append(&content);
        pill.append(&dismiss);
        window.set_child(Some(&pill));

        let state = Rc::new(RefCell::new(HudState {
            descriptor: crate::states::state_to_descriptor(None, ""),
            notice: NoticeSlot::default(),
            level: None,
            phase: RibbonPhase::Unfold,
            phase_since: Instant::now(),
            started: Instant::now(),
            reduced_motion: platform::probe_reduced_motion(),
            // No styled widget is rooted yet, so this is the fallback
            // palette; refresh_palette() re-resolves it from CSS as soon as
            // the ribbon is mapped.
            palette: platform::probe_accent_palette(None::<&gtk::Widget>).as_ribbon_palette(),
        }));

        let renderer: Rc<RefCell<Option<RibbonRenderer>>> = Rc::default();

        let hud = Rc::new(Self {
            window,
            pill,
            icon,
            label,
            ribbon,
            dismiss,
            state,
            renderer,
            preferences: RefCell::new(None),
        });

        hud.connect_renderer();
        hud.connect_palette();
        hud.connect_clock();
        hud.connect_dismiss();
        hud.connect_preferences();
        hud.apply_descriptor(crate::states::state_to_descriptor(None, ""));
        hud
    }

    /// The underlying window, for the application to present.
    pub fn window(&self) -> &gtk::ApplicationWindow {
        &self.window
    }

    /// Present as an ordinary window (lab mode — no host to adopt us).
    pub fn present_standalone(&self) {
        self.window.present();
    }

    // ── State in ────────────────────────────────────────────────────────

    /// Apply a state descriptor: label, icon, colour class, ribbon phase,
    /// held notice, visibility and input region.
    pub fn apply_descriptor(&self, descriptor: Descriptor) {
        let now = self.now_ms();
        {
            let mut state = self.state.borrow_mut();
            if descriptor.severity.is_some() {
                state
                    .notice
                    .hold(descriptor.severity, &descriptor.status_text, now);
            } else {
                state.notice.dismiss();
            }
            if let Some(phase) = ribbon_phase_for_state_key(descriptor.key) {
                // `flow` during the fresh-session unfold reveal is a no-op,
                // so the reveal is never cut short.
                let unfolding = state.phase == RibbonPhase::Unfold
                    && state.phase_since.elapsed().as_millis() < 400;
                if !(phase == RibbonPhase::Flow && unfolding) && state.phase != phase {
                    state.phase = phase;
                    state.phase_since = Instant::now();
                }
            }
            state.descriptor = descriptor.clone();
        }

        self.label.set_text(&descriptor.status_text);
        self.icon
            .set_icon_name(Some(icon_for_severity(descriptor.severity)));

        for class in PILL_COLOR_CLASSES {
            self.pill.remove_css_class(class);
        }
        if let Some(class) = pill_color_class(descriptor.key, descriptor.severity) {
            self.pill.add_css_class(class);
        }

        self.ribbon
            .set_visible(ribbon_visible_for_severity(descriptor.severity));
        self.dismiss
            .set_visible(descriptor.severity == Some(Severity::Critical));

        // The pill is hidden entirely at idle (FR-002/X3) — push-to-talk
        // means "nothing shown" is the resting state.
        self.pill.set_visible(!descriptor.hidden);

        // Announce the change to assistive technology: the status text is
        // the accessible description, and it is content-free by contract.
        self.pill
            .update_property(&[gtk::accessible::Property::Label(&descriptor.status_text)]);

        self.apply_input_region();
    }

    /// A level push from the publisher. Never deduplicated — the arrival
    /// time is what keeps a steady voice from decaying (R16a).
    pub fn push_level(&self, rms: f64, peak: f64) {
        self.state.borrow_mut().level = Some(LevelSample {
            rms,
            peak,
            at: Instant::now(),
        });
    }

    fn now_ms(&self) -> f64 {
        self.state.borrow().started.elapsed().as_secs_f64() * 1000.0
    }

    // ── Wiring ──────────────────────────────────────────────────────────

    /// Resolve the accent once the ribbon is rooted, and again whenever the
    /// theme recomputes its style (an accent change restyles the widget).
    fn connect_palette(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        self.ribbon.connect_map(move |_| {
            if let Some(this) = this.upgrade() {
                this.refresh_palette();
            }
        });
    }

    fn connect_renderer(self: &Rc<Self>) {
        let renderer = self.renderer.clone();
        self.ribbon.connect_realize(move |area| {
            area.make_current();
            if let Some(error) = area.error() {
                eprintln!("myna-hud: GL context failed: {error}");
                return;
            }
            match RibbonRenderer::realize() {
                Ok(built) => *renderer.borrow_mut() = Some(built),
                Err(e) => eprintln!("myna-hud: ribbon shader failed to build: {e}"),
            }
        });

        let renderer_unrealize = self.renderer.clone();
        self.ribbon.connect_unrealize(move |area| {
            area.make_current();
            if let Some(mut built) = renderer_unrealize.borrow_mut().take() {
                built.unrealize();
            }
        });

        let renderer_render = self.renderer.clone();
        let state = self.state.clone();
        self.ribbon.connect_render(move |area, _ctx| {
            let borrowed = renderer_render.borrow();
            let Some(renderer) = borrowed.as_ref() else {
                return glib::Propagation::Proceed;
            };

            // The GLArea's framebuffer is in device pixels.
            let scale = area.scale_factor();
            let width = area.width() * scale;
            let height = area.height() * scale;
            if width <= 0 || height <= 0 {
                return glib::Propagation::Proceed;
            }

            let state = state.borrow();
            let model = build_model(&state);
            renderer.render(&model, &state.palette, width, height);
            glib::Propagation::Proceed
        });
    }

    /// Drive the animation from the frame clock rather than a timer, so the
    /// ribbon advances in step with the compositor and stops when the
    /// window is not drawing.
    fn connect_clock(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        self.window.add_tick_callback(move |_widget, _clock| {
            let Some(this) = this.upgrade() else {
                return glib::ControlFlow::Break;
            };

            // A held recoverable notice clears itself on time.
            let now = this.now_ms();
            let expired = {
                let state = this.state.borrow();
                state.notice.severity().is_some() && !state.notice.is_showing(now)
            };
            if expired {
                this.state.borrow_mut().notice.dismiss();
                this.apply_input_region();
            }

            if this.ribbon.is_visible() {
                this.ribbon.queue_render();
            }
            glib::ControlFlow::Continue
        });
    }

    fn connect_dismiss(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        self.dismiss.connect_clicked(move |_| {
            let Some(this) = this.upgrade() else { return };
            this.state.borrow_mut().notice.dismiss();
            // An explicit dismiss returns the pill to its resting state.
            this.apply_descriptor(crate::states::state_to_descriptor(None, ""));
        });
    }

    /// Re-resolve the ribbon's palette from the live theme.
    ///
    /// The accent comes from the ribbon widget's own computed CSS colour
    /// (`color: @accent_bg_color` in `style.css`), which is only meaningful
    /// once the widget is rooted — hence the refresh on map as well as on
    /// every preference change.
    fn refresh_palette(&self) {
        let palette = platform::probe_accent_palette(Some(&self.ribbon)).as_ribbon_palette();
        self.state.borrow_mut().palette = palette;
        self.ribbon.queue_render();
    }

    fn connect_preferences(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        // Re-resolve accent + motion whenever the desktop changes either.
        let watch = platform::watch_preferences(move || {
            let Some(this) = this.upgrade() else { return };
            this.state.borrow_mut().reduced_motion = platform::probe_reduced_motion();
            this.refresh_palette();
        });
        *self.preferences.borrow_mut() = Some(watch);
    }

    /// Empty the surface's input region so the HUD is click-through,
    /// punching back only the dismiss control during a critical error
    /// (R22/T114).
    fn apply_input_region(&self) {
        let Some(surface) = self.window.surface() else {
            return;
        };
        let severity = self.state.borrow().descriptor.severity;

        let allocation = if self.dismiss.is_visible() {
            let bounds = self.dismiss.compute_bounds(&self.window);
            bounds.map(|b| Rect {
                x: b.x() as f64,
                y: b.y() as f64,
                width: b.width() as f64,
                height: b.height() as f64,
            })
        } else {
            None
        };

        let rects = input_region_rects(severity, allocation);
        let region = cairo::Region::create();
        for rect in rects {
            let r = cairo::RectangleInt::new(
                rect.x as i32,
                rect.y as i32,
                rect.width as i32,
                rect.height as i32,
            );
            let _ = region.union_rectangle(&r);
        }
        surface.set_input_region(Some(&region));
    }
}

/// Build the current frame's ribbon model from the live state.
fn build_model(state: &HudState) -> crate::ribbon::RibbonModel {
    let elapsed_ms = state.started.elapsed().as_secs_f64() * 1000.0;
    let envelope = match state.level {
        Some(sample) => {
            let age_ms = sample.at.elapsed().as_secs_f64() * 1000.0;
            levels_to_intensity(sample.rms, sample.peak, age_ms)
        }
        None => 0.0,
    };

    compute_ribbon_model(RibbonInput {
        envelope,
        elapsed_ms,
        phase: state.phase,
        phase_elapsed_ms: state.phase_since.elapsed().as_secs_f64() * 1000.0,
        reduced_motion: state.reduced_motion,
        severity_tint: state.notice.severity(),
        ..Default::default()
    })
}

/// Install the pill's stylesheet once per display.
fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(include_str!("style.css"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
