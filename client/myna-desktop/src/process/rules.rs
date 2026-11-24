//! Deterministic changes to a piece of text: no model, no budget.

use super::disfluency::{self, Lang};
use super::vocabulary::Vocabulary;

/// Words that take a capital only at the start of a sentence, whatever the
/// text around them.
const FUNCTION_WORDS: &[&str] = &[
    "the", "a", "an", "and", "but", "or", "so", "to", "of", "in", "on", "at", "for", "with", "it",
    "this", "that", "we", "you", "they", "he", "she", "is", "are", "was",
];

/// The user's lists the rules read.
pub struct Lists<'a> {
    pub vocabulary: &'a Vocabulary,
    /// `name => text` entries.
    pub snippets: &'a [String],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Fit an address field: no capital the speech model put at the start
    /// and no sentence mark at the end.
    Address,
    /// Continue a sentence: a function word needs no capital after the
    /// words before it.
    Continue,
    /// Drop stutters and unambiguous filled pauses.
    Disfluency(Lang),
    /// Spell the user's vocabulary as written; see [`Rule::apply_with`].
    Vocabulary,
    /// Type spoken emoji ("thumbs up emoji") as the emoji.
    Emoji,
    /// Type the user's snippets and the date or time for "insert <name>".
    Snippets,
    /// Case and join the words after a spoken casing ("camel case").
    Casing,
    /// Type spoken web and email addresses as addresses.
    Addresses,
    /// Type spoken numbers as digits.
    Numbers,
    /// Quotes, apostrophes, dashes and spacing as the language writes them.
    Typography(super::typography::Style),
    /// Carry out spoken editing commands ("scratch that"), which reach
    /// back over the dictation.
    Commands,
}

impl Rule {
    /// How much of the dictation the rule has to see at once.
    pub fn needs(self) -> super::graph::Needs {
        match self {
            Rule::Commands => super::graph::Needs::Dictation,
            _ => super::graph::Needs::Sentence,
        }
    }

    pub fn apply(self, text: &str) -> String {
        self.apply_with(
            text,
            &Lists {
                vocabulary: &Vocabulary::default(),
                snippets: &[],
            },
        )
    }

    /// `apply`, with the user's lists for the rules that read them.
    pub fn apply_with(self, text: &str, lists: &Lists) -> String {
        match self {
            Rule::Vocabulary => lists.vocabulary.apply(text),
            Rule::Snippets => super::snippets::apply(text, lists.snippets, super::snippets::now),
            Rule::Address => {
                let bare = text.trim().trim_end_matches(['.', '!', '?', '\u{2026}']);
                decapitalise(bare, |_| true)
            }
            Rule::Disfluency(lang) => disfluency::clean(text, lang),
            Rule::Emoji => super::emoji::apply(text),
            Rule::Casing => super::casing::apply(text),
            Rule::Addresses => super::addresses::apply(text),
            Rule::Numbers => super::numbers::apply(text),
            Rule::Typography(style) => super::typography::apply(text, style),
            Rule::Commands => super::commands::apply(text),
            Rule::Continue => decapitalise(text, |word| {
                FUNCTION_WORDS.contains(&word.to_lowercase().as_str())
            }),
        }
    }
}

/// `text` with its first word lowercased when it is capitalised and nothing
/// else (`The`, not `NASA` or `iPhone`) and `allowed` says so.
fn decapitalise(text: &str, allowed: impl Fn(&str) -> bool) -> String {
    let start = text.len() - text.trim_start().len();
    let word_len = text[start..]
        .find(|c: char| !c.is_alphanumeric())
        .unwrap_or(text.len() - start);
    let word = &text[start..start + word_len];
    let mut chars = word.chars();
    let titlecase = chars.next().is_some_and(char::is_uppercase)
        && !chars.clone().any(char::is_uppercase)
        && chars.next().is_some();
    if !titlecase || !allowed(word) {
        return text.to_owned();
    }
    let first = word.chars().next().map_or(0, char::len_utf8);
    format!(
        "{}{}{}",
        &text[..start],
        word[..first].to_lowercase(),
        &text[start + first..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_loses_its_capital_and_its_full_stop() {
        assert_eq!(Rule::Address.apply("Example.com."), "example.com");
        assert_eq!(
            Rule::Address.apply(" Maria@example.com! "),
            "maria@example.com"
        );
        assert_eq!(Rule::Address.apply("Wait..."), "wait");
    }

    #[test]
    fn a_name_or_an_acronym_keeps_its_capitals() {
        assert_eq!(Rule::Address.apply("NASA.gov"), "NASA.gov");
        assert_eq!(Rule::Address.apply("iPhone.example"), "iPhone.example");
        assert_eq!(
            Rule::Address.apply("A"),
            "A",
            "a lone letter is no word to recase"
        );
    }

    #[test]
    fn a_continued_sentence_lowercases_only_a_function_word() {
        assert_eq!(Rule::Continue.apply("The cat sat."), "the cat sat.");
        assert_eq!(
            Rule::Continue.apply("And then we left."),
            "and then we left."
        );
        assert_eq!(Rule::Continue.apply("Paris is nice."), "Paris is nice.");
        assert_eq!(Rule::Continue.apply("I agree."), "I agree.");
        assert_eq!(Rule::Continue.apply("  It's fine."), "  it's fine.");
        assert_eq!(Rule::Continue.apply(""), "");
        assert_eq!(
            Rule::Continue.apply("  The"),
            "  the",
            "a lone word after blanks"
        );
    }
}
