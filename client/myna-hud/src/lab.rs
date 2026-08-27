//! lab — the development lab (feature 004, T131), replacing the extension's
//! `dev-lab/` and `dev-lab-gpu/` harnesses.
//!
//! A control window driving the **identical** renderer modules with no
//! backend at all: every state, severity, level and phase the HUD can show
//! is reachable by hand, so the pill can be judged without a microphone, a
//! model, or the Python daemon.
//!
//! The level slider is the *smoothed envelope* — what the ribbon consumes —
//! and is pushed through [`crate::simulator::envelope_to_levels`] so it
//! travels the same calibration the wire does. Driving `push_level`
//! directly would make the lab's ribbon and the hosted ribbon sit at
//! different amplitudes for the same setting (and would hide any drift
//! between the vumeter and the simulator).
//!
//! It also carries a plain text view: dictating into it is how the
//! focus-safety claim (FR-024 — the HUD must never steal the caret) gets
//! checked by hand.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gettextrs::gettext;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

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

/// Build and show the lab: the HUD itself plus its control window.
pub fn present(app: &adw::Application) {
    present_with(app, None);
}

/// The lab plus a live `org.myna.Dictation` on the session bus, fed by the
/// same controls (`--serve-dbus`, T132). The HUD still renders directly from
/// the controls — the lab must stay usable even if no bus is available — but
/// a real name now exists for the hosted path or an external consumer to
/// read, and `Start`/`Stop`/`Toggle` are answerable from a terminal.
pub fn present_serving(app: &adw::Application) {
    let shared = crate::serve::Shared::default();
    let serve_shared = shared.clone();

    // Claim the name on the connection's own executor. A failure (the daemon
    // already owns it, or there is no session bus) is logged, not fatal: the
    // lab keeps working as a pure renderer.
    glib::spawn_future_local(async move {
        match crate::serve::serve(serve_shared).await {
            Ok(_connection) => {
                // Held for the process lifetime; the executor drives it.
                std::mem::forget(_connection);
                eprintln!("myna-hud --serve-dbus: serving org.myna.Dictation");
            }
            Err(e) => eprintln!("myna-hud --serve-dbus: {e}"),
        }
    });

    present_with(
        app,
        Some(Box::new(move |controls: &Controls, active: bool| {
            let _ = active;
            // The two problem states carry the simulator's content-free reasons.
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
        })),
    );
}

type ControlsSink = Box<dyn Fn(&Controls, bool)>;

fn present_with(app: &adw::Application, sink: Option<ControlsSink>) {
    let hud = HudWindow::new(app);
    hud.present_standalone();

    let controls = Rc::new(RefCell::new(Controls::default()));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(gettext("myna HUD lab"))
        .default_width(460)
        .default_height(520)
        .build();

    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    page.set_margin_top(16);
    page.set_margin_bottom(16);
    page.set_margin_start(16);
    page.set_margin_end(16);

    // ── State ───────────────────────────────────────────────────────────
    let states = gtk::StringList::new(&wire::ALL);
    let state_row = adw::ComboRow::builder()
        .title(gettext("State"))
        .subtitle(gettext("the wire value the publisher would send"))
        .model(&states)
        .build();
    // Start on `recording`, the state the ribbon is most alive in.
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
    let target = gtk::TextView::new();
    target.set_wrap_mode(gtk::WrapMode::WordChar);
    target.buffer().set_text(&gettext(
        "Type here while the HUD is showing: the caret must never move to the HUD.",
    ));
    let scroller = gtk::ScrolledWindow::builder()
        .child(&target)
        .vexpand(true)
        .has_frame(true)
        .build();
    let target_label = gtk::Label::new(Some(&gettext("Dictation target")));
    target_label.set_xalign(0.0);
    target_label.add_css_class("heading");
    page.append(&target_label);
    page.append(&scroller);

    // Composed by hand rather than with adw::ToolbarView, which would
    // require a libadwaita 1.4 compile-time feature; the runtime matrix
    // keeps this crate's adw feature floor at 1.0 (R26).
    let shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shell.append(&adw::HeaderBar::new());
    shell.append(&page);
    window.set_content(Some(&shell));

    // ── Wiring ──────────────────────────────────────────────────────────
    let sink = Rc::new(sink);
    let apply = {
        let hud = hud.clone();
        let controls = controls.clone();
        let sink = sink.clone();
        move || {
            let controls = controls.borrow();
            if let Some(sink) = sink.as_ref() {
                sink(&controls, controls.state != wire::IDLE);
            }
            // The two problem states carry the simulator's content-free
            // reasons, so both ErrorMessage renderings ("No speech detected"
            // from the state module's own default, and the "Error — %s"
            // prefix) are visible from here. `idle` is simply one of the
            // states in the list — it clears the HUD entirely (FR-002/X3),
            // which is why there is no separate "session active" switch:
            // it would publish idle too, saying the same thing twice.
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
            hud.apply_descriptor(state_to_descriptor(Some(&state), &reason));
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
    glib::timeout_add_local(
        Duration::from_secs_f64(1.0 / PUBLISH_HZ),
        glib::clone!(
            #[strong]
            hud,
            #[strong]
            controls,
            move || {
                let controls = controls.borrow();
                // Nothing is captured while idle, so nothing is published.
                if controls.state != wire::IDLE {
                    let (rms, peak) = envelope_to_levels(controls.envelope);
                    hud.push_level(rms, peak);
                }
                glib::ControlFlow::Continue
            }
        ),
    );

    apply();
    window.present();
}
