//! Spoken editing commands, applied to the whole dictation before any of it
//! is typed: "scratch that" takes back the sentence before it, "new line"
//! and "new paragraph" break the text, "delete last word" and "delete last
//! three words" take words back.
//!
//! A command counts only as a sentence of its own ("Monday. Scratch that.
//! Tuesday."), which is how a speech model writes a phrase said between
//! pauses; inside a sentence ("scratch that idea") it is typed as said.

/// What one sentence of the dictation is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Block {
    Text(String),
    Break(&'static str),
}

/// What a sentence says, if it is a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    /// Take back the sentence before.
    Scratch,
    /// Take back this many words.
    DeleteWords(usize),
    Break(&'static str),
}

/// The sentences of `text`, each with what ends it.
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if matches!(c, '.' | '!' | '?' | '\u{2026}') {
            // Closing marks stay with their sentence: "Stop!?" is one.
            while let Some(&(_, next)) = chars.peek() {
                if matches!(next, '.' | '!' | '?' | '\u{2026}' | '"' | '\u{201d}' | ')') {
                    chars.next();
                } else {
                    break;
                }
            }
            let end = chars.peek().map_or(text.len(), |&(j, _)| j);
            if chars.peek().is_none_or(|&(_, next)| next.is_whitespace()) {
                out.push(text[start..end].trim());
                start = end;
            }
        }
    }
    out.push(text[start..].trim());
    out.retain(|s| !s.is_empty());
    out
}

/// The words of `sentence`, lowercased, without punctuation.
fn words(sentence: &str) -> Vec<String> {
    sentence
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

fn number(word: &str) -> Option<usize> {
    const NAMES: [&str; 10] = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    NAMES
        .iter()
        .position(|n| *n == word)
        .map(|i| i + 1)
        .or_else(|| word.parse().ok().filter(|n| (1..=50).contains(n)))
}

fn command(sentence: &str) -> Option<Command> {
    let words = words(sentence);
    let said: Vec<&str> = words.iter().map(String::as_str).collect();
    Some(match said.as_slice() {
        ["scratch" | "strike" | "delete" | "undo", "that"] => Command::Scratch,
        ["new", "line"] => Command::Break("\n"),
        ["new", "paragraph"] => Command::Break("\n\n"),
        ["delete", "last", "word"] | ["delete", "the", "last", "word"] => Command::DeleteWords(1),
        ["delete", "last", n, "words"] | ["delete", "the", "last", n, "words"] => {
            Command::DeleteWords(number(n)?)
        }
        _ => return None,
    })
}

/// `text` without its last `n` words, keeping how it ended (". ", "?").
fn drop_words(text: &str, n: usize) -> String {
    let end = text.trim_end();
    let mark_at = end
        .rfind(|c: char| c.is_alphanumeric())
        .map_or(0, |i| i + end[i..].chars().next().map_or(1, char::len_utf8));
    let (body, mark) = end.split_at(mark_at);
    let mut kept: Vec<&str> = body.split_whitespace().collect();
    kept.truncate(kept.len().saturating_sub(n));
    if kept.is_empty() {
        return String::new();
    }
    let body = kept.join(" ");
    let body = body.trim_end_matches(',');
    format!("{body}{mark}")
}

/// `text` with every command it holds carried out.
pub fn apply(text: &str) -> String {
    let mut blocks: Vec<Block> = Vec::new();
    let mut acted = false;
    for sentence in sentences(text) {
        match command(sentence) {
            None => blocks.push(Block::Text(sentence.to_owned())),
            Some(cmd) => {
                acted = true;
                match cmd {
                    Command::Break(brk) => blocks.push(Block::Break(brk)),
                    Command::Scratch => {
                        if let Some(at) = blocks.iter().rposition(|b| matches!(b, Block::Text(_))) {
                            blocks.truncate(at);
                        }
                    }
                    Command::DeleteWords(mut n) => {
                        while n > 0 {
                            let Some(at) = blocks.iter().rposition(|b| matches!(b, Block::Text(_)))
                            else {
                                break;
                            };
                            let Block::Text(last) = &blocks[at] else {
                                unreachable!()
                            };
                            let count = last.split_whitespace().count();
                            let kept = drop_words(last, n);
                            n = n.saturating_sub(count);
                            if kept.is_empty() {
                                blocks.truncate(at);
                            } else {
                                blocks[at] = Block::Text(kept);
                                blocks.truncate(at + 1);
                                n = 0;
                            }
                        }
                    }
                }
            }
        }
    }
    if !acted {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut after_break = true;
    for block in blocks {
        match block {
            Block::Break(brk) => {
                out.push_str(brk);
                after_break = true;
            }
            Block::Text(t) => {
                if !after_break {
                    out.push(' ');
                }
                out.push_str(&t);
                after_break = false;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_that_takes_back_the_sentence_before_it() {
        assert_eq!(
            apply("Meet on Monday. Scratch that. Meet on Tuesday."),
            "Meet on Tuesday."
        );
        assert_eq!(apply("One. Two. Delete that! Three."), "One. Three.");
        assert_eq!(apply("Scratch that."), "");
        assert_eq!(apply("Hello. Undo that"), "");
    }

    #[test]
    fn a_command_inside_a_sentence_is_typed_as_said() {
        for said in [
            "Let's scratch that idea.",
            "I said new line, not newline.",
            "Please delete that file. Thanks.",
            "Delete the last word of the title.",
        ] {
            assert_eq!(apply(said), said);
        }
    }

    #[test]
    fn new_line_and_new_paragraph_break_the_text() {
        assert_eq!(
            apply("Dear Ann. New paragraph. Thanks for the call. New line. Bob."),
            "Dear Ann.\n\nThanks for the call.\nBob."
        );
        assert_eq!(apply("Hi. New line"), "Hi.\n");
    }

    #[test]
    fn delete_last_words_takes_back_words_and_keeps_the_mark() {
        assert_eq!(
            apply("Send it to the whole team. Delete last two words."),
            "Send it to the."
        );
        assert_eq!(apply("Is it ready? Delete the last word."), "Is it?");
        assert_eq!(apply("Go now. Stop here. Delete last 3 words."), "Go.");
        assert_eq!(
            apply("Hi there, Delete last word."),
            "Hi there, Delete last word."
        );
    }

    #[test]
    fn text_with_no_command_is_left_exactly_as_it_was() {
        let said = "  Spaced   out.  Wow!  ";
        assert_eq!(apply(said), said);
    }

    #[test]
    fn sentences_split_after_marks_followed_by_space() {
        assert_eq!(
            sentences("A. B?! C\u{2026} e.g. x 3.5 D"),
            ["A.", "B?!", "C\u{2026}", "e.g.", "x 3.5 D"]
        );
    }
}
