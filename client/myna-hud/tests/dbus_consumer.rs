// tests/dbus_consumer.rs — hermetic contract test for the
// com.canonical.Myna.Dictation consumer lifecycle (feature 004, contract
// extension.md X7–X10 re-homed to the renderer; dbus-interface.md C8/C9).
// No session bus: the name watch and the proxy are injectable seams.

use std::cell::RefCell;
use std::rc::Rc;

use myna_hud::dbus_consumer::{DictationService, Snapshot};
use myna_hud::states::wire;

/// What the consumer told the application, in order.
#[derive(Debug, Clone, PartialEq)]
enum Event {
    State { state: String, error: String },
    Level { rms: f64, peak: f64 },
    Available(bool),
}

#[derive(Default)]
struct Recorder {
    events: Vec<Event>,
}

type Shared = Rc<RefCell<Recorder>>;

fn service(recorder: &Shared) -> DictationService {
    let s1 = recorder.clone();
    let s2 = recorder.clone();
    let s3 = recorder.clone();
    DictationService::builder()
        .on_state_changed(move |state, error| {
            s1.borrow_mut().events.push(Event::State {
                state: state.to_string(),
                error: error.to_string(),
            });
        })
        .on_level(move |rms, peak| {
            s2.borrow_mut().events.push(Event::Level { rms, peak });
        })
        .on_availability_changed(move |available| {
            s3.borrow_mut().events.push(Event::Available(available));
        })
        .build()
}

// --- X7: dormant while the name has no owner -----------------------------

#[test]
fn x7_dormant_without_an_owner() {
    let recorder: Shared = Rc::default();
    let mut svc = service(&recorder);
    svc.enable();
    assert!(
        recorder.borrow().events.is_empty(),
        "no state emissions and no error surfaced while dormant"
    );
    assert!(!svc.is_available(), "not available without an owner");
}

// --- X8: name-appeared connects and reflects the current State -----------

#[test]
fn x8_name_appeared_reflects_the_current_state() {
    let recorder: Shared = Rc::default();
    let mut svc = service(&recorder);
    svc.enable();
    svc.simulate_name_appeared(Snapshot {
        state: wire::RECORDING.into(),
        error_message: String::new(),
        audio_rms: 0.2,
        audio_peak: 0.4,
    });

    let events = recorder.borrow().events.clone();
    assert!(
        events.contains(&Event::Available(true)),
        "availability is announced"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::State { state, .. } if state == wire::RECORDING)),
        "the current State is reflected on connect: {events:?}"
    );
    assert!(svc.is_available());
}

// --- X8: name-vanished clears to idle (daemon crash/exit) ---------------

#[test]
fn x8_name_vanished_clears_to_idle() {
    let recorder: Shared = Rc::default();
    let mut svc = service(&recorder);
    svc.enable();
    svc.simulate_name_appeared(Snapshot {
        state: wire::RECORDING.into(),
        ..Default::default()
    });
    recorder.borrow_mut().events.clear();
    svc.simulate_name_vanished();

    let events = recorder.borrow().events.clone();
    assert_eq!(
        events.first(),
        Some(&Event::State {
            state: wire::IDLE.into(),
            error: String::new()
        }),
        "clears to idle, not frozen mid-session"
    );
    assert!(events.contains(&Event::Available(false)));
    assert!(!svc.is_available());
}

// --- Property pushes: state transitions and levels ----------------------

#[test]
fn properties_changed_forwards_state_and_levels() {
    let recorder: Shared = Rc::default();
    let mut svc = service(&recorder);
    svc.enable();
    svc.simulate_name_appeared(Snapshot {
        state: wire::LOADING.into(),
        ..Default::default()
    });
    recorder.borrow_mut().events.clear();

    svc.simulate_properties_changed(Snapshot {
        state: wire::RECORDING.into(),
        error_message: String::new(),
        audio_rms: 0.5,
        audio_peak: 0.7,
    });

    let events = recorder.borrow().events.clone();
    assert!(
        events.contains(&Event::State {
            state: wire::RECORDING.into(),
            error: String::new()
        }),
        "state transition forwarded"
    );
    assert!(
        events.contains(&Event::Level {
            rms: 0.5,
            peak: 0.7
        }),
        "levels forwarded"
    );
}

