//! window — the HUD pill overlay window (feature 004, T112/T114; R21–R23).
//!
//! A borderless, transparent toplevel wrapping a [`crate::pill::Pill`]. This
//! module owns only the *overlay* concerns: the surface's click-through
//! input region, the X11 skip-taskbar/pager hints, the size default, and
//! tying the pill's lifetime to the window. The pill itself owns the widget
//! tree, rendering, clock and preferences.
//!
//! ## What this window deliberately does NOT do
//!
//! It never positions, sizes-to-monitor, raises, or types itself. Under
//! GNOME the `myna-shell` extension launches it through a
//! `Meta.WaylandClient`, adopts the window, makes it a DOCK, and places it
//! (R21) — a renderer that also positioned itself would fight its host.
//! Where the HUD is its own host (`--host x11`), that host does all of it
//! through [`HudWindow::set_host`] and the window's signals. In lab mode
//! there is no host, so it presents as an ordinary window.
//!
//! ## Click-through (R22/T114)
//!
//! The surface's input region is empty in **every** state, so pointer
//! events always reach whatever is underneath — the HUD is an overlay, not
//! a target, and carries no interactive control at all.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gdk4_x11 as gdkx11;
use gtk::cairo;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use myna_platform::Profile;

use crate::host::Host;
use crate::pill::{Pill, PILL_HEIGHT, PILL_WIDTH};
use crate::states::Descriptor;

/// Object-data key under which the window owns its [`HudWindow`].
const SELF_KEY: &str = "myna-hud-instance";

/// The window's resting height: the pill's natural height for a one-line
/// status with the default `bar` indicator. A floor, so the mapped window
/// does not collapse when the pill hides at idle; a taller indicator grows
/// the window past it.
const RESTING_HEIGHT: i32 = PILL_HEIGHT;

/// The show/hide fade, mirroring gnome-shell's `OsdWindow.FADE_TIME`: the
/// pill fades its opacity over this long before the surface unmaps, and
/// fades back in once mapped.
pub(crate) const FADE_MS: u64 = 100;
/// Drives the fade above — `opacity: 0`, transitioned by `style.css`.
pub(crate) const FADE_HIDDEN_CLASS: &str = "myna-hud-fade-hidden";

/// The HUD pill overlay window.
pub struct HudWindow {
    window: gtk::ApplicationWindow,
    pill: Rc<Pill>,
    /// The hidden state most recently requested, so a fade-out reversed
    /// mid-flight (a visible descriptor arrives within `FADE_MS`) does not
    /// go on to unmap a now-visible window.
    pending_hidden: Cell<bool>,
    /// The in-process host, where the HUD hosts itself ([`crate::host`]).
    host: RefCell<Option<Rc<dyn Host>>>,
}

impl HudWindow {
    /// Build the overlay window around a fresh pill.
    pub fn new(app: &adw::Application, profile: Profile) -> Rc<Self> {
        let pill = Pill::new(profile);

        // A plain GtkApplicationWindow, deliberately NOT
        // adw::ApplicationWindow: the libadwaita window imposes a 200 px
        // minimum height (it is built for adaptive app windows), which would
        // leave ~130 px of dead transparent surface below a 66 px pill —
        // surface that still counts as the overlay's extent for the host's
        // placement (R21) and its input region (R22). libadwaita is still
        // used for the style manager's accent (R26); nothing here needs an
        // adw window.
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .resizable(false)
            .decorated(false)
            .build();
        window.add_css_class("myna-hud-window");
        // The default size keeps the window from remapping at GTK's 200x200
        // fallback on a return from idle. `fit_width` widens it for a
        // longer status; this is only the resting floor.
        window.set_default_size(PILL_WIDTH, RESTING_HEIGHT);
        // The HUD must never take focus from the app being dictated into.
        // The host enforces this by DOCK-typing the window at creation,
        // before its first map: mutter's focus-on-map decision refuses
        // focus for DOCK windows, so the pill never steals the target's
        // keyboard focus. A renderer that asked for focus would still steal
        // it in lab mode.
        window.set_can_focus(false);

        // The pill lives inside a fixed-size holder rather than being the
        // window's direct child, so that on a return from idle the window
        // remaps at its resting size instead of GTK's 200x200 fallback,
        // which would resize the adopted surface under the host on every
        // recording→idle→recording cycle.
        let holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        holder.set_size_request(PILL_WIDTH, RESTING_HEIGHT);
        holder.append(pill.widget());
        window.set_child(Some(&holder));

        let hud = Rc::new(Self {
            window,
            pill,
            pending_hidden: Cell::new(false),
            host: RefCell::new(None),
        });

        // Tie our lifetime to the window's: every callback holds a weak
        // reference (a strong one would keep the struct alive through its own
        // widgets forever), so without this the caller's Rc is the only owner
        // and the frame clock would stop the moment it drops while the
        // widgets stay displayed. Released on destroy to break the cycle.
        unsafe {
            hud.window.set_data(SELF_KEY, hud.clone());
        }
        hud.window.connect_destroy(|window| unsafe {
            let _ = window.steal_data::<Rc<HudWindow>>(SELF_KEY);
        });

        hud.connect_x11_hints();
        hud.reapply_input_region_on_map();
        hud
    }

