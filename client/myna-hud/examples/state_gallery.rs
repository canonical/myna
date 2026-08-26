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
        .application_id("org.myna.StateGallery")
        .build();
    app.connect_activate(move |app| {
        let hud = HudWindow::new(app);
        hud.apply_descriptor(state_to_descriptor(Some(&state), &reason));
        hud.push_level(0.35, 0.6);
        hud.window().present();
    });
    app.run_with_args::<&str>(&[]);
}
