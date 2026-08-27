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
//! The surface's input region is empty in **every** state, so pointer
//! events always reach whatever is underneath — the HUD is an overlay, not
//! a target, and carries no interactive control at all. A critical error is
//! cleared by the client publishing a new state, not by the user clicking
//! the pill.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gdk4_x11 as gdkx11;
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
use crate::input_region::input_region_rects;
use crate::notice_slot::NoticeSlot;
use crate::platform;
use crate::ribbon::{compute_ribbon_model, RibbonInput, RibbonPhase};
use crate::states::Descriptor;
use crate::vumeter::levels_to_intensity;

/// The pill's resting width, matching the extension's `PILL_WIDTH`. It is a
/// FLOOR, not a fixed size: a long error reason grows the pill up to
/// [`PILL_MAX_WIDTH`] and wraps beyond that.
pub const PILL_WIDTH: i32 = 360;

/// The label's wrap width, in characters — what actually keeps the pill at
/// [`PILL_WIDTH`] when a long reason arrives.
///
/// GTK offers no pixel maximum for a widget, and the alternatives do not
/// work here (all measured): `AdwClamp` bounds the child's *allocation*, not
/// the window's natural size, so the window grew to 700px; a
/// `GtkScrolledWindow` hands its child unlimited width, so the label stops
/// wrapping altogether and the window reached 1280px. `max-width-chars` is
/// the lever that does bound a wrapping label, and holding the pill to one
/// width — rather than letting it grow to some larger ceiling — also stops
/// the overlay changing width underneath the user as messages change.
pub const LABEL_MAX_CHARS: i32 = 30;
/// The ribbon's height, matching the extension's `RIBBON_HEIGHT`.
pub const RIBBON_HEIGHT: i32 = 32;

/// Object-data key under which the window owns its [`HudWindow`].
const SELF_KEY: &str = "myna-hud-instance";

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
    /// The accent last read from the theme, so the palette is rebuilt only
    /// when the colour genuinely changes rather than every frame.
    accent: Option<crate::shader::Rgb>,
}

/// The HUD pill window.
pub struct HudWindow {
    window: gtk::ApplicationWindow,
    pill: gtk::Box,
    icon: gtk::Image,
    label: gtk::Label,
    ribbon: gtk::GLArea,
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
        // Centred: with the mic on the left and nothing on the right, a
        // left-aligned line sits off-centre in the pill, and a wrapped
        // reason reads as ragged rather than as a balanced block.
        label.set_xalign(0.5);
        label.set_justify(gtk::Justification::Center);
        // Wrap rather than ellipsize: a critical error's reason is the one
        // piece of text the user actually needs to read, and "Microphone
        // unavailable — check…" helps nobody. The width is bounded by the
        // pill instead (PILL_MAX_WIDTH), so wrapping is what absorbs a long
        // reason.
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_natural_wrap_mode(gtk::NaturalWrapMode::Word);
        label.set_max_width_chars(LABEL_MAX_CHARS);

