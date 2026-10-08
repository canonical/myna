//! The activation suite: the backend-neutral invariants of
//! [`crate::activation`].

use std::cell::Cell;
use std::rc::Rc;

use super::Report;
use crate::activation::{Accelerator, Action, Activation, ActivationError};

/// Sets up one case. The desktop starts with no binding for Myna.
pub trait Fixture {
    /// A fresh backend over a desktop with no Myna binding.
    fn setup(&mut self) -> Box<dyn Activation>;

    /// Give another shortcut `accelerator`, as the desktop's own keyboard
    /// settings would. `reserved` asks for one the desktop will not give up;
    /// `false` when the desktop has none.
    fn hold(&mut self, accelerator: &Accelerator, reserved: bool) -> bool;

    /// Set or clear (`None`) Myna's binding the way the user's keyboard
    /// settings do, bypassing the backend.
    fn change_outside(&mut self, binding: Option<&Accelerator>);

    /// Let queued notifications arrive.
    fn settle(&mut self) {}
}

fn key(text: &str) -> Accelerator {
    Accelerator::parse(text).expect("a chord")
}

fn toggle() -> Action {
    Action {
        name: "Dictation".into(),
        command: "toggle-dictation".into(),
    }
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
    check("nothing_is_bound_at_first", &mut |f| {
        nothing_is_bound_at_first(f);
        true
    });
    check("bind_is_read_back", &mut |f| {
        bind_is_read_back(f);
        true
    });
    check("rebinding_replaces_the_binding", &mut |f| {
        rebinding_replaces_the_binding(f);
        true
    });
    check("clear_removes_only_myna", &mut |f| {
        clear_removes_only_myna(f);
        true
    });
    check("bind_keeps_other_shortcuts", &mut |f| {
        bind_keeps_other_shortcuts(f);
        true
    });
    check("conflicts_ignore_spelling_and_myna", &mut |f| {
        conflicts_ignore_spelling_and_myna(f);
        true
    });
    check("release_frees_only_that_key", &mut |f| {
        release_frees_only_that_key(f);
        true
    });
    check("reserved_keys_are_not_released", &mut |f| {
        reserved_keys_are_not_released(f)
    });
    check("watch_hears_both_sides_until_dropped", &mut |f| {
        watch_hears_both_sides_until_dropped(f);
        true
    });
    report
}

fn bound(activation: &dyn Activation) -> Option<Accelerator> {
    activation.binding().expect("binding readable")
}

fn nothing_is_bound_at_first(fixture: &mut dyn Fixture) {
    let activation = fixture.setup();
    assert_eq!(bound(&*activation), None, "nothing_is_bound_at_first");
    assert_eq!(
        activation.command().expect("command readable"),
        None,
        "nothing_is_bound_at_first"
    );
    assert!(
        activation.clear().is_ok(),
        "nothing_is_bound_at_first: clearing nothing"
    );
}

fn bind_is_read_back(fixture: &mut dyn Fixture) {
    let activation = fixture.setup();
    activation
        .bind(&key("<Super>j"), &toggle())
        .expect("bind_is_read_back");
    let binding = bound(&*activation).expect("bind_is_read_back: bound");
    assert!(binding.same_keys("<Super>j"), "bind_is_read_back");
    assert_eq!(
        activation.command().unwrap().as_deref(),
        Some("toggle-dictation"),
        "bind_is_read_back"
    );
    // Binding again with a new command updates it in place.
    let action = Action {
        command: "other".into(),
        ..toggle()
    };
    activation.bind(&key("<Super>j"), &action).unwrap();
    assert_eq!(
        activation.command().unwrap().as_deref(),
        Some("other"),
        "bind_is_read_back: command follows"
    );
}

fn rebinding_replaces_the_binding(fixture: &mut dyn Fixture) {
    let activation = fixture.setup();
    activation.bind(&key("<Super>j"), &toggle()).unwrap();
    activation.bind(&key("<Control><Alt>d"), &toggle()).unwrap();
    let binding = bound(&*activation).expect("rebinding_replaces_the_binding");
    assert!(
        binding.same_keys("<Control><Alt>d"),
        "rebinding_replaces_the_binding"
    );
    // The old key is free again: Myna holds one key at a time.
    assert!(
        activation.conflicts(&key("<Super>j")).unwrap().is_empty(),
        "rebinding_replaces_the_binding: old key still held"
    );
    // Clearing leaves nothing behind for the old one to resurface from.
    activation.clear().unwrap();
    assert_eq!(
        bound(&*activation),
        None,
        "rebinding_replaces_the_binding: the old binding came back"
    );
}

fn clear_removes_only_myna(fixture: &mut dyn Fixture) {
    let activation = fixture.setup();
    assert!(fixture.hold(&key("<Super>t"), false));
    activation.bind(&key("<Super>j"), &toggle()).unwrap();
    activation.clear().expect("clear_removes_only_myna");
    assert_eq!(bound(&*activation), None, "clear_removes_only_myna");
    assert_eq!(
        activation.command().unwrap(),
        None,
        "clear_removes_only_myna: command"
    );
    assert_eq!(
        activation.conflicts(&key("<Super>t")).unwrap().len(),
        1,
        "clear_removes_only_myna: another shortcut went with it"
    );
}