// A `notice`/`error` transition carries its content-free reason (E3).
#[test]
fn error_state_carries_its_reason() {
    let recorder: Shared = Rc::default();
    let mut svc = service(&recorder);
    svc.enable();
    svc.simulate_name_appeared(Snapshot::default());
    recorder.borrow_mut().events.clear();
    svc.simulate_properties_changed(Snapshot {
        state: wire::ERROR.into(),
        error_message: "Microphone unavailable".into(),
        ..Default::default()
    });
    assert!(
        recorder.borrow().events.contains(&Event::State {
            state: wire::ERROR.into(),
            error: "Microphone unavailable".into()
        }),
        "the reason travels with the state"
    );
}

// --- R16a regression: level updates are NEVER deduplicated --------------
// The renderer uses ARRIVAL TIME, not value, to detect a stale stream, so a
// steady voice (which legitimately repeats the same quantized RMS/peak for
// consecutive pumps) must keep refreshing that timestamp. Dropping
// "unchanged" updates made the meter decay to floor mid-speech.

#[test]
fn r16a_repeated_levels_are_still_forwarded() {
    let recorder: Shared = Rc::default();
    let mut svc = service(&recorder);
    svc.enable();
    svc.simulate_name_appeared(Snapshot::default());
    recorder.borrow_mut().events.clear();

    for _ in 0..3 {
        svc.simulate_properties_changed(Snapshot {
            state: wire::RECORDING.into(),
            error_message: String::new(),
            audio_rms: 0.31,
            audio_peak: 0.42,
        });
    }
    let levels = recorder
        .borrow()
        .events
        .iter()
        .filter(|e| matches!(e, Event::Level { .. }))
        .count();
    assert_eq!(
        levels, 3,
        "every level arrival is forwarded, identical or not"
    );
}

// The *state* descriptor, by contrast, IS deduplicated — the publisher
// pushes the whole property set on every level tick, and re-emitting an
// unchanged state would restart notice timers.
#[test]
fn repeated_states_are_deduplicated() {
    let recorder: Shared = Rc::default();
    let mut svc = service(&recorder);
    svc.enable();
    svc.simulate_name_appeared(Snapshot {
        state: wire::RECORDING.into(),
        ..Default::default()
    });
    recorder.borrow_mut().events.clear();

    for _ in 0..3 {
        svc.simulate_properties_changed(Snapshot {
            state: wire::RECORDING.into(),
            error_message: String::new(),
            audio_rms: 0.1,
            audio_peak: 0.2,
        });
    }
    let states = recorder
        .borrow()
        .events
        .iter()
        .filter(|e| matches!(e, Event::State { .. }))
        .count();
    assert_eq!(states, 0, "an unchanged state is not re-emitted");
}

// --- X9/X10: disable drops everything; re-enable re-establishes ---------

#[test]
fn x9_disable_drops_everything_and_x10_reenable_works() {
    let recorder: Shared = Rc::default();
    let mut svc = service(&recorder);
    svc.enable();
    svc.simulate_name_appeared(Snapshot {
        state: wire::RECORDING.into(),
        ..Default::default()
    });
    svc.disable();
    assert!(!svc.is_available(), "unavailable after disable");
    assert!(!svc.is_watching(), "the name watch is removed");

    recorder.borrow_mut().events.clear();
    // Events arriving after disable() must not reach the application.
    svc.simulate_properties_changed(Snapshot {
        state: wire::TRANSCRIBING.into(),
        ..Default::default()
    });
    assert!(
        recorder.borrow().events.is_empty(),
        "no emissions after disable"
    );

    svc.enable();
    assert!(svc.is_watching(), "re-enable re-establishes the watch");
    svc.simulate_name_appeared(Snapshot {
        state: wire::FINALIZING.into(),
        ..Default::default()
    });
    assert!(
        recorder
            .borrow()
            .events
            .iter()
            .any(|e| matches!(e, Event::State { state, .. } if state == wire::FINALIZING)),
        "re-enabled service reflects state again"
    );
}

// --- C8: unknown state values pass through untouched --------------------
// The consumer never interprets the wire value; the descriptor mapping owns
// the unknown-tolerance rule, so an additive value must reach it intact.

#[test]
fn c8_unknown_state_passes_through() {
    let recorder: Shared = Rc::default();
    let mut svc = service(&recorder);
    svc.enable();
    svc.simulate_name_appeared(Snapshot {
        state: "quantizing".into(),
        ..Default::default()
    });
    assert!(
        recorder
            .borrow()
            .events
            .iter()
            .any(|e| matches!(e, Event::State { state, .. } if state == "quantizing")),
        "additive values are forwarded verbatim"
    );
}
