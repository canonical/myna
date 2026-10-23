//! `myna-hud` — the dictation HUD renderer (feature 004, T124).
//!
//! Three modes, one binary:
//!
//! * **hosted** (default) — consume `com.canonical.Myna.Dictation` and render the
//!   pill. Under GNOME the `myna-shell` extension launches this through a
//!   `Meta.WaylandClient` and owns the window's placement (R21). With
//!   `--host x11` the HUD hosts itself on an X11 window manager, with
//!   `--host layer-shell` on a wlr-layer-shell compositor
//!   ([`myna_hud::host`]); a session or display it cannot host on exits
//!   [`EXIT_WRONG_SESSION`].
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
use myna_hud::host;
use myna_hud::hud_logic::HudStyle;
use myna_hud::platform::Platform;
use myna_hud::signals::quit_on_signal;
use myna_hud::states::state_to_descriptor;
use myna_hud::window::HudWindow;
use myna_platform::Profile;

const APP_ID: &str = "com.canonical.Myna.Hud";

/// A `--host` this session cannot run: retrying cannot help (EX_CONFIG).
const EXIT_WRONG_SESSION: u8 = 78;

/// `None`: a desktop shell (GNOME's `myna-shell`) hosts the window, or nobody.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Hosted(Option<host::Kind>),
    #[cfg(dev_lab)]
    Lab,
    #[cfg(dev_lab)]
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

    let platform = Platform::current();
    let host = match mode {
        Mode::Hosted(host) => host,
        #[cfg(dev_lab)]
        _ => None,
    };
    if let Some(host) = host {
        let (kind, backend) = host.session();
        let session = &platform.session;
        if session.kind != kind {
            eprintln!(
                "myna-hud: --host {} needs a session of kind {kind:?}, this one is {session:?}",
                host.name()
            );
            return glib::ExitCode::new(EXIT_WRONG_SESSION);
        }
        gtk::gdk::set_allowed_backends(backend);
    }

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
                Mode::Hosted(_) => gtk::gio::ApplicationFlags::empty(),
            }
        }
        #[cfg(not(dev_lab))]
        {
            gtk::gio::ApplicationFlags::empty()
        }
    };
    if gtk::init().is_ok() {
        myna_hud::appearance::generic::remember_startup_theme();
        if let Some(host) = host.filter(|host| !host.display_supports()) {
            eprintln!(
                "myna-hud: --host {}: the display cannot host it",
                host.name()
            );
            return glib::ExitCode::new(EXIT_WRONG_SESSION);
        }
    }
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(flags)
        .build();

    quit_on_signal();

    let failed = std::rc::Rc::new(std::cell::Cell::new(false));
    let failed_in_activate = failed.clone();
    let profile = platform.profile;
    app.connect_activate(move |app| match mode {
        Mode::Hosted(host) => {
            if !activate_hosted(app, host, profile) {
                failed_in_activate.set(true);
                app.quit();
            }
        }
        #[cfg(dev_lab)]
        Mode::Lab => activate_lab(app, profile),
        #[cfg(dev_lab)]
        Mode::ServeDbus => activate_serve_dbus(app, profile),
    });

    // Our own argv is already consumed above.
    let code = app.run_with_args::<&str>(&[]);
    if failed.get() {
        glib::ExitCode::FAILURE
    } else {
        code
    }
}

fn parse_mode() -> Result<Mode, String> {
    let mut mode = Mode::Hosted(None);
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--host" => mode = Mode::Hosted(Some(parse_host(arguments.next().as_deref())?)),
            other if other.starts_with("--host=") => {
                mode = Mode::Hosted(Some(parse_host(other.strip_prefix("--host="))?))
            }
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
                println!("myna-hud {}", env!("MYNA_VERSION"));
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

fn parse_host(value: Option<&str>) -> Result<host::Kind, String> {
    let value = value.ok_or_else(|| format!("myna-hud: --host needs a value\n\n{USAGE}"))?;
    host::Kind::parse(value).ok_or_else(|| format!("myna-hud: unknown host {value}\n\n{USAGE}"))
}

#[cfg(dev_lab)]
const USAGE: &str = "\
Usage: myna-hud [OPTION]

The myna dictation HUD renderer.

  (no option)    consume com.canonical.Myna.Dictation and render the HUD
  --host x11     as above, placing its own window on an X11 window manager
  --host layer-shell
                 as above, as a layer surface on a Wayland compositor
  --lab          development lab: manual controls, no backend
  --serve-dbus   publish a simulated com.canonical.Myna.Dictation
  --version      print the version and exit
  -h, --help     print this help and exit";

#[cfg(not(dev_lab))]
const USAGE: &str = "\
Usage: myna-hud [OPTION]

The myna dictation HUD renderer.

  (no option)    consume com.canonical.Myna.Dictation and render the HUD
  --host x11     as above, placing its own window on an X11 window manager
  --host layer-shell
                 as above, as a layer surface on a Wayland compositor
  --version      print the version and exit
  -h, --help     print this help and exit";

/// The shipping path: render whatever the publisher reports. False when
/// the requested host cannot run.
fn activate_hosted(app: &adw::Application, host: Option<host::Kind>, profile: Profile) -> bool {
    let hud = HudWindow::new(app, profile);
    if let Some(host) = host {
        if let Err(e) = host.install(&hud) {
            eprintln!("myna-hud: --host {}: {e}", host.name());
            return false;
        }
    }
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
            .on_hud_style_changed({
                let hud = hud_for_events.clone();
                move |nick| hud.set_hud_style(HudStyle::from_nick(nick))
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
    true
}

/// The development lab: manual controls, no backend.
#[cfg(dev_lab)]
fn activate_lab(app: &adw::Application, profile: Profile) {
    myna_hud::lab::present(app, profile);
}

/// The simulated publisher: the lab, plus a real `com.canonical.Myna.Dictation` on the
/// session bus so the hosted path can be exercised without the daemon.
#[cfg(dev_lab)]
fn activate_serve_dbus(app: &adw::Application, profile: Profile) {
    myna_hud::lab::present_serving(app, profile);
}
