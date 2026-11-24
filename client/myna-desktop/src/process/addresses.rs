//! Spoken web and email addresses: "example dot com" is typed as
//! `example.com`, "john dot smith at example dot co dot uk" as
//! `john.smith@example.co.uk`, "example dot com slash docs" as
//! `example.com/docs`.
//!
//! An address ends in a known top-level domain, so "the dot on the i" is
//! left alone. "at" makes an email only after a word that introduces one
//! ("email", "is", "to"...) or at the start of a sentence: "I looked at
//! example dot com" keeps its "at".

/// Top-level domains an address may end in.
const TLDS: &[&str] = &[
    "com", "org", "net", "io", "dev", "app", "ai", "edu", "gov", "co", "uk", "us", "ca", "de",
    "fr", "es", "it", "nl", "eu", "be", "ch", "at", "se", "no", "dk", "fi", "pl", "pt", "ie", "au",
    "nz", "jp", "cn", "in", "br", "mx", "info", "biz", "me", "tv", "xyz", "tech", "cloud",
    "ubuntu", "page", "blog", "site", "online", "store",
];

/// Words that may come just before the name of an email's sender part.
const EMAIL_CUES: &[&str] = &[
    "email", "e-mail", "mail", "address", "is", "to", "me", "contact", "reach", "write", "cc",
    "from", "at", "or", "and",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Joiner {
    Dot,
    At,
    Dash,
    Underscore,
    Slash,
}

impl Joiner {
    fn of(word: &str) -> Option<Self> {
        Some(match word {
            "dot" => Joiner::Dot,
            "at" => Joiner::At,
            "dash" | "hyphen" => Joiner::Dash,
            "underscore" => Joiner::Underscore,
            "slash" => Joiner::Slash,
            _ => return None,
        })
    }

    fn mark(self) -> char {
        match self {
            Joiner::Dot => '.',
            Joiner::At => '@',
            Joiner::Dash => '-',
            Joiner::Underscore => '_',
            Joiner::Slash => '/',
        }
    }
}

/// A word: what was said, lowercased, and the marks around it.
struct Word<'a> {
    raw: &'a str,
    bare: String,
    /// Before the word: "(" of "(example".
    lead: &'a str,
    /// After the word: "." or "," of "com.".
    tail: &'a str,
}

fn word(raw: &str) -> Word<'_> {
    let start = raw.find(char::is_alphanumeric).unwrap_or(raw.len());
    let end = raw.rfind(char::is_alphanumeric).map_or(start, |i| {
        i + raw[i..].chars().next().map_or(1, char::len_utf8)
    });
    Word {
        raw,
        bare: raw[start..end].to_lowercase(),
        lead: &raw[..start],
        tail: &raw[end..],
    }
}

/// A word that may be part of an address: letters and digits only.
fn part(w: &Word) -> bool {
    !w.bare.is_empty()
        && w.bare.chars().all(|c| c.is_ascii_alphanumeric())
        && Joiner::of(&w.bare).is_none()
}

pub fn apply(text: &str) -> String {
    let raw: Vec<&str> = text.split_whitespace().collect();
    let words: Vec<Word> = raw.iter().map(|r| word(r)).collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut at = 0;
    while at < words.len() {
        match address(&words, at) {
            Some((end, typed)) => {
                out.push(typed);
                at = end;
            }
            None => {
                out.push(words[at].raw.to_owned());
                at += 1;
            }
        }
    }
    let typed = out.join(" ");
    if typed.split_whitespace().eq(text.split_whitespace()) {
        return text.to_owned();
    }
    typed
}

/// The address starting at word `start`, if one does: the index after its
/// last word, and how it is typed.
fn address(words: &[Word], start: usize) -> Option<(usize, String)> {
    if !part(&words[start]) || !words[start].tail.is_empty() {
        return None;
    }
    // Parts and the joiners between them, as far as they go: (index of
    // the joiner word, joiner).
    let mut joiners: Vec<(usize, Joiner)> = Vec::new();
    let mut next = start + 1;
    while next + 1 < words.len() {
        let (joiner, after) = (&words[next], &words[next + 1]);
        let Some(kind) = Joiner::of(&joiner.bare) else {
            break;
        };
        if !joiner.tail.is_empty() || !joiner.lead.is_empty() || !part(after) {
            break;
        }
        joiners.push((next, kind));
        next += 2;
        if !after.tail.is_empty() {
            break;
        }
    }
    // The domain ends at the last dot before a known top-level domain;
    // past it, only a path that a slash leads into.
    let tld = joiners
        .iter()
        .rposition(|(i, j)| *j == Joiner::Dot && TLDS.contains(&words[i + 1].bare.as_str()))?;
    let mut last = tld + 1;
    if joiners.get(last).is_some_and(|(_, j)| *j == Joiner::Slash) {
        last = joiners[last..]
            .iter()
            .position(|(_, j)| *j == Joiner::At)
            .map_or(joiners.len(), |p| last + p);
    }
    let joiners = &joiners[..last];
    let ats: Vec<usize> = joiners
        .iter()
        .enumerate()
        .filter(|(_, (_, j))| *j == Joiner::At)
        .map(|(n, _)| n)
        .collect();
    match ats.as_slice() {
        [] => {}
        // An email: one "at" before the domain, after a word that
        // introduces it or at a sentence's start.
        [n] if *n < tld => {
            let cued = start == 0
                || !words[start - 1].tail.is_empty()
                || EMAIL_CUES.contains(&words[start - 1].bare.as_str());
            if !cued {
                return None;
            }
        }
        _ => return None,
    }
    let end = joiners.last().map_or(start + 1, |(i, _)| i + 2);
    let mut typed = String::from(words[start].lead);
    typed.push_str(&words[start].bare);
    for (i, joiner) in joiners {
        typed.push(joiner.mark());
        typed.push_str(&words[i + 1].bare);
    }
    typed.push_str(words[end - 1].tail);
    Some((end, typed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spoken_domain_is_typed_as_one() {
        assert_eq!(apply("Go to example dot com."), "Go to example.com.");
        assert_eq!(
            apply("see docs dot ubuntu dot com slash myna"),
            "see docs.ubuntu.com/myna"
        );
        assert_eq!(apply("my site dash two dot io"), "my site-two.io");
    }

    #[test]
    fn an_email_needs_a_cue_before_it() {
        assert_eq!(
            apply("Email john dot smith at example dot co dot uk, thanks"),
            "Email john.smith@example.co.uk, thanks"
        );
        assert_eq!(
            apply("It is ann underscore b at mail dot com"),
            "It is ann_b@mail.com"
        );
        assert_eq!(
            apply("I looked at example dot com."),
            "I looked at example.com."
        );
    }

    #[test]
    fn no_known_domain_no_address() {
        for said in [
            "the dot on the i",
            "meet at noon dot",
            "a dot b dot c",
            "Example.com is fine.",
        ] {
            assert_eq!(apply(said), said);
        }
    }
}
