//! Behavioral suite for the adapter over the fake backend (plan T50): the
//! whole capture lifecycle from `docs/audio-adapter-api.md` — press → hold →
//! drain-at-ready, graceful stop, abort, overflow policy, faults — with no
//! PipeWire anywhere.

use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::StreamExt;
use myna_audio::{AudioStats, CaptureSource, ScriptedBackend, Step};
use myna_core::{AudioFormat, AudioSource, CaptureError, PcmChunk};
use tokio::sync::watch;
use tokio::time::timeout;

const FMT: AudioFormat = AudioFormat { sample_rate_hz: 16_000, channels: 1, sample_width_bytes: 2 };

fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

/// Await the stats tap matching `pred` (bounded, event-driven — no sleeps).
async fn wait_stats(
    rx: &mut watch::Receiver<AudioStats>,
    pred: impl Fn(&AudioStats) -> bool,
) -> AudioStats {
    timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = *rx.borrow_and_update();
            if pred(&snapshot) {
                return snapshot;
            }
            rx.changed().await.expect("stats sender dropped");
        }
    })
    .await
    .expect("stats condition not reached in time")
}

/// Drain a capture stream: collected Ok chunks + the fault, if any.
async fn drain(
    mut stream: myna_core::CaptureStream,
) -> (Vec<PcmChunk>, Option<CaptureError>) {
    let mut chunks = Vec::new();
    let mut fault = None;
    while let Some(item) = timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("stream stalled")
    {
        match item {
            Ok(chunk) => chunks.push(chunk),
            Err(err) => {
                assert!(fault.is_none(), "more than one Err on the stream");
                fault = Some(err);
            }
        }
    }
    (chunks, fault)
}

#[tokio::test]
async fn ring_fills_while_consumer_defers_then_drains_everything() {
    // §6: capture() is the press; the consumer holds off polling (the
    // accept-gate) and loses nothing up to the ring depth.
    let backend = ScriptedBackend::new(vec![Step::Silence(secs(0.5))]);
    let source = CaptureSource::builder(FMT).backend(Box::new(backend)).build();
    let mut stats = source.stats();
    let stream = Box::new(source).capture();

    // Deliberately do NOT poll the stream. The tap alone proves capture is
    // live during the "cold load" — and it counts before any drain.
    let snapshot = wait_stats(&mut stats, |s| s.captured >= secs(0.5)).await;
    assert_eq!(snapshot.dropped, Duration::ZERO);

    let (chunks, fault) = drain(stream).await;
    assert!(fault.is_none());
    let total: usize = chunks.iter().map(|c| c.data.len()).sum();
    assert_eq!(total, 16_000, "every captured byte is delivered after the hold");
    assert!(chunks.iter().all(|c| c.format == FMT), "exactly the configured format");
    assert_eq!(chunks.len(), 5, "0.5 s at 100 ms chunks");
}

#[tokio::test]
async fn graceful_stop_drains_then_ends() {
    // §5: hotkey release = stop() → drain what was captured, then None.
    let backend = ScriptedBackend::new(vec![
        Step::Silence(secs(0.2)),
        Step::Wait(secs(30.0)), // "device still open"; interrupted by stop
    ]);
    let source = CaptureSource::builder(FMT).backend(Box::new(backend)).build();
    let mut stats = source.stats();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();

    wait_stats(&mut stats, |s| s.captured >= secs(0.2)).await;
    stop.stop();

    let (chunks, fault) = drain(stream).await;
    assert!(fault.is_none(), "graceful stop is a clean end, never an Err");
    let total: usize = chunks.iter().map(|c| c.data.len()).sum();
    assert_eq!(total, 6_400, "everything captured before the stop drains");
}

