//! `myna-hud` — the dictation HUD renderer (feature 004, T124).
//!
//! Three modes, one binary:
//!
//! * **hosted** (default) — consume `com.canonical.Myna.Dictation` and render the
//!   pill. Under GNOME the `myna-shell` extension launches this through a
//!   `Meta.WaylandClient` and owns the window's placement (R21); elsewhere
//!   it is an ordinary always-on-top window.
//! * `--lab` — the development lab: manual controls driving the identical
//!   renderer modules with no backend at all.
//! * `--serve-dbus` — publish a simulated `com.canonical.Myna.Dictation` so the real
//!   hosted path can be exercised without the Python daemon.
//!
//! GTK owns the main thread; the bus worker talks to it over a channel.

use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use myna_hud::bus::{self, BusEvent};
use myna_hud::dbus_consumer::DictationService;
use myna_hud::i18n::DOMAIN;
use myna_hud::states::state_to_descriptor;
use myna_hud::window::HudWindow;

const APP_ID: &str = "com.canonical.Myna.Hud";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Hosted,
    Lab,
    ServeDbus,
}

fn main() -> glib::ExitCode {
    // Bind the `myna` gettext domain (R25) so the status strings and lab UI
    // localize. The domain's .mo files live in the standard share/locale
    // path; the snap stages them under $SNAP/share/locale, and a build-tree
    // run can point at its own po/ via the locale dir. If nothing is found,
    // gettext falls back to the identity function — never a failure.
    if let Ok(dir) = std::env::var("MYNA_HUD_LOCALEDIR") {
        let _ = gettextrs::TextDomain::new(DOMAIN).push(dir).init();
    } else {
        let _ = gettextrs::TextDomain::new(DOMAIN).init();
    }

    let mode = match parse_mode() {
        Ok(mode) => mode,
        Err(message) => {
            eprintln!("{message}");
            return glib::ExitCode::FAILURE;
        }
    };

    let app = adw::Application::builder()
        .application_id(APP_ID)
        // The HUD is a singleton that owns com.canonical.Myna.Hud; it
        // registers as a client of com.canonical.Myna.Dictation so the
        // publisher can suppress its notification fallback while a HUD
        // is present. NON_UNIQUE would leave the name unclaimed and the
        // publisher would never see the client, so we use the default
        // (FLAGS_NONE) which requests the well-known name.
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

  (no option)    consume com.canonical.Myna.Dictation and render the HUD
  --lab          development lab: manual controls, no backend
  --serve-dbus   publish a simulated com.canonical.Myna.Dictation
  --version      print the version and exit
  -h, --help     print this help and exit";

/// The shipping path: render whatever the publisher reports.
fn activate_hosted(app: &adw::Application) {
    let hud = HudWindow::new(app);
    // Start idle → the window stays UNMAPPED and shows nothing until the
    // first non-idle state maps it, at which point the host adopts it (the
    // host adopts on map, and re-adopts on every subsequent map across the
    // idle unmap/remap cycle). Presenting an empty window at startup would
    // otherwise flash a static, empty pill before the first bus event.
    hud.apply_descriptor(state_to_descriptor(None, ""));

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

/// The simulated publisher: the lab, plus a real `com.canonical.Myna.Dictation` on the
/// session bus so the hosted path can be exercised without the daemon.
fn activate_serve_dbus(app: &adw::Application) {
    myna_hud::lab::present_serving(app);
}
