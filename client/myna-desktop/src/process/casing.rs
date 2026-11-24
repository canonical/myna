//! Spoken code casing: "camel case user name" is typed as `userName`. The
//! casing applies to the words after its name up to the next comma, full
//! stop or other mark, or to the end; the mark itself stays.

/// How words are joined and cased.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Camel,
    Pascal,
    Snake,
    Kebab,
    Constant,
    Title,
    Upper,
    Lower,
    Smash,
}

/// The spoken names of each casing, longest first where one starts another.
const NAMES: &[(&[&str], Case)] = &[
    (&["screaming", "snake", "case"], Case::Constant),
    (&["constant", "case"], Case::Constant),
    (&["camel", "case"], Case::Camel),
    (&["pascal", "case"], Case::Pascal),
    (&["snake", "case"], Case::Snake),
    (&["kebab", "case"], Case::Kebab),
    (&["dash", "case"], Case::Kebab),
    (&["title", "case"], Case::Title),
    (&["all", "caps"], Case::Upper),
    (&["upper", "case"], Case::Upper),
    (&["lower", "case"], Case::Lower),
    (&["no", "spaces"], Case::Smash),
];

impl Case {
    fn apply(self, words: &[String]) -> String {
        let lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
        let capital = |w: &str| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        };
        match self {
            Case::Camel => lower
                .iter()
                .enumerate()
                .map(|(i, w)| if i == 0 { w.clone() } else { capital(w) })
                .collect(),
            Case::Pascal => lower.iter().map(|w| capital(w)).collect(),
            Case::Snake => lower.join("_"),
            Case::Kebab => lower.join("-"),
            Case::Constant => lower.join("_").to_uppercase(),
            Case::Title => lower
                .iter()
                .map(|w| capital(w))
                .collect::<Vec<_>>()
                .join(" "),
            Case::Upper => lower.join(" ").to_uppercase(),
            Case::Lower => lower.join(" "),
            Case::Smash => lower.concat(),
        }
    }
}

/// A word as said: its letters and digits, and the mark after it, if any.
fn split_mark(word: &str) -> (&str, &str) {
    let end = word.rfind(char::is_alphanumeric).map_or(0, |i| {
        i + word[i..].chars().next().map_or(1, char::len_utf8)
    });
    let start = word.find(char::is_alphanumeric).unwrap_or(end);
    (&word[start..end], &word[end..])
}

/// `text` with each spoken casing applied to the words it names.
pub fn apply(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let bare: Vec<String> = words
        .iter()
        .map(|w| split_mark(w).0.to_lowercase())
        .collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut at = 0;
    'words: while at < words.len() {
        for (name, case) in NAMES {
            let end = at + name.len();
            let said = bare.get(at..end).unwrap_or_default();
            if !said.iter().map(String::as_str).eq(name.iter().copied()) {
                continue;
            }
            // The name's last word ends a phrase ("camel case."): nothing to case.
            if !split_mark(words[end - 1]).1.is_empty() {
                continue;
            }
            let mut phrase = Vec::new();
            let mut mark = "";
            let mut next = end;
            while next < words.len() {
                let (word, after) = split_mark(words[next]);
                if !word.is_empty() {
                    phrase.push(word.to_owned());
                }
                next += 1;
                if !after.is_empty() {
                    mark = after;
                    break;
                }
            }
            if phrase.is_empty() {
                continue;
            }
            out.push(format!("{}{mark}", case.apply(&phrase)));
            at = next;
            continue 'words;
        }
        out.push(words[at].to_owned());
        at += 1;
    }
    // Untouched text keeps its spacing exactly.
    if out.len() == words.len() {
        return text.to_owned();
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_casing_joins_the_words_after_its_name() {
        for (said, typed) in [
            ("camel case user name", "userName"),
            ("Pascal case user name", "UserName"),
            ("snake case user name", "user_name"),
            ("kebab case user name", "user-name"),
            ("dash case main menu", "main-menu"),
            ("constant case max retries", "MAX_RETRIES"),
            ("screaming snake case max retries", "MAX_RETRIES"),
            ("title case the long goodbye", "The Long Goodbye"),
            ("all caps do not touch", "DO NOT TOUCH"),
            ("lower case Hello There", "hello there"),
            ("no spaces Open Street Map", "openstreetmap"),
        ] {
            assert_eq!(apply(said), typed, "{said}");
        }
    }

    #[test]
    fn the_casing_stops_at_a_mark_and_keeps_it() {
        assert_eq!(
            apply("Call camel case get user name, then return."),
            "Call getUserName, then return."
        );
        assert_eq!(apply("Set snake case max size."), "Set max_size.");
    }

    #[test]
    fn text_with_no_casing_keeps_its_spacing() {
        assert_eq!(apply("  a   b "), "  a   b ");
        assert_eq!(apply("x  snake case a b"), "x a_b");
    }

    #[test]
    fn a_casing_name_with_nothing_after_it_is_left_as_said() {
        for said in [
            "We use camel case.",
            "Is it snake case, or kebab case?",
            "camel case",
            "The camel crossed.",
        ] {
            assert_eq!(apply(said), said);
        }
    }
}
