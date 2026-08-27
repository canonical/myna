//! lab — the development lab and the simulator's control surface (feature
//! 004, T131/T132), replacing the extension's `dev-lab/` and `dev-lab-gpu/`
//! harnesses.
//!
//! Two modes, sharing the same controls but rendering the HUD differently:
//!
//! * **`--lab`** — the HUD is a **separate external window** (an ordinary
//!   always-on-top pill), driven directly from the controls with no backend.
//!   For developing the renderer standalone.
//! * **`--serve-dbus`** — the HUD is **embedded** in the control window as a
//!   preview (like the former Python `dev-lab-gpu`), and the controls are
//!   published over `org.myna.Dictation` so a *separate*, shell-hosted
//!   `myna-hud` instance shows the real overlay. The embedded pill is "what
//!   I am publishing"; the shell instance is the real thing.
//!
//! The level slider is the *smoothed envelope* — what the ribbon consumes —
//! and is pushed through [`envelope_to_levels`] so it travels the same
//! calibration the wire does. It also carries a plain text view: dictating
//! into it is how the focus-safety claim (FR-024) is checked by hand.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gettextrs::gettext;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::pill::Pill;
use crate::simulator::{envelope_to_levels, ERROR_REASON, NOTICE_REASON, PUBLISH_HZ};
use crate::states::{state_to_descriptor, wire};
use crate::window::HudWindow;

/// The lab's live control values.
#[derive(Clone, Debug)]
struct Controls {
    state: String,
    reason: String,
    envelope: f64,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            state: wire::RECORDING.to_string(),
            reason: String::new(),
            envelope: 0.4,
        }
    }
}

/// Where the lab renders the HUD: a separate overlay window, or a pill
/// embedded in the control window.
enum Target {
    /// `--lab`: an external always-on-top HUD window.
    Window(Rc<HudWindow>),
    /// `--serve-dbus`: an embedded preview pill.
    Embedded(Rc<Pill>),
}

impl Target {
    fn apply_descriptor(&self, descriptor: crate::states::Descriptor) {
        match self {
            Target::Window(w) => w.apply_descriptor(descriptor),
            Target::Embedded(p) => p.apply_descriptor(descriptor),
        }
    }

    fn push_level(&self, rms: f64, peak: f64) {
        match self {
            Target::Window(w) => w.push_level(rms, peak),
            Target::Embedded(p) => p.push_level(rms, peak),
        }
    }
}

/// `--lab`: the HUD as an external window, no backend.
pub fn present(app: &adw::Application) {
    let hud = HudWindow::new(app);
    hud.present_standalone();
    build_controls(app, Target::Window(hud), None);
}

/// `--serve-dbus`: the HUD embedded as a preview, publishing to the bus so a
/// shell-hosted instance shows the real overlay.
pub fn present_serving(app: &adw::Application) {
    let shared = crate::serve::Shared::default();
    let serve_shared = shared.clone();

    // Claim the name on the connection's own executor. A failure (the daemon
    // already owns it, or there is no session bus) is logged, not fatal: the
    // embedded preview still works.
    glib::spawn_future_local(async move {
        match crate::serve::serve(serve_shared).await {
            Ok(connection) => {
                std::mem::forget(connection); // held for the process lifetime
                eprintln!("myna-hud --serve-dbus: serving org.myna.Dictation");
            }
            Err(e) => eprintln!("myna-hud --serve-dbus: {e}"),
        }
    });

    let sink: ControlsSink = Box::new(move |controls: &Controls| {
        let reason = match controls.state.as_str() {
            wire::NOTICE => NOTICE_REASON.to_string(),
            wire::ERROR => ERROR_REASON.to_string(),
            _ => controls.reason.clone(),
        };
        shared.set_controls(crate::serve::Controls {
            state: controls.state.clone(),
            reason,
            envelope: controls.envelope,
        });
    });

    let pill = Pill::new();
    build_controls(app, Target::Embedded(pill), Some(sink));
}

type ControlsSink = Box<dyn Fn(&Controls)>;

