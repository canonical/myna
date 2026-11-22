//! Processing between the speech model and the field.
//!
//! The controller hands every piece of text it is about to commit to a
//! [`Processor`] and commits what comes back.

use async_trait::async_trait;
use myna_platform::text_input::FieldPurpose;

mod check;
pub mod graph;
pub mod history;
pub mod pipeline;
mod rules;

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
    /// Never fails: a step that cannot run, is too slow or returns nonsense
    /// passes its input through, so a dictation is never lost to processing.
    async fn process(&mut self, text: String, field: &Field) -> String;
}

/// Types what was heard.
pub struct Unprocessed;

#[async_trait]
impl Processor for Unprocessed {
    async fn process(&mut self, text: String, _: &Field) -> String {
        text
    }
}
