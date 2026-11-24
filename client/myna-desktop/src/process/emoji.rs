//! Spoken emoji: "thumbs up emoji" is typed as 👍. A name is the emoji's
//! Unicode name or one of its GitHub shortcodes, in English, said before
//! the word "emoji"; a name Myna does not know is left as said.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The most words a name may have.
const MAX_WORDS: usize = 6;

/// Every name, lowercased and with its words single-spaced, to its emoji.
fn names() -> &'static HashMap<String, &'static str> {
    static NAMES: OnceLock<HashMap<String, &'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = HashMap::new();
        for emoji in emojis::iter() {
            let spoken = std::iter::once(emoji.name()).chain(emoji.shortcodes());
            for name in spoken {
                let words = words(name).join(" ");
                // The first emoji to take a name keeps it: Unicode orders
                // the common ones first.
                names.entry(words).or_insert(emoji.as_str());
            }
        }
        names
    })
}

/// The words of a name or of what was said, lowercased.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == '_' || c == '-')
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// `text` with each spoken emoji typed as the emoji.
pub fn apply(text: &str) -> String {
    // Word spans: (start, end) of each run of non-space characters.
    let spans: Vec<(usize, usize)> = text
        .char_indices()
        .filter(|&(i, c)| {
            !c.is_whitespace() && (i == 0 || text[..i].ends_with(char::is_whitespace))
        })
        .map(|(start, _)| {
            let end = text[start..]
                .find(char::is_whitespace)
                .map_or(text.len(), |len| start + len);
            (start, end)
        })
        .collect();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    // The first word a name may start at: none of what is already typed.
    let mut free = 0;
    for (at, &(start, end)) in spans.iter().enumerate() {
        let word = &text[start..end];
        let bare = word.trim_matches(|c: char| !c.is_alphanumeric());
        if !bare.eq_ignore_ascii_case("emoji") {
            continue;
        }
        let first = at.saturating_sub(MAX_WORDS).max(free);
        let found = (first..at).find_map(|from| {
            let said = words(&text[spans[from].0..spans[at - 1].1]).join(" ");
            names().get(&said).map(|emoji| (from, *emoji))
        });
        if let Some((from, emoji)) = found {
            out.push_str(&text[copied..spans[from].0]);
            // What led into the name ("“") and what followed "emoji"
            // (".", ",") stay.
            let lead = &text[spans[from].0..][..text[spans[from].0..]
                .find(char::is_alphanumeric)
                .unwrap_or(0)];
            let tail_at = word.rfind(char::is_alphanumeric).map_or(0, |i| i + 1);
            out.push_str(lead);
            out.push_str(emoji);
            out.push_str(&word[tail_at..]);
            copied = end;
            free = at + 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_said_before_emoji_is_typed_as_the_emoji() {
        assert_eq!(apply("Great job thumbs up emoji"), "Great job 👍");
        assert_eq!(apply("Thumbs up emoji."), "👍.");
        assert_eq!(apply("I love it, red heart emoji!"), "I love it, ❤️!");
    }

    #[test]
    fn a_shortcode_is_a_name_too() {
        assert_eq!(apply("see you smile emoji"), "see you 😄");
        assert_eq!(apply("tada emoji we shipped"), "🎉 we shipped");
    }

    #[test]
    fn a_shortcode_said_with_spaces_for_underscores_is_found() {
        assert_eq!(apply("heart eyes emoji"), "\u{1f60d}");
    }

    #[test]
    fn the_longest_name_before_emoji_wins() {
        // "face" alone is no emoji; "face with tears of joy" is.
        assert_eq!(apply("lol face with tears of joy emoji"), "lol 😂");
    }

    #[test]
    fn an_unknown_name_or_a_bare_emoji_is_left_as_said() {
        for said in [
            "the emoji keyboard",
            "an emoji",
            "Emoji are fun.",
            "blorptastic emoji",
            "no emoji here",
        ] {
            assert_eq!(apply(said), said);
        }
    }

    #[test]
    fn case_hyphens_and_spacing_do_not_matter() {
        assert_eq!(apply("Thumbs-Up Emoji"), "👍");
        assert_eq!(apply("rocket emoji  and fire emoji"), "🚀  and 🔥");
    }

    #[test]
    fn a_name_never_reaches_back_over_an_emoji_already_typed() {
        assert_eq!(apply("fire emoji emoji"), "🔥 emoji");
    }
}
