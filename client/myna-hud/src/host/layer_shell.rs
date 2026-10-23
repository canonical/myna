//! The layer-shell status surface host (`myna-hud --host layer-shell`): the
//! HUD as a `zwlr_layer_shell_v1` surface on compositors that offer it, such
//! as KWin.
//!
//! The compositor does the placement `myna_platform::status_surface`
//! describes: anchored to the bottom edge alone, the surface is centred
//! along it, and an exclusive zone of 0 keeps it clear of the panels' zones,
//! which is the work area. Only the margin is ours. No output is named, so
//! the compositor picks one: a Wayland client cannot see which has focus.
//!
//! The overlay layer stacks above panels and fullscreen windows, keyboard
//! interactivity none means the surface is never given focus, and the
//! window's own empty input region makes it click-through.
//!
//! gtk4-layer-shell works by standing in for libwayland-client's symbols, so
//! it must load first. It does as long as the binary needs it directly and
//! libwayland-client only through GTK; the e2e test fails otherwise.

use std::rc::Rc;

use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use myna_platform::status_surface::BOTTOM_MARGIN;

use crate::window::HudWindow;

/// The surface's namespace, for compositor rules.
pub const NAMESPACE: &str = "myna-hud";

/// Whether the compositor offers `zwlr_layer_shell_v1`.
pub fn supported() -> bool {
    gtk4_layer_shell::is_supported()
}

/// Make `hud`'s window a layer surface. Before it is realized: the role is
/// chosen when the surface is created.
pub fn install(hud: &Rc<HudWindow>) {
    let window = hud.window();
    window.init_layer_shell();
    window.set_namespace(Some(NAMESPACE));
    window.set_layer(Layer::Overlay);
    window.set_anchor(Edge::Bottom, true);
    window.set_margin(Edge::Bottom, BOTTOM_MARGIN);
    window.set_keyboard_mode(KeyboardMode::None);
    window.set_exclusive_zone(0);
}
