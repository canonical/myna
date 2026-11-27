//! What a plugin's step is, read from the class it declares.
//!
//! A provider names a class; the check on its output is the client's own.
//! A plugin cannot ask for none, and a class this build does not know makes
//! the model uncovered.

use super::check::Check;
use super::wire::Capability;

/// The class a model of `task` has when its manifest names none.
pub fn default_class(task: &str) -> Option<&'static str> {
    match task {
        "text.cleanup" => Some("edit"),
        "text.translate" => Some("translate"),
        _ => None,
    }
}

/// The check that guards the output of a step of `class`.
pub fn check_for(class: &str) -> Option<Check> {
    match class {
        "edit" => Some(Check::ConservativeEdit),
        "translate" => Some(Check::Usable),
        _ => None,
    }
}

/// The check for what `capability` declares, or its task's default; `None`
/// when the class is not one this client knows.
pub fn check_of(capability: &Capability) -> Option<Check> {
    let class = capability
        .class
        .as_deref()
        .or_else(|| default_class(&capability.task))?;
    check_for(class)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(task: &str, class: Option<&str>) -> Capability {
        Capability {
            id: "m".into(),
            task: task.into(),
            class: class.map(Into::into),
            ..Capability::default()
        }
    }

    #[test]
    fn each_class_has_its_own_check() {
        assert_eq!(check_for("edit"), Some(Check::ConservativeEdit));
        assert_eq!(check_for("translate"), Some(Check::Usable));
        assert_eq!(check_for("none"), None);
        assert_eq!(check_for("Edit"), None);
        assert_eq!(check_for(""), None);
    }

    #[test]
    fn a_model_without_a_class_gets_its_tasks() {
        assert_eq!(
            check_of(&cap("text.cleanup", None)),
            Some(Check::ConservativeEdit)
        );
        assert_eq!(check_of(&cap("text.translate", None)), Some(Check::Usable));
        assert_eq!(check_of(&cap("text.other", None)), None);
    }

    #[test]
    fn a_declared_class_wins_over_the_tasks_and_an_unknown_one_is_no_check() {
        assert_eq!(check_of(&cap("text.cleanup", Some("passthrough"))), None);
        assert_eq!(
            check_of(&cap("text.other", Some("translate"))),
            Some(Check::Usable)
        );
    }
}
