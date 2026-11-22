//! What a model's answer must satisfy to be typed in place of what was
//! heard. A failed check names a [`Skip`] code and nothing else.

use std::collections::HashSet;

use myna_core::history::Skip;

/// Openings of a chat reply rather than of the text asked for.
const CHATTY: &[&str] = &["sure", "here is", "here's", "certainly", "of course"];

/// The longest run of words a cleanup may drop at once.
const LONGEST_DROP: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// Not empty.
    Present,
    /// A cleanup, whatever the model: the answer keeps what was said in
    /// order and only drops, recases and repunctuates, within bounds. The
    /// user's own words may stand in for what the speech model misheard.
    ConservativeEdit,
    /// A rewrite or translation: not empty, of a plausible length, and not
    /// a chat reply.
    Usable { translation: bool },
}

impl Check {
    /// Whether `answer` may replace `heard`; `written` is the user's own
    /// vocabulary.
    pub fn verdict(self, heard: &str, answer: &str, written: &[String]) -> Result<(), Skip> {
        if answer.trim().is_empty() {
            return Err(Skip::Empty);
        }
        match self {
            Check::Present => Ok(()),
            Check::ConservativeEdit => conservative_edit(heard, answer, written),
            Check::Usable { translation } => usable(heard, answer, translation),
        }
    }
}

/// A word of `text`, lowercased and stripped of punctuation; an apostrophe
/// inside a word stays, so "shouldn't" can be told from "should".
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            let kept: String = word
                .chars()
                .map(|c| if c == '\u{2019}' { '\'' } else { c })
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .flat_map(char::to_lowercase)
                .collect();
            kept.trim_matches('\'').to_owned()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

fn is_negation(word: &str) -> bool {
    matches!(word, "not" | "never" | "cannot") || word.ends_with("n't")
}

/// Whether dropping `run`, the words between two kept ones, is allowed.
fn droppable(run: &[String]) -> Result<(), Skip> {
    match run {
        [word] if is_negation(word) => Err(Skip::DroppedNegation),
        _ if run.len() > LONGEST_DROP => Err(Skip::LongDrop),
        _ => Ok(()),
    }
}

/// The words of the user's vocabulary a misheard run may become.
fn written_words(written: &[String]) -> HashSet<String> {
    written
        .iter()
        .flat_map(|entry| words(super::vocabulary::written(entry)))
        .collect()
}

/// The longest run of heard words one written word may stand in for:
/// "my sequel" for "MySQL".
const LONGEST_MISHEARING: usize = 3;

fn conservative_edit(heard: &str, answer: &str, written: &[String]) -> Result<(), Skip> {
    let heard = words(heard);
    let answer = words(answer);
    if answer.is_empty() {
        return Err(Skip::Empty);
    }
    let written = written_words(written);
    let n = heard.len();
    // dropped[i][j]: the fewest heard words dropped to keep answer[..j]
    // with heard[i - 1] the last word used, or i == 0 before any.
    let mut dropped = vec![vec![None::<usize>; answer.len() + 1]; n + 1];
    dropped[0][0] = Some(0);
    let mut worst = Skip::NewWord;
    for j in 0..answer.len() {
        for i in 0..=n {
            let Some(so_far) = dropped[i][j] else {
                continue;
            };
            // Up to one past the longest drop, so a run just too long is
            // named rather than read as a new word.
            for dropping in 0..=LONGEST_DROP + 1 {
                let next = i + dropping;
                if next >= n {
                    break;
                }
                let run = &heard[i..next];
                let spans = if heard[next] == answer[j] {
                    1..=1
                } else if written.contains(&answer[j]) {
                    1..=LONGEST_MISHEARING
                } else {
                    continue;
                };
                for span in spans {
                    let end = next + span;
                    if end > n || !sounds_alike(&heard[next..end], &answer[j]) {
                        continue;
                    }
                    if let Err(why) = droppable(run) {
                        worst = nearer(worst, why);
                        continue;
                    }
                    let total = so_far + run.len();
                    let cell = &mut dropped[end][j + 1];
                    *cell = Some(cell.map_or(total, |best| best.min(total)));
                }
            }
        }
    }
    let budget = 2.max(n.div_ceil(2));
    let mut best = None;
    for (i, row) in dropped.iter().enumerate() {
        let Some(so_far) = row[answer.len()] else {
            continue;
        };
        if let Err(why) = droppable(&heard[i..]) {
            worst = nearer(worst, why);
            continue;
        }
        let total = so_far + (n - i);
        best = Some(best.map_or(total, |b: usize| b.min(total)));
    }
    match best {
        Some(total) if total <= budget => Ok(()),
        Some(_) => Err(Skip::TooManyDropped),
        None => Err(worst),
    }
}

