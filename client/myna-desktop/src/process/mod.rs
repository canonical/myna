//! Processing between the speech model and the field.
//!
//! The controller hands every piece of text it is about to commit to a
//! [`Processor`] and commits what comes back.

use async_trait::async_trait;
use myna_platform::text_input::FieldPurpose;

pub use landing::Landing;

mod check;
pub mod disfluency;
pub mod graph;
pub mod history;
pub mod landing;
pub mod openai;
pub mod pipeline;
pub mod resolve;
pub mod rules;
pub mod script;
pub mod step;
pub mod vocabulary;
pub mod wire;

/// Where a piece of text is about to land, for a graph that routes on it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Field {
    pub purpose: FieldPurpose,
    /// The text continues a sentence the field already holds.
    pub mid_sentence: bool,
}

impl Field {
    /// `before` is the last character in the field ahead of the text. A
    /// space says nothing of where the sentence stands, so it is not
    /// mid-sentence; a letter, digit or clause mark is.
    pub fn after(purpose: FieldPurpose, before: Option<char>) -> Self {
        Self {
            purpose,
            mid_sentence: before
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, ',' | ';' | ':')),
        }
    }
}

/// Turns heard text into the text to type.
#[async_trait]
pub trait Processor: Send {
    /// A dictation is starting: get whatever `process` will need ready.
    fn prepare(&mut self) {}

    /// Never fails: a step that cannot run, is too slow or returns nonsense
    /// passes its input through, so a dictation is never lost to processing.
    async fn process(&mut self, text: String, field: &Field) -> String;

    /// A piece that went through `process` (or would have) has landed or been
    /// lost; the controller calls this once per piece.
    fn landed(&mut self, _: Landing) {}
}

/// Types what was heard.
pub struct Unprocessed;

#[async_trait]
impl Processor for Unprocessed {
    async fn process(&mut self, text: String, _: &Field) -> String {
        text
    }
}
