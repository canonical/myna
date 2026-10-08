//! The activation suite against an in-memory reference desktop, and against
//! that backend with one flaw each, which the suite must reject.

use std::cell::RefCell;
use std::rc::Rc;

use myna_platform::activation::{Accelerator, Action, Activation, ActivationError, Conflict};
use myna_platform::conformance::activation::{run, Fixture};
use myna_platform::conformance::Report;
use myna_platform::Subscription;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flaw {
    None,
    KeepsOldBinding,
    ClearWipesOthers,
    OwnKeyConflicts,
    CaseSensitiveConflicts,
    ReleaseWipesOthers,
    ReleasesReservedKeys,
    NeverNotifiesOutside,
    NotifiesAfterDrop,
    BindDropsOthers,
}

#[derive(Default)]
struct Desk {
    myna: Vec<(Accelerator, String)>,
    others: Vec<(Accelerator, bool)>,
    listeners: Vec<(u64, Rc<dyn Fn()>)>,
    next: u64,
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
        f();
    }
}

struct Reference {
    desk: Shared,
    flaw: Flaw,
}

impl Activation for Reference {
    fn binding(&self) -> Result<Option<Accelerator>, ActivationError> {
        Ok(self.desk.borrow().myna.last().map(|(key, _)| key.clone()))
    }

    fn command(&self) -> Result<Option<String>, ActivationError> {
        Ok(self.desk.borrow().myna.last().map(|(_, c)| c.clone()))
    }

    fn bind(&self, accelerator: &Accelerator, action: &Action) -> Result<(), ActivationError> {
        {
            let mut desk = self.desk.borrow_mut();
            if self.flaw != Flaw::KeepsOldBinding {
                desk.myna.clear();
            }
            if self.flaw == Flaw::BindDropsOthers {
                desk.others.clear();
            }
            desk.myna
                .push((accelerator.clone(), action.command.clone()));
        }
        notify(&self.desk);
        Ok(())
    }

    fn clear(&self) -> Result<(), ActivationError> {
        {
            let mut desk = self.desk.borrow_mut();
            if self.flaw == Flaw::KeepsOldBinding {
                desk.myna.pop();
            } else {
                desk.myna.clear();
            }
            if self.flaw == Flaw::ClearWipesOthers {
                desk.others.clear();
            }
        }
        notify(&self.desk);
        Ok(())
    }

    fn conflicts(&self, accelerator: &Accelerator) -> Result<Vec<Conflict>, ActivationError> {
        let desk = self.desk.borrow();
        let same = |held: &Accelerator| {
            if self.flaw == Flaw::CaseSensitiveConflicts {
                held.as_str() == accelerator.as_str()
            } else {
                accelerator.same_keys(held.as_str())
            }
        };
        let mut found: Vec<Conflict> = desk
            .others
            .iter()
            .filter(|(held, _)| same(held))
            .map(|(held, reserved)| Conflict {
                action: "Other".into(),
                reserved: *reserved,
                holder: held.to_string(),
            })
            .collect();
        if self.flaw == Flaw::OwnKeyConflicts {
            found.extend(
                desk.myna
                    .iter()
                    .filter(|(held, _)| same(held))
                    .map(|(held, _)| Conflict {
                        action: "Dictation".into(),
                        reserved: false,
                        holder: held.to_string(),
                    }),
            );
        }
        Ok(found)
    }

    fn release(&self, conflict: &Conflict) -> Result<(), ActivationError> {
        if conflict.reserved && self.flaw != Flaw::ReleasesReservedKeys {
            return Err(ActivationError::Reserved(conflict.action.clone()));
        }
        let mut desk = self.desk.borrow_mut();
        if self.flaw == Flaw::ReleaseWipesOthers {
            desk.others.clear();
        } else {
            desk.others
                .retain(|(held, _)| held.as_str() != conflict.holder);
        }
        Ok(())
    }

    fn watch(&self, changed: Box<dyn Fn()>) -> Subscription {
        let mut desk = self.desk.borrow_mut();
        desk.next += 1;
        let id = desk.next;
        desk.listeners.push((id, Rc::from(changed)));
        let shared = Rc::clone(&self.desk);
        let keep = self.flaw == Flaw::NotifiesAfterDrop;
        Subscription::new(move || {
            if !keep {
                shared.borrow_mut().listeners.retain(|(i, _)| *i != id);
            }
        })
    }
}

struct Fixtures {
    flaw: Flaw,
    reserved_keys: bool,
    desk: Shared,
}

impl Fixture for Fixtures {
    fn setup(&mut self) -> Box<dyn Activation> {
        self.desk = Shared::default();
        Box::new(Reference {
            desk: Rc::clone(&self.desk),
            flaw: self.flaw,
        })
    }

    fn hold(&mut self, accelerator: &Accelerator, reserved: bool) -> bool {
        if reserved && !self.reserved_keys {
            return false;
        }
        self.desk
            .borrow_mut()
            .others
            .push((accelerator.clone(), reserved));
        true
    }

    fn change_outside(&mut self, binding: Option<&Accelerator>) {
        {
            let mut desk = self.desk.borrow_mut();
            desk.myna.clear();
            if let Some(binding) = binding {
                desk.myna.push((binding.clone(), "outside".into()));
            }
        }
        if self.flaw != Flaw::NeverNotifiesOutside {
            notify(&self.desk);
        }
    }
}

fn suite(flaw: Flaw, reserved_keys: bool) -> Report {
    run(&mut Fixtures {
        flaw,
        reserved_keys,
        desk: Shared::default(),
    })
}

#[test]
fn a_desktop_with_reserved_keys_passes_every_check() {
    let report = suite(Flaw::None, true);
    assert_eq!(report.passed.len(), 9, "{report:?}");
    assert!(report.not_applicable.is_empty());
}

#[test]
fn a_desktop_without_reserved_keys_skips_that_check() {
    let report = suite(Flaw::None, false);
    assert_eq!(report.not_applicable, ["reserved_keys_are_not_released"]);
    assert_eq!(report.passed.len(), 8);
}

macro_rules! rejected {
    ($name:ident, $flaw:expr, $check:literal) => {
        #[test]
        #[should_panic(expected = $check)]
        fn $name() {
            suite($flaw, true);
        }
    };
}

rejected!(
    an_old_binding_left_behind,
    Flaw::KeepsOldBinding,
    "rebinding_replaces_the_binding"
);
rejected!(
    clear_taking_other_shortcuts,
    Flaw::ClearWipesOthers,
    "clear_removes_only_myna"
);
rejected!(
    bind_taking_other_shortcuts,
    Flaw::BindDropsOthers,
    "clear_removes_only_myna"
);
rejected!(
    myna_conflicting_with_itself,
    Flaw::OwnKeyConflicts,
    "conflicts_ignore_spelling_and_myna"
);
rejected!(
    spelling_sensitive_conflicts,
    Flaw::CaseSensitiveConflicts,
    "conflicts_ignore_spelling_and_myna"
);
rejected!(
    release_taking_other_shortcuts,
    Flaw::ReleaseWipesOthers,
    "release_frees_only_that_key"
);
rejected!(
    releasing_reserved_keys,
    Flaw::ReleasesReservedKeys,
    "reserved_keys_are_not_released"
);
rejected!(
    deaf_to_the_desktops_settings,
    Flaw::NeverNotifiesOutside,
    "watch_hears_both_sides_until_dropped"
);
rejected!(
    a_watch_that_outlives_its_subscription,
    Flaw::NotifiesAfterDrop,
    "watch_hears_both_sides_until_dropped"
);