#[tokio::test]
async fn dropping_the_stream_aborts_the_backend() {
    // §3: abort = drop. The backend must observe it and exit.
    let backend = ScriptedBackend::new(vec![Step::Wait(secs(30.0))]);
    let finished = backend.finished();
    let source = CaptureSource::builder(FMT).backend(Box::new(backend)).build();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();

    drop(stream);
    assert!(stop.is_stopped(), "dropping the stream trips the stop flag");
    timeout(Duration::from_secs(2), async {
        while !finished.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("backend did not exit after abort");
}

#[tokio::test]
async fn overflow_drops_oldest_and_reports_on_the_tap() {
    // §6: depth 0.2 s, 1.0 s pushed while nobody drains → the newest 0.2 s
    // survive and the tap accounts the aged-out 0.8 s.
    let steps = (0..10u8).map(|i| Step::Bytes(vec![i; 3_200])).collect();
    let backend = ScriptedBackend::new(steps);
    let source = CaptureSource::builder(FMT)
        .ring_depth(secs(0.2))
        .backend(Box::new(backend))
        .build();
    let mut stats = source.stats();
    let stream = Box::new(source).capture();

    let snapshot = wait_stats(&mut stats, |s| s.captured >= secs(1.0)).await;
    assert_eq!(snapshot.dropped, secs(0.8), "8 of 10 chunks aged out");

    let (chunks, fault) = drain(stream).await;
    assert!(fault.is_none());
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].data[0], 8, "oldest surviving chunk is the 9th pushed");
    assert_eq!(chunks[1].data[0], 9, "newest chunk survives");
}

#[tokio::test]
async fn fault_is_one_err_then_end_after_captured_audio_drains() {
    let backend =
        ScriptedBackend::new(vec![Step::Silence(secs(0.2)), Step::Fault("boom".into())]);
    let source = CaptureSource::builder(FMT).backend(Box::new(backend)).build();
    let stream = Box::new(source).capture();

    let (chunks, fault) = drain(stream).await;
    assert_eq!(chunks.len(), 2, "audio captured before the fault still drains");
    match fault {
        Some(CaptureError::Backend(msg)) => assert!(msg.contains("boom")),
        other => panic!("expected Backend fault, got {other:?}"),
    }
}

#[tokio::test]
async fn unopenable_device_is_one_err_then_end() {
    let source = CaptureSource::builder(FMT)
        .backend(Box::new(ScriptedBackend::unavailable("no mic")))
        .build();
    let (chunks, fault) = drain(Box::new(source).capture()).await;
    assert!(chunks.is_empty());
    assert!(matches!(fault, Some(CaptureError::DeviceUnavailable(_))));
}

#[tokio::test]
async fn stats_track_signal_levels() {
    // A full-scale square wave: rms ≈ peak ≈ 1.0, clipped.
    let loud: Vec<u8> =
        std::iter::repeat(i16::MAX.to_le_bytes()).take(1_600).flatten().collect();
    let backend = ScriptedBackend::new(vec![Step::Bytes(loud)]);
    let source = CaptureSource::builder(FMT).backend(Box::new(backend)).build();
    let mut stats = source.stats();
    let _stream = Box::new(source).capture();

    let snapshot = wait_stats(&mut stats, |s| s.captured >= secs(0.1)).await;
    assert!(snapshot.rms > 0.999 && snapshot.peak > 0.999);
    assert!(snapshot.clipped);
}

#[tokio::test]
async fn short_final_chunk_flushes_whole_frames_only() {
    // Stereo (4-byte frames): 402 bytes pushed → 400 delivered, the trailing
    // partial frame dropped, never padded (§4).
    let stereo = AudioFormat { sample_rate_hz: 16_000, channels: 2, sample_width_bytes: 2 };
    let backend = ScriptedBackend::new(vec![Step::Bytes(vec![0u8; 402])]);
    let source = CaptureSource::builder(stereo).backend(Box::new(backend)).build();
    let (chunks, fault) = drain(Box::new(source).capture()).await;
    assert!(fault.is_none());
    let total: usize = chunks.iter().map(|c| c.data.len()).sum();
    assert_eq!(total, 400);
    assert!(chunks.iter().all(|c| c.data.len() % 4 == 0));
}
