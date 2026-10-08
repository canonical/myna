//! `myna-hud` — the dictation HUD renderer (feature 004, T124).
//!
//! Three modes, one binary:
//!
//! * **hosted** (default) — consume `com.canonical.Myna.Dictation` and render the
//!   pill. Under GNOME the `myna-shell` extension launches this through a
//!   `Meta.WaylandClient` and owns the window's placement (R21). With
//!   `--host x11` the HUD hosts itself on an X11 window manager
//!   ([`myna_hud::host::x11`]); it refuses a Wayland session with
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
use myna_hud::host::x11::X11Host;
use myna_hud::hud_logic::HudStyle;
use myna_hud::signals::quit_on_signal;
use myna_hud::states::state_to_descriptor;
use myna_hud::window::HudWindow;

const APP_ID: &str = "com.canonical.Myna.Hud";

/// `--host x11` outside an X11 session: retrying cannot help (EX_CONFIG).
const EXIT_WRONG_SESSION: u8 = 78;

/// Who hosts the hosted HUD's window.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Host {
    /// A desktop shell (GNOME's `myna-shell`), or nobody.
    External,
    /// The HUD itself, on X11.
    X11,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Hosted(Host),
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

    if mode == Mode::Hosted(Host::X11) {
        let session = myna_platform::Session::detect(&myna_platform::SessionEnv::from_process());
        if session.kind != myna_platform::SessionKind::X11 {
            eprintln!("myna-hud: --host x11 needs an X11 session, this one is {session:?}");
            return glib::ExitCode::new(EXIT_WRONG_SESSION);
        }
        gtk::gdk::set_allowed_backends("x11");
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
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(flags)
        .build();

    quit_on_signal();

    let failed = std::rc::Rc::new(std::cell::Cell::new(false));
    let failed_in_activate = failed.clone();
    app.connect_activate(move |app| match mode {
        Mode::Hosted(host) => {
            if !activate_hosted(app, host) {
                failed_in_activate.set(true);
                app.quit();
            }
        }
        #[cfg(dev_lab)]
        Mode::Lab => activate_lab(app),
        #[cfg(dev_lab)]
        Mode::ServeDbus => activate_serve_dbus(app),
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
    let mut mode = Mode::Hosted(Host::External);
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--host" => mode = Mode::Hosted(parse_host(arguments.next().as_deref())?),
            other if other.starts_with("--host=") => {
                mode = Mode::Hosted(parse_host(other.strip_prefix("--host="))?)
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

fn parse_host(value: Option<&str>) -> Result<Host, String> {
    match value {
        Some("x11") => Ok(Host::X11),
        Some(other) => Err(format!("myna-hud: unknown host {other}\n\n{USAGE}")),
        None => Err(format!("myna-hud: --host needs a value\n\n{USAGE}")),
    }
}

#[cfg(dev_lab)]
const USAGE: &str = "\
Usage: myna-hud [OPTION]

The myna dictation HUD renderer.

  (no option)    consume com.canonical.Myna.Dictation and render the HUD
  --host x11     as above, placing its own window on an X11 window manager
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
  --version      print the version and exit
  -h, --help     print this help and exit";

/// The shipping path: render whatever the publisher reports. False when
/// the requested host cannot run.
fn activate_hosted(app: &adw::Application, host: Host) -> bool {
    let hud = HudWindow::new(app);
    if host == Host::X11 {
        if let Err(e) = X11Host::install(&hud) {
            eprintln!("myna-hud: --host x11: {e}");
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
fn activate_lab(app: &adw::Application) {
    myna_hud::lab::present(app);
}

/// The simulated publisher: the lab, plus a real `com.canonical.Myna.Dictation` on the
/// session bus so the hosted path can be exercised without the daemon.
#[cfg(dev_lab)]
fn activate_serve_dbus(app: &adw::Application) {
    myna_hud::lab::present_serving(app);
}