    /// Hand the window's map cycle to an in-process host.
    pub fn set_host(&self, host: Rc<dyn Host>) {
        self.host.replace(Some(host));
    }

    /// The underlying window, for the application to present.
    pub fn window(&self) -> &gtk::ApplicationWindow {
        &self.window
    }

    /// Apply a state descriptor to the pill, then map/unmap the overlay for
    /// the idle transition, and refresh the input region.
    pub fn apply_descriptor(self: &Rc<Self>, descriptor: Descriptor) {
        let hidden = descriptor.hidden;
        self.pill.apply_descriptor(descriptor);
        self.fit_width();
        self.set_hidden_faded(hidden);
        self.apply_input_region();
    }

    /// Size the window to the pill's natural width, never below
    /// [`PILL_WIDTH`]. A non-resizable toplevel takes its default width as
    /// long as that meets the minimum, so without this a headline that
    /// would fit on one line within [`crate::pill::LABEL_MAX_CHARS`] wraps
    /// at the resting width instead.
    fn fit_width(&self) {
        let (_, natural, _, _) = self.pill.widget().measure(gtk::Orientation::Horizontal, -1);
        self.window
            .set_default_size(natural.max(PILL_WIDTH), RESTING_HEIGHT);
    }

    /// Fade the pill to/from `hidden`, mapping or unmapping the surface only
    /// once the fade completes, rather than popping in and out.
    ///
    /// The unmap at the end of a fade-out still has to happen for real: a
    /// compositor is free to ignore surface opacity on an already-mapped
    /// toplevel (mutter does — the window reports opacity 0 but stays fully
    /// shown). `set_visible(false)` unmaps the surface, which the compositor
    /// cannot show. The window stays owned by the same Meta.WaylandClient
    /// across the unmap/remap, and the host re-asserts the overlay on the
    /// `map` signal, so a return from idle is re-adopted rather than lost.
    fn set_hidden_faded(self: &Rc<Self>, hidden: bool) {
        self.pending_hidden.set(hidden);
        let widget = self.pill.widget();
        if hidden {
            if !self.window.is_visible() {
                return; // already unmapped — nothing to fade
            }
            widget.add_css_class(FADE_HIDDEN_CLASS);
            let this = self.clone();
            glib::timeout_add_local_once(Duration::from_millis(FADE_MS), move || {
                // A later, un-hidden descriptor may have arrived mid-fade.
                if this.pending_hidden.get() {
                    this.window.set_visible(false);
                }
            });
        } else if self.window.is_visible() {
            // Already mapped, or a fade-out reversed mid-flight — let the CSS
            // transition take it back to resting opacity.
            widget.remove_css_class(FADE_HIDDEN_CLASS);
        } else {
            // Map at opacity 0, then drop the class on the next frame so the
            // transition actually fades it in rather than popping at 1.
            widget.add_css_class(FADE_HIDDEN_CLASS);
            if let Some(host) = self.host.borrow().as_ref() {
                host.before_map();
            }
            self.window.set_visible(true);
            let widget = widget.clone();
            glib::idle_add_local_once(move || {
                widget.remove_css_class(FADE_HIDDEN_CLASS);
            });
        }
    }

