//! pill — the HUD pill widget (feature 004), extracted from the window so it
//! can live either in a borderless overlay toplevel ([`crate::window`]) or
//! embedded inside another window (the `--serve-dbus` publisher's preview).
//!
//! It owns the widget tree (mic icon, status label, level indicators), the
//! live state, the frame clock, and the motion/contrast subscriptions. Every
//! *decision* is delegated to the pure modules ([`crate::states`],
//! [`crate::hud_logic`], [`crate::vumeter`], [`crate::notice_slot`]); this
//! owns only widgets and their wiring.
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

use crate::bar::BarView;
use crate::hud_logic::{
    icon_for_severity, indicator_visible_for_severity, pill_color_class, HudStyle,
    PILL_COLOR_CLASSES,
};
use crate::notice_slot::NoticeSlot;
use crate::platform;
use crate::segmented_meter::SegmentedMeterView;
use crate::states::Descriptor;

/// The pill's resting width: a floor just above the natural content width
/// (icon + the bar's own 160px minimum) for a one-line status, so the pill
/// hugs its content the way GNOME's OSD does. A longer status grows the
/// pill up to [`LABEL_MAX_CHARS`], then wraps.
pub const PILL_WIDTH: i32 = 240;

/// The label's wrap width, in characters: a status up to this long stays on
/// one line and the pill grows to fit it; a longer one wraps at it.
///
/// GTK offers no pixel maximum for a widget, and the alternatives do not
/// work here (all measured): `AdwClamp` bounds the child's *allocation*, not
/// the window's natural size, so the window grew to 700px; a
/// `GtkScrolledWindow` hands its child unlimited width, so the label stops
/// wrapping altogether and the window reached 1280px. `max-width-chars` is
/// the lever that does bound a wrapping label.
pub const LABEL_MAX_CHARS: i32 = 30;

/// The pill's resting height with the default (`bar`) indicator: padding,
/// one label line, the gap, and the 6px bar. A floor, like [`PILL_WIDTH`].
pub const PILL_HEIGHT: i32 = 58;

/// The CSS class that brightens the pill under a high-contrast preference
/// (FR-022), defined in `style.css`.
pub const HIGH_CONTRAST_CLASS: &str = "myna-hud-high-contrast";

/// The mutable state the frame clock reads.
struct PillState {
    descriptor: Descriptor,
    notice: NoticeSlot,
    started: Instant,
    reduced_motion: bool,
    #[cfg(dev_lab)]
    /// Lab override: when `Some`, replaces the desktop-derived
    /// `reduced_motion` and is not clobbered by a live preference change.
    reduced_motion_override: Option<bool>,
    /// The HUD's audio-level presentation: the accent bar or the classic
    /// segmented meter. Pushed by the publisher over `HudStyle`, never read
    /// from settings here — see `myna_desktop::dbus::hud_style`.
    hud_style: HudStyle,
    #[cfg(dev_lab)]
    /// Lab override: when `Some`, replaces the published `hud_style` and is
    /// not clobbered by a publisher push, like `reduced_motion_override`.
    hud_style_override: Option<HudStyle>,
    #[cfg(dev_lab)]
    /// Lab override: when `Some`, forces high-contrast on/off instead of the
    /// desktop's `gtk-interface-contrast` / `Adw.StyleManager:high-contrast`.
    /// Like reduced-motion, this survives live preference changes while set.
    #[cfg(dev_lab)]
    high_contrast_override: Option<bool>,
}

/// The HUD pill: a `gtk::Box` styled `.myna-hud-pill`, self-driving.
pub struct Pill {
    pill: gtk::Box,
    icon: gtk::Image,
    label: gtk::Label,
    bar: Rc<BarView>,
    meter: Rc<SegmentedMeterView>,
    state: Rc<RefCell<PillState>>,
    /// Owns the reduced-motion/contrast subscriptions; dropped with the pill,
    /// so no preference callback can outlive it.
    preferences: RefCell<Option<platform::PreferenceWatch>>,
    /// The lab accent-override provider: sets the bar accent to the override
    /// hex. `None` when unset, or cleared.
    #[cfg(dev_lab)]
    accent_override_css: RefCell<Option<gtk::CssProvider>>,
}

