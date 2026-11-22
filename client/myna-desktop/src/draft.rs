//! Text that is heard but not yet in the field.
//!
//! IBus can add text to a field and never take it back, so anything a
//! processing step may still change has to stay out of the field until it is
//! settled. [`CommitPolicy`] decides how long that is; [`DraftView`] is where
//! the user reads the text meanwhile.

use async_trait::async_trait;

use crate::dbus::{PropertyValue, SharedBus};

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
    /// How long a chain holds text back: only as far as its nodes need,
    /// the last finished sentence and the one being said, or all of it
    /// until the end. With no node text is typed as it arrives.
    pub fn for_needs(needs: Option<crate::process::graph::Needs>) -> Self {
        use crate::process::graph::Needs;
        match needs {
            None => CommitPolicy::Streaming,
            Some(Needs::Sentence) => CommitPolicy::Horizon { sentences: 1 },
            Some(Needs::Dictation) => CommitPolicy::AtEnd,
        }
    }

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
/// punctuation, then whitespace, or at once for a full-width mark, which
/// Chinese and Japanese leave unspaced. A mark at the very end closes
/// nothing yet, since the next piece may continue it ("3." then "5 metres").
pub fn sentence_ends(text: &str) -> Vec<usize> {
    let full_width = |c: char| matches!(c, '。' | '？' | '！');
    let mut ends = Vec::new();
    let (mut closed, mut closed_full_width) = (false, false);
    for (at, c) in text.char_indices() {
        if (closed && c.is_whitespace()) || (closed_full_width && !full_width(c)) {
            ends.push(at);
        }
        closed = matches!(c, '.' | '?' | '!') || full_width(c);
        closed_full_width = full_width(c);
    }
    ends
}

/// Add committed text to what is held. Servers whose segments carry natural
/// (leading-space) whitespace concatenate verbatim (contract I2);
/// stripped-segment servers get a separator, never a double space.
pub fn append(pending: &mut String, text: &str) {
    if !pending.is_empty()
        && !pending.ends_with(char::is_whitespace)
        && !text.starts_with(char::is_whitespace)
    {
        pending.push(' ');
    }
    pending.push_str(text);
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

/// Publishes the draft as the `DraftText` and `DraftTail` bus properties,
/// for whichever desktop component draws it.
pub struct BusDraftView {
    bus: SharedBus,
    shown: (String, String),
}

impl BusDraftView {
    pub fn new(bus: SharedBus) -> Self {
        Self {
            bus,
            shown: Default::default(),
        }
    }
}

#[async_trait]
impl DraftView for BusDraftView {
    async fn show(&mut self, stable: &str, tail: &str) {
        let mut bus = self.bus.lock().await;
        // The tail first: a reader that redraws on `DraftText` then sees the
        // pair that belongs together.
        if self.shown.1 != tail {
            self.shown.1 = tail.to_string();
            bus.set_property("DraftTail", PropertyValue::Str(tail.to_string()))
                .await;
        }
        if self.shown.0 != stable {
            self.shown.0 = stable.to_string();
            bus.set_property("DraftText", PropertyValue::Str(stable.to_string()))
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::FakeBus;
    use std::sync::Arc;

    #[tokio::test]
    async fn the_bus_view_publishes_only_what_changed() {
        let bus = FakeBus::new();
        let shared: SharedBus = Arc::new(tokio::sync::Mutex::new(bus.clone()));
        let mut view = BusDraftView::new(shared);
        view.show("hello", "wor").await;
        view.show("hello", "wor").await;
        view.show("hello world", "").await;
        let str = |s: &str| PropertyValue::Str(s.to_string());
        assert_eq!(
            bus.sets(),
            vec![
                ("DraftTail".to_string(), str("wor")),
                ("DraftText".to_string(), str("hello")),
                ("DraftTail".to_string(), str("")),
                ("DraftText".to_string(), str("hello world")),
            ]
        );
    }

    #[test]
    fn held_text_is_joined_without_a_double_space() {
        let mut held = String::new();
        append(&mut held, "one");
        append(&mut held, "two");
        append(&mut held, " three");
        assert_eq!(held, "one two three");
    }

    #[test]
    fn the_policy_follows_what_the_nodes_need() {
        use crate::process::graph::Needs;
        assert_eq!(CommitPolicy::for_needs(None), CommitPolicy::Streaming);
        assert_eq!(
            CommitPolicy::for_needs(Some(Needs::Sentence)),
            CommitPolicy::Horizon { sentences: 1 }
        );
        assert_eq!(
            CommitPolicy::for_needs(Some(Needs::Dictation)),
            CommitPolicy::AtEnd
        );
    }

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
    fn full_width_marks_end_a_sentence_without_a_space() {
        let policy = CommitPolicy::Horizon { sentences: 1 };
        let text = "你好。再见！谢谢？嗯";
        // The segment has no spaces: "谢谢？" is kept, "嗯" is being said.
        assert_eq!(&text[..policy.ready(text, false)], "你好。再见！");
        // A mark at the very end closes nothing yet, as a full stop does.
        assert_eq!(policy.ready("你好。再见。", false), 0);
        // A run of marks is one end.
        let run = CommitPolicy::Horizon { sentences: 0 };
        assert_eq!(&"真的？！好"[..run.ready("真的？！好", false)], "真的？！");
    }

    #[test]
    fn horizon_splits_on_character_boundaries() {
        let policy = CommitPolicy::Horizon { sentences: 0 };
        let text = "你好。 再见。 嗯";
        assert_eq!(&text[..policy.ready(text, false)], "你好。 再见。");
    }
}