        let ribbon = gtk::GLArea::new();
        ribbon.add_css_class("myna-hud-ribbon");
        ribbon.set_height_request(RIBBON_HEIGHT);
        ribbon.set_hexpand(true);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.set_hexpand(true);
        // A critical error hides the ribbon, leaving the label alone in a
        // box that would otherwise pack it against the top edge.
        content.set_valign(gtk::Align::Center);
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
            accent: None,
        }));

        let renderer: Rc<RefCell<Option<RibbonRenderer>>> = Rc::default();

        let hud = Rc::new(Self {
            window,
            pill,
            icon,
            label,
            ribbon,
            state,
            renderer,
            preferences: RefCell::new(None),
        });

        // Tie our lifetime to the window's. Every callback below holds a
        // WEAK reference (a strong one would keep the struct alive through
        // its own widgets forever), so without this the caller's `Rc` is
        // the only owner — and the moment it drops, the frame clock stops
        // while the widgets carry on being displayed: a HUD that is still
        // on screen but frozen, with no animation, no notice expiry and no
        // accent updates. That is a silent failure, and it bit both the
        // gallery and the accent-change example before this existed.
        //
        // The reference cycle this creates (window -> data -> Rc<Self> ->
        // window) is broken on destroy.
        unsafe {
            hud.window.set_data(SELF_KEY, hud.clone());
        }
        hud.window.connect_destroy(|window| unsafe {
            let _ = window.steal_data::<Rc<HudWindow>>(SELF_KEY);
        });

        hud.connect_x11_hints();
        hud.connect_renderer();
        hud.connect_palette();
        hud.connect_clock();
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
    pub fn apply_descriptor(self: &Rc<Self>, descriptor: Descriptor) {
        let now = self.now_ms();
        {
            let mut state = self.state.borrow_mut();
            if descriptor.severity.is_some() {
                state
                    .notice
                    .hold(descriptor.severity, &descriptor.status_text, now);
            } else {
                state.notice.clear();
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

        // Nothing is shown at idle (FR-002/X3) — push-to-talk means the
        // resting state is an absent HUD, not an empty one.
        //
        // The WINDOW is hidden, not just the pill: with its only child
        // hidden the window has no natural size and falls back to GTK's
        // 200x200 default, leaving an empty surface that still counts as
        // the overlay's extent for the host's placement and input region.
        //
        // The host must therefore expect the surface to come and go, and
        // adopt on every map rather than only the first — which it has to
        // do anyway, since the renderer can be respawned under it (R21).
        self.pill.set_visible(!descriptor.hidden);
        self.window.set_visible(!descriptor.hidden);

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
                this.schedule_accent_resync();
            }
        });
    }

    /// Ask an X11 window manager to keep the overlay out of the taskbar and
    /// the pager.
    ///
    /// On the shipping path this is redundant: the session is Wayland and
    /// the `myna-shell` host dock-types the window and hides it from the
    /// window list itself (R21). X11 is the lab/fallback case, where there
    /// is no host and the WM would otherwise list the HUD as an ordinary
    /// window.
    ///
    /// Note that **always-on-top has no GDK4 equivalent** — the X11 surface
    /// API exposes skip-taskbar, skip-pager, urgency and desktop placement,
    /// but nothing for `_NET_WM_STATE_ABOVE`. Stacking is the host's job on
    /// Wayland (`make_above` plus the DOCK type); doing it on X11 would mean
    /// setting the property through xlib directly.
    fn connect_x11_hints(self: &Rc<Self>) {
        self.window.connect_realize(|window| {
            let Some(surface) = window.surface() else {
                return;
            };
            // Fails cleanly on Wayland: the surface simply is not an X11 one.
            if let Some(x11) = surface.downcast_ref::<gdkx11::X11Surface>() {
                x11.set_skip_taskbar_hint(true);
                x11.set_skip_pager_hint(true);
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
                this.state.borrow_mut().notice.clear();
                this.apply_input_region();
            }

            if this.ribbon.is_visible() {
                this.ribbon.queue_render();
            }
            glib::ControlFlow::Continue
        });
    }

    fn connect_preferences(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        // Re-resolve accent + motion whenever the desktop changes either.
        let watch = platform::watch_preferences(move |readiness| {
            let Some(this) = this.upgrade() else { return };
            // Motion comes straight from its own sources, so it is always
            // read now.
            this.state.borrow_mut().reduced_motion = platform::probe_reduced_motion();

            // The accent is a computed CSS colour, so it is only readable
            // once the new styling is installed. libadwaita's own
            // notification guarantees that (it reloads the accent provider
            // before notifying), so read straight away; anything else has
            // to wait for the next frame.
            match readiness {
                platform::AccentReadiness::Current => this.sync_palette(),
                platform::AccentReadiness::NextFrame => this.schedule_accent_resync(),
            }
        });
        *self.preferences.borrow_mut() = Some(watch);
    }

    /// Make the surface fully click-through, in every state (R22/FR-025).
    ///
    /// The HUD is an overlay, never a target: it takes no pointer input at
    /// all, so the region is empty and stays empty. There is deliberately no
    /// interactive control on it — a critical error is cleared by the client
    /// publishing a new state, not by the user clicking the pill.
    /// Force a re-read of the theme's accent at the next frame.
    ///
    /// Public because a host (or a test) may know the styling changed for a
    /// reason none of the watched preferences covers — a stylesheet swapped
    /// at runtime, say. In the ordinary case the resync is scheduled
    /// automatically from the accent/theme settings and the style manager.
    pub fn resync_accent(self: &Rc<Self>) {
        self.schedule_accent_resync();
    }

    /// Re-resolve the ribbon's palette from the desktop, **once**, at the
    /// next frame.
    ///
    /// Used for triggers with no ordering guarantee (a raw GSettings key,
    /// the theme name, the initial map): the accent may still be the old
    /// one when they fire, and GTK recomputes styles lazily for the next
    /// frame anyway. Costs nothing in the steady state, unlike re-reading on
    /// every repaint.
    fn schedule_accent_resync(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        self.ribbon.add_tick_callback(move |_area, _clock| {
            if let Some(this) = this.upgrade() {
                this.sync_palette();
            }
            glib::ControlFlow::Break
        });
    }

    /// Read the desktop's accent and rebuild the palette if it changed.
    fn sync_palette(&self) {
        let palette = platform::probe_accent_palette(Some(&self.ribbon));
        let accent = palette.main_rgb();
        let mut state = self.state.borrow_mut();
        if state.accent == Some(accent) {
            return;
        }
        state.accent = Some(accent);
        state.palette = palette.as_ribbon_palette();
        drop(state);
        self.ribbon.queue_render();
    }

    fn apply_input_region(&self) {
        let Some(surface) = self.window.surface() else {
            return;
        };
        let rects = input_region_rects(self.state.borrow().descriptor.severity);
        debug_assert!(
            rects.is_empty(),
            "the HUD takes no pointer input in any state"
        );
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
