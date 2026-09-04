//! The audio-level pump (feature 004, contract publisher.md P6–P8): a tokio
//! task that reads `myna_audio`'s `AudioStats` `watch` and publishes the
//! normalized `AudioRms`/`AudioPeak` to `com.canonical.Myna.Dictation` at a bounded
//! cadence while a session is active, zeroing them when it ends.
//!
//! It only *reads* the existing stats watch and emits — no new work on the
//! capture thread (P8), and it carries energy only, never samples or content
//! (constitution V). Throttled so the bus and the extension's repaint stay
//! cheap; the extension applies its own stale-decay on top (research R5).

use std::time::Duration;

use myna_audio::AudioStats;
use tokio::sync::watch;

use crate::dbus::{PropertyValue, SharedBus};

/// Publish cadence (~20 Hz): smooth enough for a VU, cheap on the bus (R5).
pub const PUMP_INTERVAL: Duration = Duration::from_millis(50);

/// Clamp a raw level into the `[0.0, 1.0]` the contract advertises (E2).
fn clamp_level(level: f32) -> f64 {
    (level.clamp(0.0, 1.0)) as f64
}

/// Run the level pump until the stats sender is dropped (session end). Reads
/// the latest `AudioStats` at [`PUMP_INTERVAL`] and publishes `AudioRms`/
/// `AudioPeak`; on exit it publishes `0.0` for both (idle floor, P7).
///
/// Cadence is driven by a timer, not by every stats change, so a burst of
/// capture callbacks can never spam the bus (P6). Conflating by design — a VU
/// wants the latest value, not history.
pub async fn run(bus: SharedBus, mut stats: watch::Receiver<AudioStats>) {
    let mut ticker = tokio::time::interval(PUMP_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let (rms, peak) = {
                    let s = *stats.borrow();
                    (clamp_level(s.rms), clamp_level(s.peak))
                };
                publish_levels(&bus, rms, peak).await;
            }
            // The session's capture source dropped the sender: end, at floor.
            changed = stats.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }
    publish_levels(&bus, 0.0, 0.0).await;
}

async fn publish_levels(bus: &SharedBus, rms: f64, peak: f64) {
    let mut bus = bus.lock().await;
    bus.set_property("AudioRms", PropertyValue::F64(rms)).await;
    bus.set_property("AudioPeak", PropertyValue::F64(peak))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::{DictationService, FakeBus};
    use std::sync::Arc;

    fn stats(rms: f32, peak: f32) -> AudioStats {
        AudioStats {
            rms,
            peak,
            ..Default::default()
        }
    }

    /// P6/E2: while a session runs, the pump publishes the latest RMS/peak
    /// (clamped to [0,1]); P7: it zeroes them when the sender is dropped.
    #[tokio::test]
    async fn pumps_latest_levels_then_zeroes_on_end() {
        let fake = FakeBus::new();
        let service = DictationService::new(fake.clone());
        let (tx, rx) = watch::channel(stats(0.0, 0.0));

        let handle = tokio::spawn(super::run(service.bus(), rx));

        // Drive a couple of level updates; the pump conflates to the latest.
        tx.send(stats(0.4, 0.7)).unwrap();
        tokio::time::sleep(PUMP_INTERVAL * 3).await;
        tx.send(stats(1.5, -0.2)).unwrap(); // out of range → clamped
        tokio::time::sleep(PUMP_INTERVAL * 3).await;

        assert_eq!(fake.property("AudioRms"), Some(PropertyValue::F64(1.0)));
        assert_eq!(fake.property("AudioPeak"), Some(PropertyValue::F64(0.0)));

        // Session end: dropping the sender ends the pump at the idle floor.
        drop(tx);
        handle.await.unwrap();
        assert_eq!(fake.property("AudioRms"), Some(PropertyValue::F64(0.0)));
        assert_eq!(fake.property("AudioPeak"), Some(PropertyValue::F64(0.0)));
    }

    /// P8/V: the pump never publishes anything but the two level properties —
    /// no state, no content.
    #[tokio::test]
    async fn publishes_only_levels() {
        let fake = FakeBus::new();
        let service = DictationService::new(fake.clone());
        let (tx, rx) = watch::channel(stats(0.2, 0.3));
        let handle = tokio::spawn(super::run(service.bus(), rx));
        tokio::time::sleep(PUMP_INTERVAL * 2).await;
        drop(tx);
        handle.await.unwrap();

        assert!(
            fake.property("State").is_none(),
            "pump must not touch State"
        );
        assert!(
            fake.property("StatusMessage").is_none(),
            "pump must not touch StatusMessage"
        );
        let _ = Arc::strong_count(&service.bus());
    }
}
