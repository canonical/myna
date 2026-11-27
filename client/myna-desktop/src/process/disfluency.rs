//! Stutters and filled pauses, removed without a model.
//!
//! Only what no speaker means is touched: an immediate repeat of one to
//! four words and a vocalised pause (`um`, `euh`, `ähm`). Words that are
//! sometimes a pause and sometimes speech (`like`, `so`, `well`) and
//! self-repairs are left for the model step behind this one.

/// Whose pauses to strip. `Any` holds only those that are no word in any
/// supported language, for text whose language is not known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Any,
    En,
}

impl Lang {
    fn is_pause(self, word: &str) -> bool {
        let word = word.to_lowercase();
        match self {
            Lang::Any => matches!(
                word.as_str(),
                "um" | "uh" | "uhm" | "erm" | "euh" | "äh" | "ähm"
            ),
            Lang::En => matches!(word.as_str(), "um" | "uh" | "uhm" | "erm" | "hmm"),
        }
    }
}

/// Words said twice on purpose, and negations, whose doubling is not ours
/// to undo. Anything else said twice in a row is a stutter, except a name
/// (capitalised both times, which a speech model never does for a stutter)
/// and a run of three or more content words (a chant).
const REDUPLICATION: &[&str] = &[
    "very", "so", "really", "had", "that", "bye", "no", "way", "fifty", "night", "knock", "la",
    "ha", "now", "there", "well", "yes", "not", "never", "nor",
];

const FUNCTION_WORDS: &[&str] = &[
    "i", "we", "you", "he", "she", "it", "they", "the", "a", "an", "and", "but", "or", "to", "of",
    "in", "on", "at", "for", "with", "is", "are", "was", "my", "your", "our", "if",
];

const STOPS: &[char] = &['.', '!', '?', ';', ':', '\u{2026}'];
const MARKS: &[char] = &['.', ',', ';', ':', '!', '?', '\u{2026}'];
const OPENERS: &[char] = &['"', '\'', '(', '[', '\u{201c}', '\u{2018}'];
const CLOSERS: &[char] = &['"', '\'', ')', ']', '\u{201d}', '\u{2019}'];

/// A whitespace-delimited token split into the word and what surrounds it.
#[derive(Clone)]
struct Tok {
    lead: String,
    word: String,
    trail: String,
}

impl Tok {
    fn new(raw: &str) -> Self {
        let body = raw.trim_start_matches(OPENERS);
        let lead = &raw[..raw.len() - body.len()];
        let word = body
            .trim_end_matches(|c| MARKS.contains(&c) || CLOSERS.contains(&c))
            .len();
        Tok {
            lead: lead.to_owned(),
            word: body[..word].to_owned(),
            trail: body[word..].to_owned(),
        }
    }

    fn closes_sentence(&self) -> bool {
        self.trail.contains(STOPS)
    }

    fn render(&self) -> String {
        format!("{}{}{}", self.lead, self.word, self.trail)
    }
}

fn name(word: &str) -> bool {
    word != "I" && word.chars().next().is_some_and(char::is_uppercase)
}

/// Whether the `k` tokens at `at` and the `k` after spell one stutter.
fn stutter(toks: &[Tok], at: usize, k: usize) -> bool {
    let (a, b) = (&toks[at..at + k], &toks[at + k..at + 2 * k]);
    let same = a
        .iter()
        .zip(b)
        .all(|(x, y)| !x.word.is_empty() && x.word.to_lowercase() == y.word.to_lowercase());
    if !same || a.iter().any(Tok::closes_sentence) || b.iter().all(|t| name(&t.word)) {
        return false;
    }
    if k > 1 {
        return true;
    }
    let w = a[0].word.to_lowercase();
    if REDUPLICATION.contains(&w.as_str()) || w.ends_with("n't") || w.ends_with("n\u{2019}t") {
        return false;
    }
    let same = |t: &&Tok| t.word.to_lowercase() == w;
    let run = toks[..at].iter().rev().take_while(same).count()
        + toks[at..].iter().take_while(same).count();
    run < 3 || FUNCTION_WORDS.contains(&w.as_str())
}

