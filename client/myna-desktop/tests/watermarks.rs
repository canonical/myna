//! Performance watermarks (constitution Principle III).
//!
//! Checked-in latency baselines with declared tolerances. The **hermetic**
//! watermark below runs everywhere (offline, in CI) and is a regression signal
//! on the controller's own per-segment overhead. The **hardware** SLOs
//! (activation→indicator-visible ≤200 ms — SC-005; press→capture-start <100 ms;
//! per-segment commit <50 ms on reference hardware) require a real display /
//! capture / IBus and are measured under the env-gated suites on the desktop VM
//! and on hardware; the capture-path baselines are inherited from feature 002.
//!
//! Baselines (reference environment; regressions beyond tolerance are a Principle
//! III violation):
//!   - hermetic per-segment route+commit overhead (MockInjector): < 2 ms/segment
//!     (measured ~µs; the 2 ms bound is generous headroom for CI noise).
//!   - hardware activation→indicator-visible: 100–200 ms (SC-005)  [gated]
//!   - hardware press→capture-start:          < 100 ms             [gated]
//!   - hardware per-segment commit:           < 50 ms              [gated]

use std::time::{Duration, Instant};

use myna_desktop::controller::SessionRun;
use myna_desktop::indicator::mock::MockIndicator;
use myna_desktop::inject::mock::MockInjector;
use myna_desktop::{DesktopController, DictationState};
use myna_orchestrator::{
    OrchestratorEvent, ScriptedTrigger, SessionOutcome, StopHandle, TriggerEdge,
};
use tokio::sync::mpsc;

/// Hermetic watermark: the controller's per-segment routing + commit overhead
/// (mock injector, in-memory) stays well under the tolerance — a cheap
/// regression signal that runs offline.
#[tokio::test]
async fn perf_hermetic_per_segment_overhead_within_tolerance() {
    const SEGMENTS: usize = 200;
    const TOLERANCE_PER_SEGMENT: Duration = Duration::from_millis(2);

    let injector = MockInjector::new();
    let inject_log = injector.log();

    // A session that commits SEGMENTS `Final`s then completes.
    let session = move |tx: mpsc::Sender<OrchestratorEvent>| -> (SessionRun, StopHandle) {
        let run: SessionRun = Box::pin(async move {
            let _ = tx.send(OrchestratorEvent::Loading).await;
            let _ = tx.send(OrchestratorEvent::Ready).await;
            for i in 0..SEGMENTS {
                let _ = tx
                    .send(OrchestratorEvent::Final(format!("segment {i}")))
                    .await;
            }
            let _ = tx.send(OrchestratorEvent::Done(String::new())).await;
            Ok(SessionOutcome::Completed {
                transcript: String::new(),
            })
        });
        (run, StopHandle::default())
    };

    let mut controller = DesktopController::builder()
        .trigger(ScriptedTrigger::new([TriggerEdge::Press]))
        .injector(injector)
        .indicator(MockIndicator::new())
        .session(session)
        .build();

    let start = Instant::now();
    controller.run().await;
    let elapsed = start.elapsed();

    // Coalesced: 200 back-to-back finals (no event between them) are buffered
    // and inserted as ONE CommitText on the terminal `done`. The per-segment
    // routing/buffering overhead is what this measures.
    let commits = inject_log.lock().unwrap().commits.clone();
    assert_eq!(commits.len(), 1, "the back-to-back burst is one coalesced commit");
    assert!(commits[0].starts_with("segment 0"), "first segment present, in order");
    assert!(commits[0].ends_with(&format!("segment {}", SEGMENTS - 1)), "last segment present");
    assert_eq!(controller.state(), DictationState::Idle);

    let per_segment = elapsed / SEGMENTS as u32;
    assert!(
        per_segment < TOLERANCE_PER_SEGMENT,
        "per-segment overhead {per_segment:?} exceeds tolerance {TOLERANCE_PER_SEGMENT:?}"
    );
    eprintln!(
        "hermetic per-segment overhead: {per_segment:?} (tolerance {TOLERANCE_PER_SEGMENT:?})"
    );
}
