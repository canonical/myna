//! The appearance suite against an in-memory reference desktop, and against
//! that backend with one flaw each, which the suite must reject.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use myna_platform::appearance::{Appearance, AppearanceReadings, Freshness};
use myna_platform::conformance::appearance::{run, Fixture};
use myna_platform::Subscription;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flaw {
    None,
    StartsReduced,
    ReadingsDrift,
    NeverNotifies,
    NotifiesAfterDrop,
    DropSilencesEveryWatch,
    NoContrast,
}

type Listener = (u64, Rc<dyn Fn(Freshness)>);

#[derive(Default)]
struct Desk {
    reduced_motion: bool,
    high_contrast: bool,
    listeners: Vec<Listener>,
    next: u64,
    reads: Cell<u32>,
}

type Shared = Rc<RefCell<Desk>>;

fn notify(desk: &Shared) {
    let heard: Vec<_> = desk
        .borrow()
        .listeners
        .iter()
        .map(|(_, f)| Rc::clone(f))
        .collect();
    for f in heard {
        f(Freshness::Current);
    }
}

struct Reference {
    desk: Shared,
    flaw: Flaw,
}

impl Appearance for Reference {
    fn read(&self) -> AppearanceReadings {
        let desk = self.desk.borrow();
        desk.reads.set(desk.reads.get() + 1);
        AppearanceReadings {
            accent: None,
            reduced_motion: desk.reduced_motion
                || self.flaw == Flaw::StartsReduced
                || (self.flaw == Flaw::ReadingsDrift && desk.reads.get() % 2 == 0),
            high_contrast: desk.high_contrast,
        }
    }

    fn watch(&self, changed: Box<dyn Fn(Freshness)>) -> Subscription {
        let id = {
            let mut desk = self.desk.borrow_mut();
            desk.next += 1;
            let id = desk.next;
            if self.flaw != Flaw::NeverNotifies {
                desk.listeners.push((id, Rc::from(changed)));
            }
            id
        };
        let desk = Rc::clone(&self.desk);
        let flaw = self.flaw;
        Subscription::new(move || match flaw {
            Flaw::NotifiesAfterDrop => {}
            Flaw::DropSilencesEveryWatch => desk.borrow_mut().listeners.clear(),
            _ => desk.borrow_mut().listeners.retain(|(i, _)| *i != id),
        })
    }
}

struct Rig {
    desk: Shared,
    flaw: Flaw,
}

impl Fixture for Rig {
    fn setup(&mut self) -> Box<dyn Appearance> {
        self.desk = Shared::default();
        Box::new(Reference {
            desk: Rc::clone(&self.desk),
            flaw: self.flaw,
        })
    }

    fn set_reduced_motion(&mut self, on: bool) -> bool {
        self.desk.borrow_mut().reduced_motion = on;
        notify(&self.desk);
        true
    }

    fn set_high_contrast(&mut self, on: bool) -> bool {
        if self.flaw == Flaw::NoContrast {
            return false;
        }
        self.desk.borrow_mut().high_contrast = on;
        notify(&self.desk);
        true
    }
}

fn rig(flaw: Flaw) -> Rig {
    Rig {
        desk: Shared::default(),
        flaw,
    }
}

fn rejects(flaw: Flaw, check: &str) {
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(&mut rig(flaw));
    }))
    .expect_err("the suite accepted a flawed backend");
    let message = failure
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| failure.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        message.contains(check),
        "{flaw:?} was rejected by the wrong check: {message}"
    );
}

#[test]
fn the_reference_desktop_passes_every_check() {
    let report = run(&mut rig(Flaw::None));
    assert_eq!(report.passed.len(), 6);
    assert!(report.not_applicable.is_empty());
}

#[test]
fn a_desktop_that_cannot_set_contrast_still_checks_motion() {
    let report = run(&mut rig(Flaw::NoContrast));
    assert_eq!(report.not_applicable, ["high_contrast_is_read_back"]);
}

#[test]
fn a_desktop_that_starts_reduced_is_rejected() {
    rejects(
        Flaw::StartsReduced,
        "a_plain_desktop_reads_as_no_preference",
    );
}

#[test]
fn drifting_readings_are_rejected() {
    rejects(Flaw::ReadingsDrift, "readings_are_repeatable");
}

#[test]
fn a_watch_that_never_fires_is_rejected() {
    rejects(Flaw::NeverNotifies, "watch_hears_a_change_until_dropped");
}

#[test]
fn a_watch_that_outlives_its_subscription_is_rejected() {
    rejects(
        Flaw::NotifiesAfterDrop,
        "watch_hears_a_change_until_dropped: heard after the drop",
    );
}

#[test]
fn a_drop_that_silences_other_watches_is_rejected() {
    rejects(
        Flaw::DropSilencesEveryWatch,
        "watches_are_independent: the kept one",
    );
}
