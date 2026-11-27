//! What became of a piece of text once the controller had processed it, for
//! the record of dictations.

use std::time::{Duration, Instant};

use myna_core::history::Outcome;

/// Monotonic waits of one piece of text that landed after the stop edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// Stop edge to the text being ready to process.
    pub asr_tail: Duration,
    /// The field taking the text.
    pub inject: Duration,
    /// Stop edge to the text being in the field.
    pub dead_air: Duration,
}

impl Timing {
    /// `stopped` is the stop edge, `ready` when processing began, and
    /// `injecting`/`landed` bracket the commit. Instants that run backwards
    /// count as zero.
    pub fn between(stopped: Instant, ready: Instant, injecting: Instant, landed: Instant) -> Self {
        Self {
            asr_tail: ready.saturating_duration_since(stopped),
            inject: landed.saturating_duration_since(injecting),
            dead_air: landed.saturating_duration_since(stopped),
        }
    }
}

/// One piece of text that reached the field, or was lost on the way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landing {
    pub heard: String,
    /// What was, or would have been, typed; empty when it never got past
    /// the dropped write.
    pub typed: String,
    pub outcome: Outcome,
    /// Only for text that landed after the stop edge.
    pub timing: Option<Timing>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_wait_is_measured_from_its_own_edge() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let timing = Timing::between(at(0), at(100), at(350), at(400));
        assert_eq!(timing.asr_tail, Duration::from_millis(100));
        assert_eq!(timing.inject, Duration::from_millis(50));
        assert_eq!(timing.dead_air, Duration::from_millis(400));
    }

    #[test]
    fn instants_running_backwards_count_as_no_wait() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let timing = Timing::between(at(50), at(10), at(20), at(5));
        assert_eq!(timing.asr_tail, Duration::ZERO);
        assert_eq!(timing.inject, Duration::ZERO);
        assert_eq!(timing.dead_air, Duration::ZERO);
    }
}
