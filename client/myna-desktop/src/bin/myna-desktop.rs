//! `myna-desktop` — the shipped push-to-talk dictation app (plan T21/T22).
//!
//! Composes the activation trigger (the GlobalShortcuts portal hotkey, or the
//! `StdinTrigger` stand-in for the MVP), the IBus text injector, an activity
//! indicator (GTK overlay under `ui-gtk`, else notifications), and the
//! `myna-orchestrator` session over a live `myna-audio` capture source into a
//! [`myna_desktop::DesktopController`].
//!
//! The full wiring (trigger/injector/indicator selection, `--socket`/
//! `--language`/`--hotkey` flags, the GTK main-thread ↔ tokio-worker bridge)
//! lands across branches 003b (T020), 003c (T025) and 003d (T030). This
//! foundational branch ships a compiling entry point so the crate builds under
//! both feature settings.

fn main() {
    eprintln!(
        "myna-desktop: the push-to-talk dictation app is wired up in branch \
         003b (T020). This foundational build ships the controller + boundary \
         seams (myna_desktop lib) only."
    );
    std::process::exit(1);
}