/// Drops the second copy of every immediate repeat, longest phrase first so
/// that "in the in the" is one repeat of two words. The first copy keeps its
/// casing and opening mark and takes the second copy's closing mark.
fn collapse(toks: &mut Vec<Tok>) -> bool {
    let mut edited = false;
    for k in (1..=4).rev() {
        let mut at = 0;
        while at + 2 * k <= toks.len() {
            if stutter(toks, at, k) {
                toks[at + k - 1].trail = toks[at + 2 * k - 1].trail.clone();
                toks.drain(at + k..at + 2 * k);
                edited = true;
            } else {
                at += 1;
            }
        }
    }
    edited
}

/// Drops each pause with the commas around it. A sentence mark on the pause
/// ("tomorrow, um.") ends the word before it, and a sentence that lost its
/// first word capitalises the next.
fn unpause(toks: Vec<Tok>, lang: Lang) -> (Vec<Tok>, bool) {
    let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
    let (mut edited, mut capital) = (false, false);
    for tok in toks {
        if !(tok.lead.is_empty() && lang.is_pause(&tok.word)) {
            if capital && !tok.word.chars().any(char::is_uppercase) {
                let mut chars = tok.word.chars();
                let first: String = chars
                    .next()
                    .into_iter()
                    .flat_map(char::to_uppercase)
                    .collect();
                out.push(Tok {
                    word: first + chars.as_str(),
                    ..tok
                });
            } else {
                out.push(tok);
            }
            capital = false;
            continue;
        }
        edited = true;
        if let Some(prev) = out.last_mut() {
            if tok.closes_sentence() {
                prev.trail = tok.trail.clone();
            } else if tok.trail.contains(',') && prev.trail == "," {
                prev.trail.clear();
            }
        }
        capital = capital || out.last().is_none_or(Tok::closes_sentence);
    }
    (out, edited)
}

pub fn clean(text: &str, lang: Lang) -> String {
    let mut toks: Vec<Tok> = text.split_whitespace().map(Tok::new).collect();
    let collapsed = collapse(&mut toks);
    let (toks, paused) = unpause(toks, lang);
    if !(collapsed || paused) || toks.is_empty() {
        return text.to_owned();
    }
    let lead = &text[..text.len() - text.trim_start().len()];
    let tail = &text[text.trim_end().len()..];
    let words: Vec<String> = toks.iter().map(Tok::render).collect();
    format!("{lead}{}{tail}", words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn en(text: &str) -> String {
        clean(text, Lang::En)
    }

    #[test]
    fn a_stutter_keeps_one_copy_and_the_first_ones_case() {
        assert_eq!(en("I I think so."), "I think so.");
        assert_eq!(
            en("We meet in the in the morning."),
            "We meet in the morning."
        );
        assert_eq!(en("I, I think so."), "I think so.");
        assert_eq!(en("Say echo echo now."), "Say echo now.");
        assert_eq!(
            en("We need the report the report today."),
            "We need the report today."
        );
        assert_eq!(en("Send the report report."), "Send the report.");
    }

    #[test]
    fn a_repeat_made_on_purpose_stays() {
        for kept in [
            "It was very very cold.",
            "That is a very, very good idea.",
            "She said bye bye and left.",
            "We had had enough.",
            "I know that that is true.",
            "A fifty fifty chance.",
            "Stop. Stop. Go.",
            "Knock knock, who is there?",
        ] {
            assert_eq!(en(kept), kept);
        }
    }

    #[test]
    fn a_pause_goes_with_its_commas() {
        assert_eq!(en("Um, we should go."), "We should go.");
        assert_eq!(en("We, uh, met."), "We met.");
        assert_eq!(en("I'll call her tomorrow, um."), "I'll call her tomorrow.");
        assert_eq!(en("Fine. Uh, next."), "Fine. Next.");
        assert_eq!(en("It was, hmm, odd."), "It was odd.");
        assert_eq!(
            en("Um, iPhone is on."),
            "IPhone is on.".replace("IPhone", "iPhone")
        );
    }
}