fn build_controls(app: &adw::Application, target: Target, sink: Option<ControlsSink>) {
    let target = Rc::new(target);
    let controls = Rc::new(RefCell::new(Controls::default()));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(gettext("myna HUD lab"))
        .default_width(460)
        .default_height(560)
        .build();

    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    page.set_margin_top(16);
    page.set_margin_bottom(16);
    page.set_margin_start(16);
    page.set_margin_end(16);

    // ── Embedded HUD preview (--serve-dbus only) ────────────────────────
    // The pill is what the shell-hosted instance is being told to show; it
    // lives at the top of the control window, framed, so "publishing X" is
    // visible without a second window.
    if let Target::Embedded(pill) = target.as_ref() {
        let frame = gtk::Frame::new(Some(&gettext("Published to org.myna.Dictation")));
        let holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        holder.set_halign(gtk::Align::Center);
        holder.set_margin_top(8);
        holder.set_margin_bottom(8);
        holder.append(pill.widget());
        frame.set_child(Some(&holder));
        page.append(&frame);
    }

    // ── State ───────────────────────────────────────────────────────────
    let states = gtk::StringList::new(&wire::ALL);
    let state_row = adw::ComboRow::builder()
        .title(gettext("State"))
        .subtitle(gettext("the wire value the publisher would send"))
        .model(&states)
        .build();
    state_row.set_selected(
        wire::ALL
            .iter()
            .position(|s| *s == wire::RECORDING)
            .unwrap_or(0) as u32,
    );

    // ── Level ───────────────────────────────────────────────────────────
    let level = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
    level.set_value(0.4);
    level.set_hexpand(true);
    level.set_draw_value(true);
    let level_row = adw::ActionRow::builder()
        .title(gettext("Audio level"))
        .subtitle(gettext("the smoothed envelope, published as RMS/peak"))
        .build();
    level_row.add_suffix(&level);

    let group = adw::PreferencesGroup::new();
    group.add(&state_row);
    group.add(&level_row);
    page.append(&group);

    // ── Dictation target (focus safety, FR-024) ─────────────────────────
    let dictation_target = gtk::TextView::new();
    dictation_target.set_wrap_mode(gtk::WrapMode::WordChar);
    dictation_target.buffer().set_text(&gettext(
        "Type here while the HUD is showing: the caret must never move to the HUD.",
    ));
    let scroller = gtk::ScrolledWindow::builder()
        .child(&dictation_target)
        .vexpand(true)
        .has_frame(true)
        .build();
    let target_label = gtk::Label::new(Some(&gettext("Dictation target")));
    target_label.set_xalign(0.0);
    target_label.add_css_class("heading");
    page.append(&target_label);
    page.append(&scroller);

    // Composed by hand rather than with adw::ToolbarView, which would require
    // a libadwaita 1.4 compile-time feature; the runtime matrix keeps this
    // crate's adw feature floor at 1.0 (R26).
    let shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shell.append(&adw::HeaderBar::new());
    shell.append(&page);
    window.set_content(Some(&shell));

    // ── Wiring ──────────────────────────────────────────────────────────
    let sink = Rc::new(sink);
    let apply = {
        let target = target.clone();
        let controls = controls.clone();
        let sink = sink.clone();
        move || {
            let controls = controls.borrow();
            if let Some(sink) = sink.as_ref() {
                sink(&controls);
            }
            // The two problem states carry the simulator's content-free
            // reasons, so both ErrorMessage renderings are visible from here.
            // `idle` is simply one of the states in the list — it clears the
            // HUD entirely (FR-002/X3), which is why there is no separate
            // "session active" switch: it would say the same thing twice.
            let (state, reason) = match controls.state.as_str() {
                wire::NOTICE => (wire::NOTICE.to_string(), NOTICE_REASON.to_string()),
                wire::ERROR => (wire::ERROR.to_string(), ERROR_REASON.to_string()),
                other => (other.to_string(), String::new()),
            };
            let reason = if controls.reason.is_empty() {
                reason
            } else {
                controls.reason.clone()
            };
            target.apply_descriptor(state_to_descriptor(Some(&state), &reason));
        }
    };

    state_row.connect_selected_notify({
        let controls = controls.clone();
        let apply = apply.clone();
        move |row| {
            let index = row.selected() as usize;
            if let Some(state) = wire::ALL.get(index) {
                controls.borrow_mut().state = (*state).to_string();
            }
            apply();
        }
    });

    level.connect_value_changed({
        let controls = controls.clone();
        move |scale| controls.borrow_mut().envelope = scale.value()
    });

    // Publish levels at the contract's cadence rather than the render rate,
    // so the consumer sees the update pattern it was tuned against (C4) —
    // including the stale-decay behaviour when the slider sits still.
    //
    // This drives BOTH targets: the embedded/external pill directly via
    // push_level, and — crucially — the bus `Shared` via the sink, so a
    // shell-hosted instance's AudioRms tracks the slider. Without the sink
    // call here the envelope only reached the bus on a state change (the
    // only other place the sink runs), so the embedded preview moved with
    // the slider but the external instance sat frozen at the last state's
    // level.
    glib::timeout_add_local(
        Duration::from_secs_f64(1.0 / PUBLISH_HZ),
        glib::clone!(
            #[strong]
            target,
            #[strong]
            controls,
            #[strong]
            sink,
            move || {
                let controls = controls.borrow();
                // Nothing is captured while idle, so nothing is published.
                if controls.state != wire::IDLE {
                    let (rms, peak) = envelope_to_levels(controls.envelope);
                    target.push_level(rms, peak);
                    if let Some(sink) = sink.as_ref() {
                        sink(&controls);
                    }
                }
                glib::ControlFlow::Continue
            }
        ),
    );

    apply();
    window.present();
}
