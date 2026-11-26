//! The processing steps as stored: an ordered list of enabled step ids and
//! their parameters.
//!
//! Steps run in a fixed stage order that the user does not set; the daemon
//! compiles the list (`myna-desktop`'s `process::steps`), this module only
//! names and orders it.

use std::collections::BTreeMap;

/// Step parameters as stored: `<step>.<param>` to its value.
pub type Params = BTreeMap<String, String>;

/// The parameter key of `step`'s `param`: `translate.to`.
pub fn param_key(step: &str, param: &str) -> String {
    format!("{step}.{param}")
}

/// How Chinese text is written (`script.mode`). A speech model picks a
/// script as it likes; anything but `AsRecognised` converts what it wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChineseScript {
    /// Typed as the speech model wrote it.
    #[default]
    AsRecognised,
    Simplified,
    Traditional,
    /// Traditional, in the characters and words used in Taiwan.
    TraditionalTaiwan,
    /// Traditional, in the characters used in Hong Kong.
    TraditionalHongKong,
}

impl ChineseScript {
    pub const ALL: [Self; 5] = [
        Self::AsRecognised,
        Self::Simplified,
        Self::Traditional,
        Self::TraditionalTaiwan,
        Self::TraditionalHongKong,
    ];

    /// The schema enum nick.
    pub fn nick(self) -> &'static str {
        match self {
            Self::AsRecognised => "as-recognised",
            Self::Simplified => "simplified",
            Self::Traditional => "traditional",
            Self::TraditionalTaiwan => "traditional-taiwan",
            Self::TraditionalHongKong => "traditional-hongkong",
        }
    }

    /// The script a nick names; `None` for one this build does not know.
    pub fn from_nick(nick: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|script| script.nick() == nick)
    }
}

/// Stage order, one of each; ids this build does not know keep their order
/// after the known ones, so a newer build's step survives a round trip.
pub fn in_stage_order(ids: &[String]) -> Vec<String> {
    let rank = |id: &String| {
        crate::chains::KNOWN_STEPS
            .iter()
            .position(|known| known == id)
    };
    let mut ordered: Vec<String> = Vec::new();
    for id in ids {
        if !ordered.contains(id) {
            ordered.push(id.clone());
        }
    }
    ordered.sort_by_key(|id| rank(id).unwrap_or(crate::chains::KNOWN_STEPS.len()));
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn steps_are_ordered_by_stage_once_each() {
        assert_eq!(
            in_stage_order(&ids(&["script", "x", "translate", "translate", "tidy"])),
            ids(&["tidy", "translate", "script", "x"])
        );
    }

    #[test]
    fn the_script_nicks_round_trip() {
        for script in ChineseScript::ALL {
            assert_eq!(ChineseScript::from_nick(script.nick()), Some(script));
        }
        assert_eq!(ChineseScript::from_nick("pinyin"), None);
    }
}
