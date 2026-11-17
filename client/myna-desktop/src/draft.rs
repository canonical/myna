//! Text that is heard but not yet in the field.
//!
//! IBus can add text to a field and never take it back, so anything a
//! processing step may still change has to stay out of the field until it is
//! settled. [`CommitPolicy`] decides how long that is; [`DraftView`] is where
//! the user reads the text meanwhile.

use async_trait::async_trait;

/// When heard text may be written into the field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CommitPolicy {
    /// As each piece arrives. Nothing is held, so nothing can be revised.
    #[default]
    Streaming,
    /// Once, when the dictation ends.
    AtEnd,
    /// Everything but the last `sentences` complete sentences and the
    /// unfinished one after them, which stay open to revision.
    Horizon { sentences: usize },
}

impl CommitPolicy {
    /// Whether text goes to the field as it arrives, with nothing held back.
    pub fn streams(self) -> bool {
        self == CommitPolicy::Streaming
    }

    /// How many bytes at the front of `pending` may be written now. Always a
    /// character boundary; all of it once the dictation is `finished`.
    pub fn ready(self, pending: &str, finished: bool) -> usize {
        match self {
            _ if finished => pending.len(),
            CommitPolicy::Streaming => pending.len(),
            CommitPolicy::AtEnd => 0,
            CommitPolicy::Horizon { sentences } => {
                let ends = sentence_ends(pending);
                ends.len()
                    .checked_sub(sentences + 1)
                    .map_or(0, |settled| ends[settled])
            }
        }
    }
}

/// The byte offset after each sentence that something follows: its closing
/// punctuation, then whitespace. A full stop at the very end closes nothing
/// yet, since the next piece may continue it ("3." then "5 metres").
fn sentence_ends(text: &str) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut closed = false;
    for (at, c) in text.char_indices() {
        if closed && c.is_whitespace() {
            ends.push(at);
        }
        closed = matches!(c, '.' | '?' | '!' | '。' | '？' | '！');
    }
    ends
}

/// Where the user reads held text while it is not in the field.
#[async_trait]
pub trait DraftView: Send {
    /// Show `stable`, which only processing may still change, followed by
    /// `tail`, the recogniser's current guess. Both empty hides the view.
    async fn show(&mut self, stable: &str, tail: &str);
}

/// No view: held text is seen only once it is committed.
pub struct NoDraftView;

#[async_trait]
impl DraftView for NoDraftView {
    async fn show(&mut self, _stable: &str, _tail: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_commits_everything_at_once() {
        assert_eq!(CommitPolicy::Streaming.ready("one two", false), 7);
    }

    #[test]
    fn at_end_holds_until_finished() {
        assert_eq!(CommitPolicy::AtEnd.ready("One. Two.", false), 0);
        assert_eq!(CommitPolicy::AtEnd.ready("One. Two.", true), 9);
    }

    #[test]
    fn horizon_keeps_the_last_sentences_and_the_unfinished_one() {
        let policy = CommitPolicy::Horizon { sentences: 1 };
        let text = "One. Two? Three! Fou";
        // "Three!" is the one kept sentence, "Fou" the unfinished one.
        assert_eq!(&text[..policy.ready(text, false)], "One. Two?");
        assert_eq!(policy.ready("One. Tw", false), 0);
        assert_eq!(policy.ready(text, true), text.len());
    }

    #[test]
    fn horizon_zero_commits_every_followed_sentence() {
        let policy = CommitPolicy::Horizon { sentences: 0 };
        assert_eq!(&"One. Two"[..policy.ready("One. Two", false)], "One.");
        // Nothing follows the stop, so it may yet be "3.5".
        assert_eq!(policy.ready("It is 3.", false), 0);
    }

    #[test]
    fn horizon_splits_on_character_boundaries() {
        let policy = CommitPolicy::Horizon { sentences: 0 };
        let text = "你好。 再见。 嗯";
        assert_eq!(&text[..policy.ready(text, false)], "你好。 再见。");
    }
}
