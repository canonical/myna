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
    #[cfg(dev_lab)]
    Lab,
    #[cfg(dev_lab)]
    ServeDbus,
}

fn main() -> glib::ExitCode {
    // Bind the `myna` gettext domain (R25) so the lab UI localizes. The
    // publisher translates StatusMessage before it crosses D-Bus. The domain's
    // .mo files live in the standard share/locale
    // path; the snap stages them under $SNAP/share/locale, and a build-tree
    // run can point at its own po/ via the locale dir. If nothing is found,
    // gettext falls back to the identity function — never a failure.
    if let Ok(dir) = std::env::var("MYNA_LOCALEDIR") {
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

    // Lab / serve-dbus are developer harnesses that you want to run
    // repeatedly (often several at once) without D-Bus single-instance
    // forwarding — e.g. `myna-hud --lab` alongside a hosted instance.
    // There `NON_UNIQUE` is correct. The hosted HUD (no flag) must be a
    // singleton owning `com.canonical.Myna.Hud` so `myna-desktop`'s
    // `RegisterClient` + `NameOwnerChanged` pruning sees it, and the snap
    // `hud` D-Bus slot is actually claimed.
    let flags = {
        #[cfg(dev_lab)]
        {
            match mode {
                Mode::Lab | Mode::ServeDbus => gtk::gio::ApplicationFlags::NON_UNIQUE,
                Mode::Hosted => gtk::gio::ApplicationFlags::empty(),
            }
        }
        #[cfg(not(dev_lab))]
        {
            gtk::gio::ApplicationFlags::empty()
        }
    };
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(flags)
        .build();

    app.connect_activate(move |app| match mode {
        Mode::Hosted => activate_hosted(app),
        #[cfg(dev_lab)]
        Mode::Lab => activate_lab(app),
        #[cfg(dev_lab)]
        Mode::ServeDbus => activate_serve_dbus(app),
    });

    // Our own argv is already consumed above.
    app.run_with_args::<&str>(&[])
}

fn parse_mode() -> Result<Mode, String> {
    #[allow(unused_mut)]
    let mut mode = Mode::Hosted;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            #[cfg(dev_lab)]
            "--lab" => mode = Mode::Lab,
            #[cfg(not(dev_lab))]
            "--lab" => {
                return Err(format!(
                    "myna-hud: --lab requires --features dev-lab (or a debug build)\n\n{USAGE}"
                ))
            }
            #[cfg(dev_lab)]
            "--serve-dbus" => mode = Mode::ServeDbus,
            #[cfg(not(dev_lab))]
            "--serve-dbus" => {
                return Err(format!(
                "myna-hud: --serve-dbus requires --features dev-lab (or a debug build)\n\n{USAGE}"
            ))
            }
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

#[cfg(dev_lab)]
const USAGE: &str = "\
Usage: myna-hud [OPTION]

The myna dictation HUD renderer.

  (no option)    consume com.canonical.Myna.Dictation and render the HUD
  --lab          development lab: manual controls, no backend
  --serve-dbus   publish a simulated com.canonical.Myna.Dictation
  --version      print the version and exit
  -h, --help     print this help and exit";

#[cfg(not(dev_lab))]
const USAGE: &str = "\
Usage: myna-hud [OPTION]

The myna dictation HUD renderer.

  (no option)    consume com.canonical.Myna.Dictation and render the HUD
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
                move |state, status_message| {
                    hud.apply_descriptor(state_to_descriptor(Some(state), status_message));
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
#[cfg(dev_lab)]
fn activate_lab(app: &adw::Application) {
    myna_hud::lab::present(app);
}

/// The simulated publisher: the lab, plus a real `com.canonical.Myna.Dictation` on the
/// session bus so the hosted path can be exercised without the daemon.
#[cfg(dev_lab)]
fn activate_serve_dbus(app: &adw::Application) {
    myna_hud::lab::present_serving(app);
}
