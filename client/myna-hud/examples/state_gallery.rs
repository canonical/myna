// examples/state_gallery.rs — present the HUD in one given state so the
// severity treatments (amber notice, red error + dismiss control, the warm
// loading tint) can be eyeballed. Replaces the extension's
// `entrance-visual.sh`.
//
//   xvfb-run -a cargo run -p myna-hud --example state_gallery -- error

use libadwaita as adw;
use libadwaita::prelude::*;
use myna_hud::states::state_to_descriptor;
use myna_hud::window::HudWindow;

fn main() {
    let state = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "recording".into());
    let reason = std::env::args().nth(2).unwrap_or_default();
    let app = adw::Application::builder()
        .application_id("com.canonical.Myna.StateGallery")
        .build();
    app.connect_activate(move |app| {
        let hud = HudWindow::new(app);
        // Present first, then apply — the order the running app uses, and
        // the one that lets `idle` actually hide the window rather than
        // being undone by a later present().
        hud.window().present();
        hud.apply_descriptor(state_to_descriptor(Some(&state), &reason));
        hud.push_level(0.35, 0.6);
    });
    app.run_with_args::<&str>(&[]);
}
