//! Status surface hosts: what places and protects the HUD window where no
//! desktop shell does it for us.
//!
//! On GNOME the `myna-shell` extension is the host and this crate has none.
//! Elsewhere the HUD hosts itself, as `--host` names: [`x11`] on X11 window
//! managers, [`layer_shell`] on Wayland compositors with wlr-layer-shell.

pub mod ewmh;
pub mod layer_shell;
pub mod x11;

use std::rc::Rc;

use myna_platform::SessionKind;

use crate::window::HudWindow;

/// A host's hook into the window's idle unmap/remap cycle.
pub trait Host {
    /// The window is realized and unmapped, and is about to map.
    fn before_map(&self);
}

/// A host the HUD can be, by its `--host` name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    X11,
    LayerShell,
}

impl Kind {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "x11" => Some(Self::X11),
            "layer-shell" => Some(Self::LayerShell),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::X11 => "x11",
            Self::LayerShell => "layer-shell",
        }
    }

    /// The session the host runs in, and the GDK backend pinned for it.
    pub fn session(self) -> (SessionKind, &'static str) {
        match self {
            Self::X11 => (SessionKind::X11, "x11"),
            Self::LayerShell => (SessionKind::Wayland, "wayland"),
        }
    }

    /// Whether the display GTK opened can host the HUD. Needs GTK up.
    pub fn display_supports(self) -> bool {
        match self {
            Self::X11 => true,
            Self::LayerShell => layer_shell::supported(),
        }
    }

    /// Take over `hud`'s window, before it is first shown.
    pub fn install(self, hud: &Rc<HudWindow>) -> Result<(), String> {
        match self {
            Self::X11 => x11::X11Host::install(hud)
                .map(drop)
                .map_err(|e| e.to_string()),
            Self::LayerShell => {
                layer_shell::install(hud);
                Ok(())
            }
        }
    }
}
