//! Fitting text to the field it lands in: the last change before a commit,
//! after any chain and with none, a piece at a time.
//!
//! It reads only the field (its purpose, the character before the text), so
//! it needs no more of the dictation than the piece being committed and text
//! still streams. Where the field says nothing (an unknown purpose and no
//! character before), only the user's vocabulary is spelled: never a guess
//! about where the sentence stands.

use myna_platform::text_input::FieldPurpose;

use super::rules::Rule;
use super::vocabulary::Vocabulary;
use super::Field;

/// `text` fitted to `field`: an address field gets no leading capital and no
/// closing sentence mark; text continuing a sentence gets no capital on a
/// function word; the vocabulary is spelled as written outside addresses.
pub fn fit(text: &str, field: &Field, vocabulary: &Vocabulary) -> String {
    if matches!(field.purpose, FieldPurpose::Url | FieldPurpose::Email) {
        return Rule::Address.apply(text);
    }
    let text = if field.mid_sentence {
        Rule::Continue.apply(text)
    } else {
        text.to_owned()
    };
    // After the recasing, so a term keeps its capitals.
    vocabulary.apply(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(purpose: FieldPurpose, before: Option<char>) -> Field {
        Field::after(purpose, before)
    }

    fn myna() -> Vocabulary {
        Vocabulary::new(&["Myna".to_owned()])
    }

    #[test]
    fn an_address_field_gets_no_capital_and_no_full_stop() {
        for purpose in [FieldPurpose::Url, FieldPurpose::Email] {
            assert_eq!(
                fit("Example.com.", &after(purpose, Some('x')), &myna()),
                "example.com"
            );
        }
    }

    #[test]
    fn a_sentence_carried_on_loses_the_capital_of_its_function_word() {
        let mid = after(FieldPurpose::Plain, Some('x'));
        assert_eq!(fit("The myna sang.", &mid, &myna()), "the Myna sang.");
        assert_eq!(fit("NASA called.", &mid, &myna()), "NASA called.");
        let comma = after(FieldPurpose::Plain, Some(','));
        assert_eq!(fit("And then.", &comma, &myna()), "and then.");
    }

    #[test]
    fn a_new_sentence_keeps_its_capital_and_gets_the_vocabulary() {
        let new = after(FieldPurpose::Plain, Some('.'));
        assert_eq!(fit("The myna sang.", &new, &myna()), "The Myna sang.");
    }

    /// No purpose and nothing before: no guess, only the spelling.
    #[test]
    fn an_unknown_field_is_left_as_heard_but_for_the_vocabulary() {
        let unknown = after(FieldPurpose::default(), None);
        assert_eq!(fit("The myna sang", &unknown, &myna()), "The Myna sang");
        assert_eq!(
            fit("The bird sang", &unknown, &Vocabulary::default()),
            "The bird sang"
        );
    }
}
