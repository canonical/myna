// tests/quit_on_signal.rs — the HUD's SIGINT/SIGTERM wiring (T124).
//
// Two properties, one process, in the order a stop actually happens:
//
//  1. a signal is taken by the main loop rather than by the default
//     disposition — the HUD gets to quit its application instead of dying
//     mid-frame with `com.canonical.Myna.Hud` still held;
//  2. once that watch has been dispatched the default disposition is back,
//     so a second signal terminates a shutdown that hangs.
//
// Both are observed the way the kernel sees them: this test process raises a
// real SIGTERM at itself and reads the disposition back with `sigaction`.
// Property 2 is what a userspace signal library gets wrong — `signal-hook`,
// which this wiring used to be built on, documents that removing the last
// action leaves the signal *ignored* rather than restoring the default.
//
// Signals are process-wide, so this file holds exactly one test.

use std::time::{Duration, Instant};

use gtk4::glib;

/// The installed handler for `signum`, `SIG_DFL` when there is none.
fn disposition(signum: libc::c_int) -> libc::sighandler_t {
    let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
    let read = unsafe { libc::sigaction(signum, std::ptr::null(), &mut current) };
    assert_eq!(read, 0, "sigaction({signum}) failed");
    current.sa_sigaction
}

#[test]
fn a_signal_reaches_the_main_loop_and_leaves_the_default_behind() {
    let context = glib::MainContext::default();
    let _acquired = context
        .acquire()
        .expect("the default main context is free in a test binary");

    myna_hud::signals::quit_on_signal();
    assert_ne!(
        disposition(libc::SIGTERM),
        libc::SIG_DFL,
        "quit_on_signal left SIGTERM on its default disposition"
    );

    // Unhandled, this kills the test binary — surviving it IS property 1.
    assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);

    // Property 2. The watch is one-shot, so dispatching it drops GLib's last
    // SIGTERM source and the default disposition comes back.
    let deadline = Instant::now() + Duration::from_secs(5);
    while disposition(libc::SIGTERM) != libc::SIG_DFL {
        assert!(
            Instant::now() < deadline,
            "the SIGTERM watch never dispatched, or never gave the signal back"
        );
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(10));
    }

    // Each signal is watched on its own, and only the raised one is spent.
    assert_ne!(
        disposition(libc::SIGINT),
        libc::SIG_DFL,
        "dispatching SIGTERM also gave up the SIGINT watch"
    );
}