/// Of two reasons no alignment worked, the one nearer to passing: a
/// dropped negation or a long drop explains more than "new word".
fn nearer(so_far: Skip, other: Skip) -> Skip {
    if so_far == Skip::NewWord {
        other
    } else {
        so_far
    }
}

/// Whether `run` may be what the speech model made of `word`: it starts
/// as the word does. Crude on purpose; it only keeps a written word from
/// standing in for any three words.
fn sounds_alike(run: &[String], word: &str) -> bool {
    run.first()
        .and_then(|first| first.chars().next())
        .is_some_and(|c| word.starts_with(c))
}

fn usable(heard: &str, answer: &str, translation: bool) -> Result<(), Skip> {
    let heard = heard.trim();
    let answer = answer.trim();
    let (said, got) = (heard.chars().count(), answer.chars().count());
    // got / said within 0.25..=4.0, in integers.
    if said == 0 || got * 4 < said || got > said * 4 {
        return Err(Skip::OddLength);
    }
    if !translation {
        let opens = |text: &str, with: &str| {
            let lower = text.to_lowercase();
            lower.starts_with(with)
                && !lower[with.len()..]
                    .chars()
                    .next()
                    .is_some_and(char::is_alphanumeric)
        };
        if CHATTY
            .iter()
            .any(|with| opens(answer, with) && !opens(heard, with))
        {
            return Err(Skip::Chatty);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tidy(heard: &str, answer: &str) -> Result<(), Skip> {
        Check::ConservativeEdit.verdict(heard, answer, &[])
    }

    #[test]
    fn a_cleanup_may_drop_recase_and_repunctuate() {
        assert_eq!(
            tidy("Um, let's meet Monday, no, Tuesday.", "Let's meet Tuesday."),
            Ok(())
        );
        assert_eq!(tidy("so I I think", "So I think."), Ok(()));
        assert_eq!(tidy("hello world", "Hello, world!"), Ok(()));
    }

    #[test]
    fn a_repair_may_drop_a_negation_with_the_words_around_it() {
        assert_eq!(tidy("Monday, no, Tuesday", "Tuesday"), Ok(()));
        assert_eq!(tidy("we shouldn't, wait, we should", "we should"), Ok(()));
        assert_eq!(tidy("we should not, never mind", "we should"), Ok(()));
    }

    #[test]
    fn a_lone_negation_is_never_dropped() {
        assert_eq!(tidy("do not merge", "do merge"), Err(Skip::DroppedNegation));
        assert_eq!(
            tidy("I never said that", "I said that"),
            Err(Skip::DroppedNegation)
        );
        assert_eq!(
            tidy("it isn't ready", "it ready"),
            Err(Skip::DroppedNegation)
        );
        assert_eq!(
            tidy("it isn\u{2019}t ready", "it ready"),
            Err(Skip::DroppedNegation)
        );
        assert_eq!(tidy("you cannot go", "you go"), Err(Skip::DroppedNegation));
        assert_eq!(tidy("ship it not", "ship it"), Err(Skip::DroppedNegation));
        assert_eq!(tidy("not now", "now"), Err(Skip::DroppedNegation));
    }

    #[test]
    fn a_new_moved_or_doubled_word_fails() {
        assert_eq!(
            tidy("meet Tuesday", "Sure! Meet Tuesday."),
            Err(Skip::NewWord)
        );
        assert_eq!(tidy("meet Tuesday", "Tuesday meet"), Err(Skip::NewWord));
        assert_eq!(
            tidy("meet Tuesday", "meet meet Tuesday"),
            Err(Skip::NewWord)
        );
        assert_eq!(tidy("we should", "we shouldn't"), Err(Skip::NewWord));
        assert_eq!(tidy("", "hello"), Err(Skip::NewWord));
    }

    #[test]
    fn a_cleanup_keeps_at_least_half_and_at_least_one_word() {
        assert_eq!(tidy("one", "..."), Err(Skip::Empty));
        assert_eq!(tidy("one two three four five six", "four five six"), Ok(()));
        assert_eq!(
            tidy("one two three four five six", "five six"),
            Err(Skip::TooManyDropped)
        );
        // Two words may always go, however short the text.
        assert_eq!(tidy("um uh right", "Right."), Ok(()));
    }

    #[test]
    fn at_most_six_words_in_a_row_may_go() {
        let heard = "a b c d e f g h i j k l m n";
        assert_eq!(tidy(heard, "a h i j k l m n"), Ok(()));
        assert_eq!(tidy(heard, "a i j k l m n"), Err(Skip::LongDrop));
        assert_eq!(tidy(heard, "h i j k l m n"), Err(Skip::LongDrop));
        assert_eq!(tidy(heard, "a b c d e f g"), Err(Skip::LongDrop));
    }

    #[test]
    fn repeated_words_align_where_the_drops_are_allowed() {
        assert_eq!(tidy("the the the cat", "the cat"), Ok(()));
        // Keeping the first "the" would drop a lone "not"; keeping the
        // second drops it inside a longer run, which a repair may.
        assert_eq!(tidy("x not the a the cat", "x the cat"), Ok(()));
    }

    #[test]
    fn a_written_word_may_stand_in_for_what_was_misheard() {
        let written = ["MySQL".to_owned(), "Visual Studio Code".to_owned()];
        let tidy = |heard, answer| Check::ConservativeEdit.verdict(heard, answer, &written);
        assert_eq!(
            tidy("um restart my sequel now", "Restart MySQL now."),
            Ok(())
        );
        assert_eq!(
            tidy("open visual studio code", "Open Visual Studio Code."),
            Ok(())
        );
        assert_eq!(
            tidy("restart now", "Restart MySQL now."),
            Err(Skip::NewWord)
        );
        assert_eq!(
            tidy("restart the my sequel server now", "Restart MySQL now."),
            Ok(())
        );
        assert_eq!(tidy("a b c d", "MySQL"), Err(Skip::NewWord));
        let spoken = ["my sequel => MySQL".to_owned()];
        let tidy = |heard, answer| Check::ConservativeEdit.verdict(heard, answer, &spoken);
        assert_eq!(tidy("restart my sequel", "Restart MySQL."), Ok(()));
        assert_eq!(
            tidy("restart now", "Restart my now."),
            Err(Skip::NewWord),
            "the spoken side of an entry is not a written word"
        );
        // The cheapest reading wins: "MySQL" for "m s q" drops two words,
        // "MySQL" for "m" alone would drop four, past the budget of three.
        assert_eq!(tidy("x y m s q k", "MySQL k"), Ok(()));
        assert_eq!(
            tidy("restart a sequel now", "restart MySQL now"),
            Err(Skip::NewWord)
        );
    }

    fn usable(heard: &str, answer: &str) -> Result<(), Skip> {
        Check::Usable { translation: false }.verdict(heard, answer, &[])
    }

    #[test]
    fn a_rewrite_must_be_of_a_plausible_length() {
        assert_eq!(usable("abcd", "a"), Ok(()));
        assert_eq!(usable("abcde", "a"), Err(Skip::OddLength));
        assert_eq!(usable("a", "abcd"), Ok(()));
        assert_eq!(usable("a", "abcde"), Err(Skip::OddLength));
        assert_eq!(usable("", "a"), Err(Skip::OddLength));
        assert_eq!(usable("hello", " "), Err(Skip::Empty));
    }

    #[test]
    fn a_rewrite_that_opens_like_a_chat_reply_fails() {
        let heard = "meet me on Tuesday at noon";
        for answer in [
            "Sure! Meet me Tuesday at noon.",
            "Here is: meet me Tuesday.",
            "Here's the text: meet Tuesday",
            "Certainly. Meet me Tuesday.",
            "Of course, meet me Tuesday.",
        ] {
            assert_eq!(usable(heard, answer), Err(Skip::Chatty), "{answer}");
        }
        assert_eq!(usable("surely we can", "Surely we can."), Ok(()));
        assert_eq!(usable("surely we can meet", "Surely we can."), Ok(()));
        assert_eq!(usable("sure, I'll come", "Sure, I'll come."), Ok(()));
        let translate = Check::Usable { translation: true };
        assert_eq!(translate.verdict("Bien sûr.", "Of course.", &[]), Ok(()));
    }

    #[test]
    fn present_wants_anything_but_blanks() {
        assert_eq!(Check::Present.verdict("x", "y", &[]), Ok(()));
        assert_eq!(Check::Present.verdict("x", " \t", &[]), Err(Skip::Empty));
    }
}