impl Pill {
    /// Build the pill and wire its clock and preference tracking.
    pub fn new() -> Rc<Self> {
        load_css();

        let icon = gtk::Image::from_icon_name("audio-input-microphone-symbolic");
        icon.add_css_class("myna-hud-icon");
        icon.set_pixel_size(24);
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

        let bar = BarView::new();
        let meter = SegmentedMeterView::new();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_hexpand(true);
        // A notice or error hides the indicator, leaving the label alone in a
        // box that would otherwise pack it against the top edge.
        content.set_valign(gtk::Align::Center);
        content.append(&label);
        content.append(bar.widget());
        content.append(meter.widget());

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
            started: Instant::now(),
            reduced_motion: platform::probe_reduced_motion(),
            #[cfg(dev_lab)]
            reduced_motion_override: None,
            // The default until the publisher says otherwise: nothing is
            // drawn before the first bus event anyway, so no wrong meter is
            // ever visible.
            hud_style: HudStyle::default(),
            #[cfg(dev_lab)]
            hud_style_override: None,
            #[cfg(dev_lab)]
            high_contrast_override: None,
        }));

        let this = Rc::new(Self {
            pill,
            icon,
            label,
            bar,
            meter,
            state,
            preferences: RefCell::new(None),
            #[cfg(dev_lab)]
            accent_override_css: RefCell::new(None),
        });

        this.connect_clock();
        this.connect_preferences();
        this.sync_high_contrast();
        this.push_reduced_motion();
        this.apply_descriptor(crate::states::state_to_descriptor(None, ""));
        this
    }

    /// The pill's root widget, to embed in a window or another container.
    pub fn widget(&self) -> &gtk::Box {
        &self.pill
    }

    /// The segmented meter, for the window to read its allocation when the
    /// `vumeter` hud-style is active (input region).
    pub fn meter(&self) -> &gtk::Widget {
        self.meter.widget()
    }

    /// The accent level bar, for the window to read its allocation when the
    /// `bar` hud-style is active (input region).
    pub fn bar(&self) -> &gtk::Widget {
        self.bar.widget()
    }

    /// Current descriptor (for lab sync when the HUD auto-dismisses locally).
    pub fn current_descriptor(&self) -> crate::states::Descriptor {
        self.state.borrow().descriptor.clone()
    }

    /// Current wire state string (e.g. "idle", "notice", "error") for lab sync.
    pub fn current_wire_state(&self) -> String {
        let d = self.state.borrow().descriptor.clone();
        // Reverse of `states::state_to_descriptor` — only needed for lab sync
        // where the HUD may have returned to idle locally without a bus
        // publish (notifier-side timeout).
        match d.key {
            crate::states::DictationState::Idle => crate::states::wire::IDLE.to_string(),
            crate::states::DictationState::Loading => crate::states::wire::LOADING.to_string(),
            crate::states::DictationState::Recording => crate::states::wire::RECORDING.to_string(),
            crate::states::DictationState::Transcribing => {
                crate::states::wire::TRANSCRIBING.to_string()
            }
            crate::states::DictationState::Finalizing => {
                crate::states::wire::FINALIZING.to_string()
            }
            crate::states::DictationState::Notice => crate::states::wire::NOTICE.to_string(),
            crate::states::DictationState::Error => crate::states::wire::ERROR.to_string(),
            crate::states::DictationState::Active => "active".to_string(),
        }
    }

    // ── State in ────────────────────────────────────────────────────────

    /// Apply a state descriptor: label, icon, colour class, held notice, and
    /// visibility. Positioning/input-region are the
    /// window's concern (its opacity is bound to the pill's visibility).
    pub fn apply_descriptor(self: &Rc<Self>, descriptor: Descriptor) {
        let now = self.now_ms();
        // Server auto-dismisses `notice` after its own longer hold. Client
        // keeps showing for its own minimum (slower reading) and ignores
        // server `idle` until that minimum completes, unless a new
        // non-idle state arrives (which replaces immediately).
        if descriptor.hidden {
            let (is_showing, expires_at) = {
                let state = self.state.borrow();
                (state.notice.is_showing(now), state.notice.expires_at())
            };
            if is_showing {
                if let Some(expires_at) = expires_at {
                    // Still within client's minimum for a `notice` — keep
                    // showing and schedule the idle for the remaining time.
                    let remaining = (expires_at - now).max(0.0) as u64;
                    let this_weak = Rc::downgrade(self);
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(remaining),
                        move || {
                            if let Some(this) = this_weak.upgrade() {
                                // Only go idle if still the same notice (no
                                // replacement in the meantime).
                                let still = {
                                    let s = this.state.borrow();
                                    s.notice.expires_at() == Some(expires_at)
                                        && s.notice.is_showing(this.now_ms())
                                };
                                if still {
                                    this.apply_descriptor(crate::states::state_to_descriptor(
                                        None, "",
                                    ));
                                    if let Some(root) = this.pill.root() {
                                        if let Some(window) = root.downcast_ref::<gtk::Window>() {
                                            if window.has_css_class("myna-hud-window") {
                                                // The same fade as the
                                                // window's own hide path.
                                                this.pill.add_css_class(
                                                    crate::window::FADE_HIDDEN_CLASS,
                                                );
                                                let window = window.clone();
                                                glib::timeout_add_local_once(
                                                    std::time::Duration::from_millis(
                                                        crate::window::FADE_MS,
                                                    ),
                                                    move || window.set_visible(false),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        },
                    );
                }
                return;
            }
        }
        {
            let mut state = self.state.borrow_mut();
            if descriptor.severity.is_some() {
                state
                    .notice
                    .hold(descriptor.severity, &descriptor.status_text, now);
            } else {
                state.notice.clear();
            }
            state.descriptor = descriptor.clone();
        }

        // Drive the views' state animations (loading pulse, finalize
        // settle) from the same descriptor.
        self.bar.set_state(descriptor.key, descriptor.severity);
        self.meter.set_state(descriptor.key, descriptor.severity);

        self.label.set_text(&descriptor.status_text);
        self.icon
            .set_icon_name(Some(icon_for_severity(descriptor.severity)));

        for class in PILL_COLOR_CLASSES {
            self.pill.remove_css_class(class);
        }
        if let Some(class) = pill_color_class(descriptor.key, descriptor.severity) {
            self.pill.add_css_class(class);
        }

        // The indicator is hidden for a notice or error OR when
        // the whole pill is hidden at idle — the latter matters because the
        // frame clock only queues a redraw while the indicator is visible, so
        // hiding it here is what makes idle cost nothing. Which indicator is
        // shown follows the `hud-style` setting (bar/vumeter).
        let visible = !descriptor.hidden && indicator_visible_for_severity(descriptor.severity);
        let style = {
            let state = self.state.borrow();
            state.hud_style
        };
        self.meter
            .widget()
            .set_visible(visible && style == HudStyle::Vumeter);
        self.bar
            .widget()
            .set_visible(visible && style == HudStyle::Bar);

        // Nothing is shown at idle (FR-002/X3) — push-to-talk means the
        // resting state is an absent HUD, not an empty one. The pill keeps
        // its footprint (so the overlay window stays a stable size for the
        // host); it is the WINDOW's opacity that makes it vanish — see
        // `HudWindow::apply_descriptor` — and in the embedded lab preview
        // there is no window, so the pill is simply left empty at idle.
        // Either way the indicator above is hidden, so nothing draws.

        // Announce the change to assistive technology: the status text is
        // the accessible description, and it is content-free by contract.
        self.pill
            .update_property(&[gtk::accessible::Property::Label(&descriptor.status_text)]);
        // Live announcement (GTK 4.14+): `Label` alone is only spoken on
        // focus, but the HUD is deliberately non-focusable chrome
        // (`window.set_can_focus(false)`, DOCK, empty input region).  The
        // explicit announcement is the AT-SPI live-region signal Orca
        // speaks even without focus.  Only for `notice`/`error` (severity
        // present) — the continuous `Recording`/`Finalizing` states would
        // spam.
        if descriptor.severity.is_some() && !descriptor.status_text.is_empty() {
            self.pill.announce(
                &descriptor.status_text,
                gtk::AccessibleAnnouncementPriority::Medium,
            );
        }
    }

    /// A level push from the publisher. Never deduplicated — the arrival
    /// time is what keeps a steady voice from decaying (R16a).
    pub fn push_level(&self, rms: f64, peak: f64) {
        self.bar.push_level(rms, peak);
        self.meter.push_level(rms, peak);
    }

    fn now_ms(&self) -> f64 {
        self.state.borrow().started.elapsed().as_secs_f64() * 1000.0
    }

    // ── Wiring ──────────────────────────────────────────────────────────

    /// Drive the animation from the frame clock rather than a timer, so the
    /// indicator advances in step with the compositor and stops when not
    /// drawing. The tick is attached to the pill widget, so it works whether
    /// the pill is a toplevel's child or embedded.
    fn connect_clock(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        self.pill.add_tick_callback(move |_widget, _clock| {
            let Some(this) = this.upgrade() else {
                return glib::ControlFlow::Break;
            };
            // Queue whichever indicator is currently visible so a hidden one
            // costs no redraw.
            if this.meter.widget().is_visible() {
                this.meter.queue_draw();
            }
            if this.bar.widget().is_visible() {
                this.bar.queue_draw();
            }
            glib::ControlFlow::Continue
        });
    }

    fn connect_preferences(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        let watch = platform::watch_preferences(move |_| {
            let Some(this) = this.upgrade() else { return };
            // Motion comes straight from its own sources, so it is always
            // read now — unless the lab has pinned it.
            #[cfg(dev_lab)]
            {
                let mut state = this.state.borrow_mut();
                if state.reduced_motion_override.is_none() {
                    state.reduced_motion = platform::probe_reduced_motion();
                }
            }
            #[cfg(not(dev_lab))]
            {
                this.state.borrow_mut().reduced_motion = platform::probe_reduced_motion();
            }
            // Recompute the views' pulse pace.
            this.push_reduced_motion();

            // High contrast is a plain setting too — unless the lab has
            // pinned it, re-read the desktop preference live.
            // (The override survives while set, like reduced-motion.)
            #[cfg(dev_lab)]
            let high_changed = {
                let state = this.state.borrow();
                state.high_contrast_override.is_none()
            };
            #[cfg(not(dev_lab))]
            let high_changed = true;
            if high_changed {
                this.sync_high_contrast_inner(None);
            }
        });
        *self.preferences.borrow_mut() = Some(watch);
    }

    /// Apply/remove the high-contrast CSS class from `GtkSettings:gtk-interface-contrast`
    /// (FR-022). The class brightens the pill's border and background so it
    /// stays legible against any wallpaper; severity also carries an icon
    /// change (never colour-only — FR-007's icon/`microphone-disabled` for a
    /// critical error), so contrast mode never reduces legibility to colour
    /// alone.
    fn sync_high_contrast(&self) {
        self.sync_high_contrast_inner(None);
    }

    fn sync_high_contrast_inner(&self, forced: Option<bool>) {
        let high = if let Some(v) = forced {
            v
        } else {
            #[cfg(dev_lab)]
            {
                let state = self.state.borrow();
                if let Some(v) = state.high_contrast_override {
                    v
                } else {
                    platform::probe_high_contrast()
                }
            }
            #[cfg(not(dev_lab))]
            {
                platform::probe_high_contrast()
            }
        };
        if high {
            self.pill.add_css_class(HIGH_CONTRAST_CLASS);
        } else {
            self.pill.remove_css_class(HIGH_CONTRAST_CLASS);
        }
    }

    /// Forward the current reduce-animation preference to the views so they travel at the right pace (slower under reduced motion).
    fn push_reduced_motion(&self) {
        let reduced = {
            let state = self.state.borrow();
            state.reduced_motion
        };
        self.bar.set_reduced_motion(reduced);
        self.meter.set_reduced_motion(reduced);
    }

    /// Switch the audio-level presentation: the accent level bar or the
    /// classic segmented meter.
    ///
    /// The value arrives from the publisher's `HudStyle` property; the HUD
    /// reads no settings store of its own.
    pub fn set_hud_style(self: &Rc<Self>, style: HudStyle) {
        {
            let mut state = self.state.borrow_mut();
            // A lab override outranks the publisher and survives its pushes,
            // exactly like the reduced-motion and high-contrast overrides.
            #[cfg(dev_lab)]
            if state.hud_style_override.is_some() {
                return;
            }
            if state.hud_style == style {
                return;
            }
            state.hud_style = style;
        }
        // Re-assert the visibility split so the newly shown indicator
        // reflects the current state immediately (not just at the next
        // descriptor).
        self.apply_descriptor(self.current_descriptor());
    }

    /// Override the indicator style for lab previewing. `None` releases the
    /// override; the next publisher push (or the current value until then)
    /// takes over again.
    #[cfg(dev_lab)]
    pub fn set_hud_style_override(self: &Rc<Self>, style: Option<HudStyle>) {
        {
            let mut state = self.state.borrow_mut();
            state.hud_style_override = style;
            match style {
                Some(style) if state.hud_style != style => state.hud_style = style,
                _ => return,
            }
        }
        self.apply_descriptor(self.current_descriptor());
    }

    #[cfg(dev_lab)]
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
        self.push_reduced_motion();
    }

    /// Force the bar's accent to a `#rrggbb` hex (the lab's override).
    /// `None` returns to the desktop accent. libadwaita has no public runtime
    /// accent setter (it is a desktop preference), so the lab injects a
    /// high-priority CSS rule on the bar's colour instead.
    #[cfg(dev_lab)]
    pub fn set_accent_override(&self, hex: Option<String>) {
        let display = match gdk::Display::default() {
            Some(d) => d,
            None => {
                *self.accent_override_css.borrow_mut() = None;
                return;
            }
        };
        let provider = gtk::CssProvider::new();
        match hex.as_deref() {
            Some(hex) => {
                let css = format!(".myna-hud-bar {{ color: {hex}; }}");
                provider.load_from_string(&css);
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_USER,
                );
                *self.accent_override_css.borrow_mut() = Some(provider);
            }
            None => {
                // Drop the old provider (removes its rules from the display).
                *self.accent_override_css.borrow_mut() = None;
            }
        }
        // Force the bar to re-resolve its CSS colour.
        self.bar.queue_draw();
    }

    /// Force high-contrast on/off for the lab; `None` returns to the desktop
    /// preference (GtkSettings `gtk-interface-contrast` with libadwaita
    /// `high-contrast` fallback). Works on any libadwaita 1.x — the
    /// `high-contrast` property existed before `gtk-interface-contrast`.
    #[cfg(dev_lab)]
    pub fn set_high_contrast_override(&self, value: Option<bool>) {
        self.state.borrow_mut().high_contrast_override = value;
        self.sync_high_contrast_inner(value);
    }
}

/// Install the pill's stylesheet once per display.
fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