    /// A level push from the publisher.
    pub fn push_level(&self, rms: f64, peak: f64) {
        self.pill.push_level(rms, peak);
    }

    /// Force a theme accent re-read (a host may know styling changed).
    pub fn resync_accent(&self) {
        self.pill.resync_accent();
    }

    /// Override reduced-motion for lab testing; `None` returns to the desktop.
    #[cfg(dev_lab)]
    pub fn set_reduced_motion_override(&self, value: Option<bool>) {
        self.pill.set_reduced_motion_override(value);
    }

    /// Set the HUD indicator style, as published on `HudStyle`.
    pub fn set_hud_style(&self, style: crate::hud_logic::HudStyle) {
        self.pill.set_hud_style(style);
    }

    /// Override the HUD indicator style for lab testing; `None` returns to
    /// whatever the publisher last sent.
    #[cfg(dev_lab)]
    pub fn set_hud_style_override(&self, style: Option<crate::hud_logic::HudStyle>) {
        self.pill.set_hud_style_override(style);
    }

    /// Force the accent hex (lab override); `None` returns to the desktop.
    #[cfg(dev_lab)]
    pub fn set_accent_override(&self, hex: Option<String>) {
        self.pill.set_accent_override(hex);
    }

    /// Force high-contrast for the lab; `None` returns to the desktop.
    #[cfg(dev_lab)]
    pub fn set_high_contrast_override(&self, value: Option<bool>) {
        self.pill.set_high_contrast_override(value);
    }

    /// Current wire state (for lab sync when HUD auto-dismisses locally).
    pub fn current_wire_state(&self) -> String {
        self.pill.current_wire_state()
    }

    // ── Overlay concerns ────────────────────────────────────────────────

    /// Ask an X11 window manager to keep the overlay out of the taskbar and
    /// the pager. Reached on any X11 session: lab mode, and the X11 host,
    /// which relies on it. There is no GDK4 always-on-top equivalent —
    /// stacking is the host's job.
    fn connect_x11_hints(self: &Rc<Self>) {
        self.window.connect_realize(|window| {
            let Some(surface) = window.surface() else {
                return;
            };
            if let Some(x11) = surface.downcast_ref::<gdkx11::X11Surface>() {
                x11.set_skip_taskbar_hint(true);
                x11.set_skip_pager_hint(true);
            }
        });
    }

    /// Re-apply the (empty) input region whenever the indicator maps, since
    /// the toolkit can reset the surface's input region across a map. Hooked
    /// on all available indicators (ribbon + segmented meter + accent bar) so
    /// the correct one's map is observed whichever `hud-style` is active.
    fn reapply_input_region_on_map(self: &Rc<Self>) {
        let on_map = {
            let this = Rc::downgrade(self);
            move |_: &gtk::Widget| {
                if let Some(this) = this.upgrade() {
                    this.apply_input_region();
                }
            }
        };
        let ribbon: &gtk::Widget = self.pill.ribbon().upcast_ref();
        let meter: &gtk::Widget = self.pill.meter().upcast_ref();
        let bar: &gtk::Widget = self.pill.bar().upcast_ref();
        ribbon.connect_map(on_map.clone());
        meter.connect_map(on_map.clone());
        bar.connect_map(on_map);
    }

    /// Make the surface fully click-through, in every state (R22/FR-025):
    /// an empty input region. The HUD takes no pointer input at all; a
    /// critical error is cleared by the client publishing a new state, not by
    /// clicking the pill.
    fn apply_input_region(&self) {
        if let Some(surface) = self.window.surface() {
            surface.set_input_region(&cairo::Region::create());
        }
    }
}
