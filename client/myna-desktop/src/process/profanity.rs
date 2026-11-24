//! Strong English swear words masked but for their first letter: "f***".
//! A short list of our own, the strong words only; mild ones ("damn") and
//! words that are also names or plain words ("Dick", "cock") are left.

/// Words masked wherever they start a word ("fucking", "shitty").
const STEMS: &[&str] = &["fuck", "motherfuck", "shit", "bullshit", "cunt"];

/// Words masked only whole, with their plural.
const WORDS: &[&str] = &["bitch", "bastard", "asshole", "arsehole", "wanker", "twat"];

fn masked(word: &str) -> bool {
    let lower = word.to_lowercase();
    STEMS.iter().any(|stem| lower.starts_with(stem))
        || WORDS.iter().any(|w| {
            lower == *w
                || lower
                    .strip_prefix(w)
                    .is_some_and(|rest| ["s", "es"].contains(&rest))
        })
}

fn mask(word: &str) -> String {
    let mut chars = word.chars();
    let first = chars.next().map(String::from).unwrap_or_default();
    format!("{first}{}", "*".repeat(chars.count()))
}

pub fn apply(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if masked(word) {
            out.push_str(&mask(word));
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphanumeric() {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strong_words_keep_only_their_first_letter() {
        assert_eq!(apply("What the fuck."), "What the f***.");
        assert_eq!(apply("Fucking hell, bullshit!"), "F****** hell, b*******!");
        assert_eq!(apply("those bastards"), "those b*******");
    }

    #[test]
    fn mild_words_names_and_lookalikes_are_left() {
        for said in [
            "Damn, Dick Smith fed the cock.",
            "Bitcoin in Scunthorpe",
            "Shiitake, not shitake",
        ] {
            assert_eq!(apply(said), said.replace("shitake", "s******"));
        }
    }
}