fn bind_keeps_other_shortcuts(fixture: &mut dyn Fixture) {
    let activation = fixture.setup();
    assert!(fixture.hold(&key("<Super>t"), false));
    activation.bind(&key("<Super>j"), &toggle()).unwrap();
    activation.bind(&key("<Super>k"), &toggle()).unwrap();
    assert_eq!(
        activation.conflicts(&key("<Super>t")).unwrap().len(),
        1,
        "bind_keeps_other_shortcuts"
    );
}

fn conflicts_ignore_spelling_and_myna(fixture: &mut dyn Fixture) {
    let activation = fixture.setup();
    assert!(fixture.hold(&key("<Control><Alt>q"), false));
    for spelling in ["<Primary><Alt>q", "<Alt><Ctrl>Q", "<Mod1><Control>q"] {
        let conflicts = activation.conflicts(&key(spelling)).unwrap();
        assert_eq!(
            conflicts.len(),
            1,
            "conflicts_ignore_spelling_and_myna: {spelling}"
        );
        assert!(!conflicts[0].reserved);
        assert!(
            !conflicts[0].action.is_empty() && !conflicts[0].holder.is_empty(),
            "conflicts_ignore_spelling_and_myna: describes the holder"
        );
    }
    assert!(activation.conflicts(&key("<Super>x")).unwrap().is_empty());
    // Myna's own key is not a conflict with itself.
    activation.bind(&key("<Super>j"), &toggle()).unwrap();
    assert!(
        activation.conflicts(&key("<Super>j")).unwrap().is_empty(),
        "conflicts_ignore_spelling_and_myna: own key"
    );
}

fn release_frees_only_that_key(fixture: &mut dyn Fixture) {
    let activation = fixture.setup();
    assert!(fixture.hold(&key("<Super>t"), false));
    assert!(fixture.hold(&key("<Super>y"), false));
    activation.bind(&key("<Super>j"), &toggle()).unwrap();
    let conflict = activation
        .conflicts(&key("<Super>t"))
        .unwrap()
        .pop()
        .expect("release_frees_only_that_key: held");
    activation
        .release(&conflict)
        .expect("release_frees_only_that_key");
    assert!(
        activation.conflicts(&key("<Super>t")).unwrap().is_empty(),
        "release_frees_only_that_key: still held"
    );
    assert_eq!(
        activation.conflicts(&key("<Super>y")).unwrap().len(),
        1,
        "release_frees_only_that_key: another key went too"
    );
    assert!(
        bound(&*activation).is_some_and(|binding| binding.same_keys("<Super>j")),
        "release_frees_only_that_key: Myna's binding"
    );
}

fn reserved_keys_are_not_released(fixture: &mut dyn Fixture) -> bool {
    let activation = fixture.setup();
    if !fixture.hold(&key("<Super>o"), true) {
        return false;
    }
    let conflict = activation
        .conflicts(&key("<Super>o"))
        .unwrap()
        .pop()
        .expect("reserved_keys_are_not_released: listed");
    assert!(conflict.reserved, "reserved_keys_are_not_released: flagged");
    assert!(
        matches!(
            activation.release(&conflict),
            Err(ActivationError::Reserved(_))
        ),
        "reserved_keys_are_not_released"
    );
    assert_eq!(
        activation.conflicts(&key("<Super>o")).unwrap().len(),
        1,
        "reserved_keys_are_not_released: it went anyway"
    );
    true
}

fn watch_hears_both_sides_until_dropped(fixture: &mut dyn Fixture) {
    let activation = fixture.setup();
    let heard = Rc::new(Cell::new(0));
    let subscription = activation.watch(Box::new({
        let heard = Rc::clone(&heard);
        move || heard.set(heard.get() + 1)
    }));
    fixture.settle();
    let before = heard.get();

    activation.bind(&key("<Super>j"), &toggle()).unwrap();
    fixture.settle();
    assert!(
        heard.get() > before,
        "watch_hears_both_sides_until_dropped: Myna's own bind"
    );

    let before = heard.get();
    fixture.change_outside(Some(&key("<Super>k")));
    fixture.settle();
    assert!(
        heard.get() > before,
        "watch_hears_both_sides_until_dropped: the desktop's settings"
    );
    assert!(
        bound(&*activation).is_some_and(|binding| binding.same_keys("<Super>k")),
        "watch_hears_both_sides_until_dropped: outside change is read"
    );

    drop(subscription);
    let before = heard.get();
    fixture.change_outside(None);
    activation.bind(&key("<Super>j"), &toggle()).unwrap();
    fixture.settle();
    assert_eq!(
        heard.get(),
        before,
        "watch_hears_both_sides_until_dropped: heard after the drop"
    );
}
