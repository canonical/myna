//! Snippets: "insert my address" types the text the user saved under "my
//! address" (`my address => 1 Main Street`), and "insert today's date" or
//! "insert the time" type the date or time now. Saying "insert" first is
//! what keeps "what's today's date" typed as said.

/// The values Myna knows itself, by the names that ask for them.
const DATE: [&str; 4] = ["today's date", "todays date", "the date", "date"];
const TIME: [&str; 3] = ["the time", "time", "the current time"];

/// The date and time now, as the user's locale writes them.
pub struct Now {
    pub date: String,
    pub time: String,
}

/// Now, from the local clock: the date as "24 November 2026", which no
/// reader takes for another day (the locale's `%x` is "11/24/26" in the
/// US), with the month in the locale's language.
pub fn now() -> Now {
    let now = gio::glib::DateTime::now_local().ok();
    let format = |f: &str| {
        now.as_ref()
            .and_then(|n| n.format(f).ok())
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    Now {
        date: format("%e %B %Y").trim_start().to_owned(),
        time: format("%R"),
    }
}

/// The words of `text`, lowercased, for matching names.
fn key(text: &str) -> String {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '\u{2019}')
                .replace('\u{2019}', "'")
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text` with every "insert <name>" replaced by what the name stands for;
/// `now` is asked only when a date or time is.
pub fn apply(text: &str, entries: &[String], now: impl Fn() -> Now) -> String {
    let saved: Vec<(String, &str)> = entries
        .iter()
        .filter_map(|e| myna_core::spoken_written(e))
        .map(|(name, typed)| (key(name), typed))
        .filter(|(name, _)| !name.is_empty())
        .collect();
    // Word spans of `text`.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices().chain([(text.len(), ' ')]) {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                spans.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut at = 0;
    let mut clock: Option<Now> = None;
    while at < spans.len() {
        let (s, e) = spans[at];
        if key(&text[s..e]) != "insert" {
            at += 1;
            continue;
        }
        // The longest name after "insert", up to eight words.
        let found = (at + 1..spans.len().min(at + 9)).rev().find_map(|last| {
            let said = &text[spans[at + 1].0..spans[last].1];
            let name = key(said);
            let saved = saved
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, typed)| (*typed).to_owned());
            let known = || {
                let now = clock.get_or_insert_with(&now);
                if DATE.contains(&name.as_str()) {
                    Some(now.date.clone())
                } else if TIME.contains(&name.as_str()) {
                    Some(now.time.clone())
                } else {
                    None
                }
            };
            saved
                .or_else(|| {
                    (DATE.contains(&name.as_str()) || TIME.contains(&name.as_str()))
                        .then(known)
                        .flatten()
                })
                .map(|typed| (last, typed))
        });
        match found {
            Some((last, typed)) if !typed.is_empty() => {
                out.push_str(&text[copied..s]);
                // What led into "insert" and what followed the name stay:
                // "(insert the date)." keeps its brackets and stop.
                let first = &text[s..e];
                out.push_str(&first[..first.find(char::is_alphanumeric).unwrap_or(0)]);
                out.push_str(&typed);
                let (_, end) = spans[last];
                let tail = &text[spans[last].0..end];
                let keep = tail
                    .rfind(|c: char| c.is_alphanumeric() || c == '\'')
                    .map_or(tail.len(), |i| i + 1);
                out.push_str(&tail[keep..]);
                copied = end;
                at = last + 1;
            }
            _ => at += 1,
        }
    }
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed() -> Now {
        Now {
            date: "24/11/2026".into(),
            time: "10:30".into(),
        }
    }

    fn mine() -> Vec<String> {
        [
            "my address => 1 Main Street, Springfield",
            "sign off => Best, Charles",
            "date => not the clock",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn insert_and_a_saved_name_types_the_snippet() {
        assert_eq!(
            apply("Send it to insert my address.", &mine(), fixed),
            "Send it to 1 Main Street, Springfield."
        );
        assert_eq!(
            apply("Thanks again. Insert sign off", &mine(), fixed),
            "Thanks again. Best, Charles"
        );
    }

    #[test]
    fn the_date_and_time_need_no_snippet() {
        assert_eq!(
            apply("Due insert today's date at insert the time.", &[], fixed),
            "Due 24/11/2026 at 10:30."
        );
        assert_eq!(apply("(Insert the date)", &[], fixed), "(24/11/2026)");
    }

    #[test]
    fn a_saved_name_wins_over_a_value_of_the_same_name() {
        assert_eq!(apply("insert date", &mine(), fixed), "not the clock");
    }

    #[test]
    fn without_insert_or_with_an_unknown_name_nothing_changes() {
        for said in [
            "What's today's date?",
            "My address is on file.",
            "Insert the card here.",
            "insert",
        ] {
            assert_eq!(apply(said, &mine(), || panic!("no clock needed")), said);
        }
    }

    #[test]
    fn the_date_names_its_month_and_the_whole_year() {
        let now = now();
        assert!(
            now.date.rsplit(' ').next().is_some_and(|y| y.len() == 4),
            "{}",
            now.date
        );
        assert!(!now.date.starts_with(' '));
        assert_eq!(now.time.len(), 5, "{}", now.time);
    }

    #[test]
    fn a_clock_that_could_not_be_read_types_what_was_said() {
        let unread = || Now {
            date: String::new(),
            time: String::new(),
        };
        assert_eq!(
            apply("Due insert the date.", &[], unread),
            "Due insert the date."
        );
    }

    #[test]
    fn the_clock_is_read_only_when_asked() {
        assert_eq!(
            apply("insert my address", &mine(), || panic!("not asked")),
            "1 Main Street, Springfield"
        );
    }
}
