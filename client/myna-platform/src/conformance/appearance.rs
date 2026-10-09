//! The appearance suite: the backend-neutral invariants of
//! [`crate::appearance`].

use std::cell::Cell;
use std::rc::Rc;

use super::Report;
use crate::appearance::{Appearance, Freshness};

type Heard = (Rc<Cell<u32>>, Box<dyn Fn(Freshness)>);

/// Sets up one case. The desktop starts with full motion and normal contrast.
pub trait Fixture {
    /// A fresh backend over a desktop with no appearance preference set.
    fn setup(&mut self) -> Box<dyn Appearance>;

    /// Set the desktop's reduced-motion preference the way its settings do,
    /// bypassing the backend; `false` when the desktop has no way to.
    fn set_reduced_motion(&mut self, on: bool) -> bool;

    /// As [`Fixture::set_reduced_motion`], for high contrast.
    fn set_high_contrast(&mut self, on: bool) -> bool;

    /// Let queued notifications arrive.
    fn settle(&mut self) {}
}

/// Run every check against `fixture`'s backend. Panics on a violation.
pub fn run(fixture: &mut dyn Fixture) -> Report {
    let mut report = Report::default();
    let mut check = |name: &'static str, run: &mut dyn FnMut(&mut dyn Fixture) -> bool| {
        if run(fixture) {
            report.passed.push(name);
        } else {
            report.not_applicable.push(name);
        }
    };
    check("a_plain_desktop_reads_as_no_preference", &mut |f| {
        a_plain_desktop_reads_as_no_preference(f);
        true
    });
    check("readings_are_repeatable", &mut |f| {
        readings_are_repeatable(f);
        true
    });
    check("reduced_motion_is_read_back", &mut |f| {
        reduced_motion_is_read_back(f)
    });
    check("high_contrast_is_read_back", &mut |f| {
        high_contrast_is_read_back(f)
    });
    check("watch_hears_a_change_until_dropped", &mut |f| {
        watch_hears_a_change_until_dropped(f)
    });
    check("watches_are_independent", &mut |f| {
        watches_are_independent(f)
    });
    report
}

fn counter() -> Heard {
    let heard = Rc::new(Cell::new(0));
    let callback = Box::new({
        let heard = Rc::clone(&heard);
        move |_| heard.set(heard.get() + 1)
    });
    (heard, callback)
}

fn a_plain_desktop_reads_as_no_preference(fixture: &mut dyn Fixture) {
    let readings = fixture.setup().read();
    assert!(
        !readings.reduced_motion,
        "a_plain_desktop_reads_as_no_preference: motion"
    );
    assert!(
        !readings.high_contrast,
        "a_plain_desktop_reads_as_no_preference: contrast"
    );
}

fn readings_are_repeatable(fixture: &mut dyn Fixture) {
    let appearance = fixture.setup();
    assert_eq!(
        appearance.read(),
        appearance.read(),
        "readings_are_repeatable"
    );
}

fn reduced_motion_is_read_back(fixture: &mut dyn Fixture) -> bool {
    let appearance = fixture.setup();
    if !fixture.set_reduced_motion(true) {
        return false;
    }
    fixture.settle();
    assert!(
        appearance.read().reduced_motion,
        "reduced_motion_is_read_back: on"
    );
    assert!(fixture.set_reduced_motion(false));
    fixture.settle();
    assert!(
        !appearance.read().reduced_motion,
        "reduced_motion_is_read_back: off again"
    );
    true
}

fn high_contrast_is_read_back(fixture: &mut dyn Fixture) -> bool {
    let appearance = fixture.setup();
    if !fixture.set_high_contrast(true) {
        return false;
    }
    fixture.settle();
    assert!(
        appearance.read().high_contrast,
        "high_contrast_is_read_back: on"
    );
    assert!(fixture.set_high_contrast(false));
    fixture.settle();
    assert!(
        !appearance.read().high_contrast,
        "high_contrast_is_read_back: off again"
    );
    true
}

/// Flip whichever preference the desktop lets the fixture set.
fn flip(fixture: &mut dyn Fixture, on: bool) -> bool {
    fixture.set_reduced_motion(on) || fixture.set_high_contrast(on)
}

fn watch_hears_a_change_until_dropped(fixture: &mut dyn Fixture) -> bool {
    let appearance = fixture.setup();
    let (heard, callback) = counter();
    let subscription = appearance.watch(callback);
    if !flip(fixture, true) {
        return false;
    }
    fixture.settle();
    assert!(
        heard.get() > 0,
        "watch_hears_a_change_until_dropped: the change"
    );
    drop(subscription);
    let before = heard.get();
    flip(fixture, false);
    fixture.settle();
    assert_eq!(
        heard.get(),
        before,
        "watch_hears_a_change_until_dropped: heard after the drop"
    );
    true
}

fn watches_are_independent(fixture: &mut dyn Fixture) -> bool {
    let appearance = fixture.setup();
    let (first, first_callback) = counter();
    let (second, second_callback) = counter();
    let dropped = appearance.watch(first_callback);
    let _kept = appearance.watch(second_callback);
    drop(dropped);
    if !flip(fixture, true) {
        return false;
    }
    fixture.settle();
    assert_eq!(first.get(), 0, "watches_are_independent: the dropped one");
    assert!(second.get() > 0, "watches_are_independent: the kept one");
    true
}
