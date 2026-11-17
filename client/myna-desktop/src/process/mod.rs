//! Processing between the speech model and the field.
//!
//! The controller hands every piece of text it is about to commit to a
//! [`Processor`] and commits what comes back.

use async_trait::async_trait;

/// Turns heard text into the text to type.
#[async_trait]
pub trait Processor: Send {
    /// Never fails: a step that cannot run, is too slow or returns nonsense
    /// passes its input through, so a dictation is never lost to processing.
    async fn process(&mut self, text: String) -> String;
}

/// Types what was heard.
pub struct Unprocessed;

#[async_trait]
impl Processor for Unprocessed {
    async fn process(&mut self, text: String) -> String {
        text
    }
}
