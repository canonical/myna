//! pill — the HUD pill widget (feature 004), extracted from the window so it
//! can live either in a borderless overlay toplevel ([`crate::window`]) or
//! embedded inside another window (the `--serve-dbus` publisher's preview).
//!
//! It owns the widget tree (mic icon, status label, GPU wave ribbon), the
//! live state, the GL renderer, the frame clock, and the accent/motion
//! subscriptions. Every *decision* is delegated to the pure modules
//! ([`crate::states`], [`crate::hud_logic`], [`crate::vumeter`],
//! [`crate::ribbon`], [`crate::notice_slot`]); this owns only widgets and
//! their wiring.
//!
//! The pill knows nothing about the surface: click-through, positioning and
//! window typing are overlay concerns that live in [`crate::window`].

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::gl::RibbonRenderer;
use crate::hud_logic::{
    icon_for_severity, pill_color_class, ribbon_phase_for_state_key, ribbon_visible_for_severity,
    PILL_COLOR_CLASSES,
};
use crate::notice_slot::NoticeSlot;
use crate::platform;
use crate::ribbon::{compute_ribbon_model, RibbonInput, RibbonPhase};
use crate::shader::hex_to_rgb;
use crate::states::Descriptor;
use crate::vumeter::levels_to_intensity;

/// The pill's resting width, matching the extension's `PILL_WIDTH`. It is a
/// FLOOR, not a fixed size: a long error reason grows the pill up to a
/// ceiling and wraps beyond that (bounded by [`LABEL_MAX_CHARS`]).
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
struct PillState {
    descriptor: Descriptor,
    notice: NoticeSlot,
    level: Option<LevelSample>,
    /// Forced by a state transition; `None` lets the ribbon manage itself.
    phase: RibbonPhase,
    phase_since: Instant,
    started: Instant,
    reduced_motion: bool,
    /// Lab override: when `Some`, replaces the desktop-derived
    /// `reduced_motion` and is not clobbered by a live preference change.
    reduced_motion_override: Option<bool>,
    palette: crate::shader::RibbonPalette,
    /// The accent last read from the theme, so the palette is rebuilt only
    /// when the colour genuinely changes rather than every frame.
    accent: Option<crate::shader::Rgb>,
    /// Lab override: when `Some`, forces the accent hex instead of the
    /// desktop's — libadwaita has no public runtime accent setter (it is a
    /// desktop preference), so the lab forces the palette directly.
    accent_override: Option<String>,
}

/// The HUD pill: a `gtk::Box` styled `.myna-hud-pill`, self-driving.
pub struct Pill {
    pill: gtk::Box,
    icon: gtk::Image,
    label: gtk::Label,
    ribbon: gtk::GLArea,
    state: Rc<RefCell<PillState>>,
    renderer: Rc<RefCell<Option<RibbonRenderer>>>,
    /// Owns the accent/reduced-motion subscriptions; dropped with the pill,
    /// so no preference callback can outlive it.
    preferences: RefCell<Option<platform::PreferenceWatch>>,
}

impl Pill {
    /// Build the pill and wire its clock, renderer and preference tracking.
    pub fn new() -> Rc<Self> {
        load_css();

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
        // pill instead, so wrapping is what absorbs a long reason.
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
        // The pill's own request is its resting size; the toplevel (or the
        // embedding container) takes it from here.
        pill.set_size_request(PILL_WIDTH, -1);
        pill.append(&icon);
        pill.append(&content);

        let state = Rc::new(RefCell::new(PillState {
            descriptor: crate::states::state_to_descriptor(None, ""),
            notice: NoticeSlot::default(),
            level: None,
            phase: RibbonPhase::Unfold,
            phase_since: Instant::now(),
            started: Instant::now(),
            reduced_motion: platform::probe_reduced_motion(),
            reduced_motion_override: None,
            // No styled widget is rooted yet, so this is the fallback
            // palette; sync_palette() re-resolves from the theme once the
            // ribbon is mapped.
            palette: platform::probe_accent_palette(None::<&gtk::Widget>).as_ribbon_palette(),
            accent: None,
            accent_override: None,
        }));

        let this = Rc::new(Self {
            pill,
            icon,
            label,
            ribbon,
            state,
            renderer: Rc::default(),
            preferences: RefCell::new(None),
        });

        this.connect_palette();
        this.connect_renderer();
        this.connect_clock();
        this.connect_preferences();
        this.apply_descriptor(crate::states::state_to_descriptor(None, ""));
        this
    }

    /// The pill's root widget, to embed in a window or another container.
    pub fn widget(&self) -> &gtk::Box {
        &self.pill
    }

