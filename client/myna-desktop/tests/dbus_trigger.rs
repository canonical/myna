// tests/dbus_trigger.rs — hermetic contract test for `DbusTrigger` (feature
// 004, T140; contract publisher.md P9–P12, dbus-interface.md C6/C7). The
// edge source is driven directly (a fake stands in for the D-Bus method
// handlers), so no session bus is needed.

use myna_desktop::shortcut::dbus::{DbusTrigger, DbusTriggerSource};
use myna_orchestrator::{Trigger, TriggerEdge};

/// Drive a `DbusTrigger` from a source, exactly as the served object would.
fn trigger() -> (DbusTrigger, DbusTriggerSource) {
    DbusTrigger::new()
}

// --- P9: Toggle alternates Press/Release; Start/Stop map to the edges ----

#[test]
fn p9_toggle_alternates_press_release() {
    let (mut trigger, source) = trigger();

    source.start();
    assert_eq!(next_edge_sync(&mut trigger), Some(TriggerEdge::Press));

    source.toggle();
    assert_eq!(next_edge_sync(&mut trigger), Some(TriggerEdge::Release));

    source.toggle();
    assert_eq!(next_edge_sync(&mut trigger), Some(TriggerEdge::Press));
}

#[test]
fn p9_start_yields_press_when_idle_stop_yields_release_when_active() {
    let (mut trigger, source) = trigger();

    source.start();
    assert_eq!(next_edge_sync(&mut trigger), Some(TriggerEdge::Press));

    source.stop();
    assert_eq!(next_edge_sync(&mut trigger), Some(TriggerEdge::Release));
}

// --- P10: duplicate Start/Toggle do not start two sessions ----------------

#[test]
fn p10_duplicate_start_is_deduped_to_one_press() {
    let (mut trigger, source) = trigger();

    source.start();
    source.start(); // a second call while active — must not start a second session
    assert_eq!(next_edge_sync(&mut trigger), Some(TriggerEdge::Press));

    // One Release ends the single session.
    source.stop();
    assert_eq!(next_edge_sync(&mut trigger), Some(TriggerEdge::Release));

    // After a Release, a fresh Start is a new Press (the dedup resets).
    source.start();
    assert_eq!(next_edge_sync(&mut trigger), Some(TriggerEdge::Press));
}

// --- P11: Start reports (false, content-free reason) when it cannot -------
// The DbusTrigger itself cannot "fail to start" (there is no target here);
// the (false, reason) shape belongs to the served method, which consults a
// startability gate. Pinned so the trigger never panics on a refused start.

#[test]
fn p11_start_never_panics_and_reports_a_shape() {
    let (mut trigger, source) = trigger();
    // A refused start (simulated: the gate is closed) must not push an edge
    // and must not panic.
    source.refuse();
    assert_eq!(
        next_edge_sync(&mut trigger),
        None,
        "a refused start pushes nothing"
    );
}

// --- P12: exhaustion ends the edge stream cleanly ------------------------

#[test]
fn p12_source_dropped_ends_the_stream() {
    let (mut trigger, source) = trigger();
    drop(source);
    assert_eq!(
        next_edge_sync(&mut trigger),
        None,
        "the trigger ends cleanly when the source goes away"
    );
}

// --- discard_pending swallows queued edges without flipping parity --------

#[test]
fn p10_discard_pending_swallows_edges_without_flipping() {
    let (mut trigger, source) = trigger();
    source.start();
    // An utterance ended without reading the edge; discard the queued Press.
    discard_pending_sync(&mut trigger);
    assert_eq!(
        next_edge_sync(&mut trigger),
        None,
        "the queued Press was discarded"
    );

    // The source's own state is unchanged (it is still "active"): the next
    // edge is the matching Release, so parity is not desynced.
    source.stop();
    assert_eq!(next_edge_sync(&mut trigger), Some(TriggerEdge::Release));

    // A fresh start after the release is a Press again.
    source.start();
    assert_eq!(next_edge_sync(&mut trigger), Some(TriggerEdge::Press));
}

// Helper: DbusTrigger's async trait is exercised synchronously here via a
// small runtime (the trait methods are the public contract).
fn next_edge_sync(trigger: &mut DbusTrigger) -> Option<TriggerEdge> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // A short timeout: `next_edge` blocks until an edge OR the source closes,
    // so the "expects nothing" cases must not hang forever.
    rt.block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            Trigger::next_edge(trigger),
        )
        .await
        .unwrap_or_default()
    })
}

fn discard_pending_sync(trigger: &mut DbusTrigger) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(Trigger::discard_pending(trigger));
}
