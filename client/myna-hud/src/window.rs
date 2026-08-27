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
//! (R21) — a renderer that also positioned itself would fight its host. In
//! [`present_standalone`](HudWindow::present_standalone) (lab) mode there is
//! no host, so it presents as an ordinary window.
//!
//! ## Click-through (R22/T114)
//!
//! The surface's input region is empty in **every** state, so pointer
//! events always reach whatever is underneath — the HUD is an overlay, not
//! a target, and carries no interactive control at all.

use std::rc::Rc;

use gdk4_x11 as gdkx11;
use gtk::cairo;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::input_region::input_region_rects;
use crate::pill::{Pill, PILL_WIDTH, RIBBON_HEIGHT};
use crate::states::Descriptor;

/// Object-data key under which the window owns its [`HudWindow`].
const SELF_KEY: &str = "myna-hud-instance";

/// The window's resting height: the pill's natural height for a one-line
/// status (ribbon + label + padding). A stable floor so the mapped window
/// does not collapse when the pill hides at idle.
const RESTING_HEIGHT: i32 = RIBBON_HEIGHT + 44;

/// The HUD pill overlay window.
pub struct HudWindow {
    window: gtk::ApplicationWindow,
    pill: Rc<Pill>,
}

impl HudWindow {
    /// Build the overlay window around a fresh pill.
    pub fn new(app: &adw::Application) -> Rc<Self> {
        let pill = Pill::new();

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
        // The window stays mapped for the renderer's whole life (idle just
        // hides the pill inside it), so the host adopts it exactly once. A
        // fixed default size keeps it from collapsing to GTK's 200x200
        // fallback when the pill is hidden at idle — which would resize the
        // adopted surface under the host and squeeze the ribbon below its
        // 160px minimum on the way back. The pill grows the window past this
        // for a wrapped error; this is only the resting/idle floor.
        window.set_default_size(PILL_WIDTH, RESTING_HEIGHT);
        // The HUD must never take focus from the app being dictated into.
        // The host also enforces this by DOCK-typing the window (mutter
        // forces takes_focus = FALSE), but a renderer that asked for focus
        // would still steal it in lab mode.
        window.set_can_focus(false);

        // The pill lives inside a fixed-size holder rather than being the
        // window's direct child, so the mapped window keeps a stable size
        // even when the pill hides (the host adopts the window once and must
        // not have it resized under it — see the never-unmap design).
        let holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        holder.set_size_request(PILL_WIDTH, RESTING_HEIGHT);
        holder.append(pill.widget());
        window.set_child(Some(&holder));

        // The window's OPACITY is the single source of truth for "is the HUD
        // showing". The pill widget's visibility is *bound* to it: when the
        // overlay is hidden at idle (opacity 0) the pill — and therefore its
        // ribbon — is made invisible, which stops the frame clock queuing
        // renders, so idle costs no GPU. Hiding via opacity (not
        // `window.set_visible(false)`) keeps the surface mapped for the
        // host; the binding then removes the manual visibility bookkeeping.
        window
            .bind_property("opacity", pill.widget(), "visible")
            .transform_to(|_, opacity: f64| Some(opacity > 0.0))
            .sync_create()
            .build();

        let hud = Rc::new(Self { window, pill });

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

    /// The underlying window, for the application to present.
    pub fn window(&self) -> &gtk::ApplicationWindow {
        &self.window
    }

    /// Present as an ordinary window (lab mode — no host to adopt us).
    pub fn present_standalone(&self) {
        self.window.present();
    }

    /// Apply a state descriptor to the pill and refresh the input region.
    ///
    /// The window is **never unmapped**, not even at idle. Unmapping would
    /// destroy the Wayland surface, and the host would then have to re-adopt
    /// the fresh window on every return from idle — fragile, and it lost the
    /// window in practice. Instead the pill itself hides (an empty,
    /// transparent, click-through area) while the window stays mapped, so
    /// the host adopts exactly once for the renderer's whole life.
    pub fn apply_descriptor(self: &Rc<Self>, descriptor: Descriptor) {
        let hidden = descriptor.hidden;
        self.pill.apply_descriptor(descriptor);
        // Opacity on the WINDOW is the single control for "shown": it makes
        // the whole surface composite nothing at idle while staying mapped
        // for the host, and the pill's visibility is bound to it (see
        // `new`), so hiding here also stops the ribbon rendering.
        println!("HUD: set_opacity({})", if hidden { 0.0 } else { 1.0 });
        self.window.set_opacity(if hidden { 0.0 } else { 1.0 });
        self.apply_input_region();
    }

    /// A level push from the publisher.
    pub fn push_level(&self, rms: f64, peak: f64) {
        self.pill.push_level(rms, peak);
    }

    /// Force a theme accent re-read (a host may know styling changed).
    pub fn resync_accent(&self) {
        self.pill.resync_accent();
    }

    // ── Overlay concerns ────────────────────────────────────────────────

    /// Ask an X11 window manager to keep the overlay out of the taskbar and
    /// the pager. Redundant on the Wayland shipping path (the host handles
    /// it); this is the lab/fallback case. There is no GDK4 always-on-top
    /// equivalent — stacking is the host's job on Wayland.
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

    /// Re-apply the (empty) input region whenever the ribbon maps, since the
    /// toolkit can reset the surface's input region across a map.
    fn reapply_input_region_on_map(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        self.pill.ribbon().connect_map(move |_| {
            if let Some(this) = this.upgrade() {
                this.apply_input_region();
            }
        });
    }

    /// Make the surface fully click-through, in every state (R22/FR-025).
    /// The HUD takes no pointer input at all, so the region is empty and
    /// stays empty; a critical error is cleared by the client publishing a
    /// new state, not by clicking the pill.
    fn apply_input_region(&self) {
        let Some(surface) = self.window.surface() else {
            return;
        };
        let rects = input_region_rects(None);
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