    /// The ribbon area, for the window to read its allocation (input region).
    pub fn ribbon(&self) -> &gtk::GLArea {
        &self.ribbon
    }

    // ── State in ────────────────────────────────────────────────────────

    /// Apply a state descriptor: label, icon, colour class, ribbon phase,
    /// held notice, and visibility. Positioning/input-region are the
    /// window's concern (its opacity is bound to the pill's visibility).
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

        // The ribbon is hidden when a critical error collapses it OR when
        // the whole pill is hidden at idle — the latter matters because the
        // frame clock only queues a render while the ribbon is visible, so
        // hiding it here is what makes idle cost no GPU.
        self.ribbon
            .set_visible(!descriptor.hidden && ribbon_visible_for_severity(descriptor.severity));

        // Nothing is shown at idle (FR-002/X3) — push-to-talk means the
        // resting state is an absent HUD, not an empty one. The pill keeps
        // its footprint (so the overlay window stays a stable size for the
        // host); it is the WINDOW's opacity that makes it vanish — see
        // `HudWindow::apply_descriptor` — and in the embedded lab preview
        // there is no window, so the pill is simply left empty at idle.
        // Either way the ribbon above is hidden, so nothing draws.

        // Announce the change to assistive technology: the status text is
        // the accessible description, and it is content-free by contract.
        self.pill
            .update_property(&[gtk::accessible::Property::Label(&descriptor.status_text)]);
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
    /// theme recomputes its style.
    fn connect_palette(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        self.ribbon.connect_map(move |_| {
            if let Some(this) = this.upgrade() {
                this.schedule_accent_resync();
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
    /// ribbon advances in step with the compositor and stops when not
    /// drawing. The tick is attached to the pill widget, so it works whether
    /// the pill is a toplevel's child or embedded.
    fn connect_clock(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        self.pill.add_tick_callback(move |_widget, _clock| {
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
            }

            if this.ribbon.is_visible() {
                this.ribbon.queue_render();
            }
            glib::ControlFlow::Continue
        });
    }

    fn connect_preferences(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        let watch = platform::watch_preferences(move |readiness| {
            let Some(this) = this.upgrade() else { return };
            // Motion comes straight from its own sources, so it is always
            // read now — unless the lab has pinned it.
            {
                let mut state = this.state.borrow_mut();
                if state.reduced_motion_override.is_none() {
                    state.reduced_motion = platform::probe_reduced_motion();
                }
            }

            // The accent is a computed CSS colour, readable immediately only
            // on libadwaita's own notification (it reloads the accent
            // provider before notifying); anything else waits for the next
            // frame.
            match readiness {
                platform::AccentReadiness::Current => this.sync_palette(),
                platform::AccentReadiness::NextFrame => this.schedule_accent_resync(),
            }
        });
        *self.preferences.borrow_mut() = Some(watch);
    }

    /// Force a re-read of the theme's accent at the next frame.
    /// Override the reduced-motion mode (the lab's accessibility toggle).
    /// `None` returns to the desktop preference.
    pub fn set_reduced_motion_override(&self, value: Option<bool>) {
        let mut state = self.state.borrow_mut();
        state.reduced_motion_override = value;
        if let Some(v) = value {
            state.reduced_motion = v;
        } else {
            state.reduced_motion = platform::probe_reduced_motion();
        }
        drop(state);
        self.ribbon.queue_render();
    }

    /// Force the accent to a `#rrggbb` hex (the lab's override). `None`
    /// returns to the desktop accent. libadwaita has no public runtime
    /// accent setter (it is a desktop preference), so the lab forces the
    /// palette directly.
    pub fn set_accent_override(&self, hex: Option<String>) {
        self.state.borrow_mut().accent_override = hex;
        self.sync_palette();
    }

    pub fn resync_accent(self: &Rc<Self>) {
        self.schedule_accent_resync();
    }

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
        let mut state = self.state.borrow_mut();
        // A lab override wins over the desktop entirely.
        let palette = match state.accent_override.as_deref() {
            Some(hex) => crate::accent::resolve_theme_accent_palette(hex_to_rgb(hex)),
            None => platform::probe_accent_palette(Some(&self.ribbon)),
        };
        let accent = palette.main_rgb();
        if state.accent == Some(accent) {
            return;
        }
        state.accent = Some(accent);
        state.palette = palette.as_ribbon_palette();
        drop(state);
        self.ribbon.queue_render();
    }
}

/// Build the current frame's ribbon model from the live state.
fn build_model(state: &PillState) -> crate::ribbon::RibbonModel {
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
        reduced_motion: state
            .reduced_motion_override
            .unwrap_or(state.reduced_motion),
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
