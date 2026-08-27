// examples/state_cycle.rs — walk the HUD through a realistic session:
// idle (nothing shown) -> loading -> recording -> transcribing -> idle.
//
// The idle->active transition is the one the single-state gallery never
// exercises, and it is where the window has to grow from nothing to the
// pill's size.
//
//   xvfb-run -a cargo run -p myna-hud --example state_cycle

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use myna_hud::states::{state_to_descriptor, wire};
use myna_hud::window::HudWindow;

fn main() {
    let app = adw::Application::builder()
        .application_id("org.myna.StateCycle")
        .build();
    app.connect_activate(|app| {
        let hud = HudWindow::new(app);
        hud.window().present();
        hud.apply_descriptor(state_to_descriptor(Some(wire::IDLE), ""));

        let steps = [
            wire::LOADING,
            wire::RECORDING,
            wire::TRANSCRIBING,
            wire::IDLE,
            wire::RECORDING,
        ];
        for (i, state) in steps.into_iter().enumerate() {
            let hud = hud.clone();
            gtk::glib::timeout_add_local_once(
                std::time::Duration::from_millis(800 * (i as u64 + 1)),
                move || {
                    println!("state-cycle: {state}");
                    hud.apply_descriptor(state_to_descriptor(Some(state), ""));
                    hud.push_level(0.4, 0.7);
                },
            );
        }
    });
    app.run_with_args::<&str>(&[]);
}
