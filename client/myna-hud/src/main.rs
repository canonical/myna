//! `myna-hud` — the dictation HUD renderer (feature 004, T124).
//!
//! Three modes, one binary:
//!
//! * **hosted** (default) — consume `org.myna.Dictation` and render the
//!   pill. Under GNOME the `myna-shell` extension launches this through a
//!   `Meta.WaylandClient` and owns the window's placement (R21); elsewhere
//!   it is an ordinary always-on-top window.
//! * `--lab` — the development lab: manual controls driving the identical
//!   renderer modules with no backend at all.
//! * `--serve-dbus` — publish a simulated `org.myna.Dictation` so the real
//!   hosted path can be exercised without the Python daemon.
//!
//! GTK owns the main thread; the bus worker talks to it over a channel.

use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use myna_hud::bus::{self, BusEvent};
use myna_hud::dbus_consumer::DictationService;
use myna_hud::states::state_to_descriptor;
use myna_hud::window::HudWindow;

const APP_ID: &str = "org.myna.Hud";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Hosted,
    Lab,
    ServeDbus,
}

fn main() -> glib::ExitCode {
    let mode = match parse_mode() {
        Ok(mode) => mode,
        Err(message) => {
            eprintln!("{message}");
            return glib::ExitCode::FAILURE;
        }
    };

    let app = adw::Application::builder()
        .application_id(APP_ID)
        // The modes are dispatched here, not through GApplication's own
        // option parsing, so `--lab` cannot be forwarded to a running
        // instance and silently change its behaviour.
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_activate(move |app| match mode {
        Mode::Hosted => activate_hosted(app),
        Mode::Lab => activate_lab(app),
        Mode::ServeDbus => activate_serve_dbus(app),
    });

    // Our own argv is already consumed above.
    app.run_with_args::<&str>(&[])
}

fn parse_mode() -> Result<Mode, String> {
    let mut mode = Mode::Hosted;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--lab" => mode = Mode::Lab,
            "--serve-dbus" => mode = Mode::ServeDbus,
            "--version" => {
                println!("myna-hud {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("myna-hud: unknown option {other}\n\n{USAGE}")),
        }
    }
    Ok(mode)
}

const USAGE: &str = "\
Usage: myna-hud [OPTION]

The myna dictation HUD renderer.

  (no option)    consume org.myna.Dictation and render the HUD
  --lab          development lab: manual controls, no backend
  --serve-dbus   publish a simulated org.myna.Dictation
  --version      print the version and exit
  -h, --help     print this help and exit";

/// The shipping path: render whatever the publisher reports.
fn activate_hosted(app: &adw::Application) {
    let hud = HudWindow::new(app);
    hud.window().present();

    let (sender, receiver) = async_channel::unbounded::<BusEvent>();
    bus::spawn(sender);

    // The consumer's rules run on the main thread beside the widgets, so a
    // state change and its redraw can never interleave.
    let hud_for_events = hud.clone();
    glib::spawn_future_local(async move {
        let mut service = DictationService::builder()
            .on_state_changed({
                let hud = hud_for_events.clone();
                move |state, error| {
                    hud.apply_descriptor(state_to_descriptor(Some(state), error));
                }
            })
            .on_level({
                let hud = hud_for_events.clone();
                move |rms, peak| hud.push_level(rms, peak)
            })
            .build();
        service.enable();

        while let Ok(event) = receiver.recv().await {
            match event {
                BusEvent::NameAppeared(snapshot) => service.simulate_name_appeared(snapshot),
                BusEvent::NameVanished => service.simulate_name_vanished(),
                BusEvent::Properties(snapshot) => service.simulate_properties_changed(snapshot),
            }
        }
    });
}

/// The development lab: manual controls, no backend.
fn activate_lab(app: &adw::Application) {
    myna_hud::lab::present(app);
}

/// The simulated publisher (T132 fills in the served interface; for now it
/// presents the lab so the mode is not a dead end).
fn activate_serve_dbus(app: &adw::Application) {
    myna_hud::lab::present(app);
}
