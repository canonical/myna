//! Behavioral suite for the adapter over the fake backend: the whole capture
//! lifecycle — press → hold →
//! drain-at-ready, graceful stop, abort, overflow policy, faults — with no
//! PipeWire anywhere.

use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::StreamExt;
use myna_audio::{
    AudioStats, CaptureBackend, CaptureSource, CaptureSpec, Producer, ScriptedBackend, Step,
};
use myna_core::{
    AudioFormat, AudioSource, CaptureError, CaptureHealth, CaptureHealthStream, PcmChunk,
};
use tokio::sync::watch;
use tokio::time::timeout;

const FMT: AudioFormat = AudioFormat {
    sample_rate_hz: 16_000,
    channels: 1,
    sample_width_bytes: 2,
};

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
async fn drain(mut stream: myna_core::CaptureStream) -> (Vec<PcmChunk>, Option<CaptureError>) {
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
    let source = CaptureSource::builder(FMT)
        .backend(Box::new(backend))
        .build();
    let mut stats = source.stats();
    let stream = Box::new(source).capture();

    // Deliberately do NOT poll the stream. The tap alone proves capture is
    // live during the "cold load" — and it counts before any drain.
    wait_stats(&mut stats, |s| s.captured >= secs(0.5)).await;

    let (chunks, fault) = drain(stream).await;
    assert!(fault.is_none());
    let total: usize = chunks.iter().map(|c| c.data.len()).sum();
    assert_eq!(
        total, 16_000,
        "every captured byte is delivered after the hold"
    );
    assert!(
        chunks.iter().all(|c| c.format == FMT),
        "exactly the configured format"
    );
    assert_eq!(chunks.len(), 5, "0.5 s at 100 ms chunks");
}

#[tokio::test]
async fn graceful_stop_drains_then_ends() {
    // §5: hotkey release = stop() → drain what was captured, then None.
    let backend = ScriptedBackend::new(vec![
        Step::Silence(secs(0.2)),
        Step::Wait(secs(30.0)), // "device still open"; interrupted by stop
    ]);
    let source = CaptureSource::builder(FMT)
        .backend(Box::new(backend))
        .build();
    let mut stats = source.stats();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();

    wait_stats(&mut stats, |s| s.captured >= secs(0.2)).await;
    stop.stop();

    let (chunks, fault) = drain(stream).await;
    assert!(
        fault.is_none(),
        "graceful stop is a clean end, never an Err"
    );
    let total: usize = chunks.iter().map(|c| c.data.len()).sum();
    assert_eq!(total, 6_400, "everything captured before the stop drains");
}

#[tokio::test]
async fn dropping_the_stream_aborts_the_backend() {
    // §3: abort = drop. The backend must observe it and exit.
    let backend = ScriptedBackend::new(vec![Step::Wait(secs(30.0))]);
    let finished = backend.finished();
    let source = CaptureSource::builder(FMT)
        .backend(Box::new(backend))
        .build();
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
async fn buffer_holds_all_audio_and_never_drops() {
    // §6 (corrected): the capture buffer never drops. 1.0 s is pushed while
    // nobody drains (the pre-ready cold-load window), well within the bound;
    // every chunk must survive.
    let steps = (0..10u8).map(|i| Step::Bytes(vec![i; 3_200])).collect();
    let backend = ScriptedBackend::new(steps);
    let source = CaptureSource::builder(FMT)
        .ring_depth(secs(10.0)) // generous bound; must NOT trip on 1 s
        .backend(Box::new(backend))
        .build();
    let mut stats = source.stats();
    let stream = Box::new(source).capture();

    wait_stats(&mut stats, |s| s.captured >= secs(1.0)).await;

    let (chunks, fault) = drain(stream).await;
    assert!(fault.is_none());
    assert_eq!(chunks.len(), 10, "all 10 captured chunks survive");
    for (i, chunk) in chunks.iter().enumerate() {
        assert_eq!(
            chunk.data[0], i as u8,
            "chunks arrive in capture order, none lost"
        );
    }
}

#[tokio::test]
async fn overload_bound_faults_instead_of_dropping_or_growing() {
    // Past the bound (a service that can't keep up): the accepted audio still
    // drains, then the stream ends with an `Overloaded` fault — never a silent
    // truncation, never unbounded growth. Bound 0.2 s, push 1.0 s with nobody
    // draining.
    use myna_core::CaptureError;
    let steps = (0..10u8).map(|i| Step::Bytes(vec![i; 3_200])).collect();
    let backend = ScriptedBackend::new(steps);
    let source = CaptureSource::builder(FMT)
        .ring_depth(secs(0.2)) // ~6400 bytes: 2 chunks fit, the 3rd overflows
        .backend(Box::new(backend))
        .build();
    let stream = Box::new(source).capture();

    let (chunks, fault) = drain(stream).await;
    assert!(!chunks.is_empty(), "accepted audio drains before the fault");
    assert!(
        matches!(fault, Some(CaptureError::Overloaded(_))),
        "a full buffer faults as Overloaded, got {fault:?}"
    );
}

#[tokio::test]
async fn fault_is_one_err_then_end_after_captured_audio_drains() {
    let backend = ScriptedBackend::new(vec![Step::Silence(secs(0.2)), Step::Fault("boom".into())]);
    let source = CaptureSource::builder(FMT)
        .backend(Box::new(backend))
        .build();
    let stream = Box::new(source).capture();

    let (chunks, fault) = drain(stream).await;
    assert_eq!(
        chunks.len(),
        2,
        "audio captured before the fault still drains"
    );
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
    let loud: Vec<u8> = std::iter::repeat(i16::MAX.to_le_bytes())
        .take(1_600)
        .flatten()
        .collect();
    let backend = ScriptedBackend::new(vec![Step::Bytes(loud)]);
    let source = CaptureSource::builder(FMT)
        .backend(Box::new(backend))
        .build();
    let mut stats = source.stats();
    let _stream = Box::new(source).capture();

    let snapshot = wait_stats(&mut stats, |s| s.captured >= secs(0.1)).await;
    assert!(snapshot.rms > 0.999 && snapshot.peak > 0.999);
    assert!(snapshot.clipped);
}

#[tokio::test]
async fn stats_track_voice_activity() {
    // A quiet lead-in, one second of speech-level signal, then quiet again:
    // the tap marks where sustained voice last ended and reports the input's
    // floor and speech level, so a policy can act on either without samples.
    let quiet = |ms: usize| -> Vec<u8> {
        std::iter::repeat(3i16.to_le_bytes())
            .take(16 * ms)
            .flatten()
            .collect()
    };
    let speech: Vec<u8> = std::iter::repeat(328i16.to_le_bytes())
        .take(16_000)
        .flatten()
        .collect();
    let backend = ScriptedBackend::new(vec![
        Step::Bytes(quiet(300)),
        Step::Bytes(speech),
        Step::Bytes(quiet(1_000)),
    ]);
    let source = CaptureSource::builder(FMT)
        .backend(Box::new(backend))
        .build();
    let mut stats = source.stats();
    let _stream = Box::new(source).capture();

    let snapshot = wait_stats(&mut stats, |s| s.captured >= secs(2.3)).await;
    let mark = snapshot.last_voice.expect("voice was heard");
    assert!(mark > secs(1.3) && mark <= secs(1.4), "mark {mark:?}");
    assert!(
        snapshot.noise_floor < 2e-4,
        "floor {}",
        snapshot.noise_floor
    );
    assert!(
        snapshot.speech_level > 0.009,
        "speech {}",
        snapshot.speech_level
    );
}

#[tokio::test]
async fn short_final_chunk_flushes_whole_frames_only() {
    // Stereo (4-byte frames): 402 bytes pushed → 400 delivered, the trailing
    // partial frame dropped, never padded (§4).
    let stereo = AudioFormat {
        sample_rate_hz: 16_000,
        channels: 2,
        sample_width_bytes: 2,
    };
    let backend = ScriptedBackend::new(vec![Step::Bytes(vec![0u8; 402])]);
    let source = CaptureSource::builder(stereo)
        .backend(Box::new(backend))
        .build();
    let (chunks, fault) = drain(Box::new(source).capture()).await;
    assert!(fault.is_none());
    let total: usize = chunks.iter().map(|c| c.data.len()).sum();
    assert_eq!(total, 400);
    assert!(chunks.iter().all(|c| c.data.len() % 4 == 0));
}

/// Await the first health state matching `pred`, never touching the PCM
/// stream. Panics if health ends or stalls first.
async fn health_until(
    health: &mut CaptureHealthStream,
    pred: impl Fn(&CaptureHealth) -> bool,
) -> CaptureHealth {
    timeout(Duration::from_secs(5), async {
        loop {
            let state = health.next().await.expect("health ended first");
            if pred(&state) {
                return state;
            }
        }
    })
    .await
    .expect("health condition not reached in time")
}

/// The last health state before the stream ends (the backend released).
async fn final_health(mut health: CaptureHealthStream) -> Option<CaptureHealth> {
    timeout(Duration::from_secs(5), async {
        let mut last = None;
        while let Some(state) = health.next().await {
            last = Some(state);
        }
        last
    })
    .await
    .expect("health stream did not end")
}

#[tokio::test]
async fn health_reports_opening_then_capturing_without_draining() {
    let backend = ScriptedBackend::new(vec![
        Step::Wait(secs(0.3)),
        Step::Silence(secs(0.1)),
        Step::Wait(secs(30.0)),
    ]);
    let source = CaptureSource::builder(FMT)
        .backend(Box::new(backend))
        .build();
    let mut health = source.health();
    let _stream = Box::new(source).capture();

    assert_eq!(health.next().await, Some(CaptureHealth::Opening));
    health_until(&mut health, |h| *h == CaptureHealth::Capturing).await;
}

#[tokio::test]
async fn fault_is_visible_on_health_before_the_stream_is_drained() {
    let backend = ScriptedBackend::new(vec![
        Step::Silence(secs(0.2)),
        Step::Fault("boom".into()),
        Step::Wait(secs(30.0)),
    ]);
    let source = CaptureSource::builder(FMT)
        .backend(Box::new(backend))
        .build();
    let health = source.health();
    let stream = Box::new(source).capture();

    // The PCM stream is not polled until health has reported the fault.
    assert_eq!(
        final_health(health).await,
        Some(CaptureHealth::Faulted(CaptureError::Backend("boom".into())))
    );
    let (chunks, fault) = drain(stream).await;
    assert_eq!(
        chunks.len(),
        2,
        "audio captured before the fault still drains"
    );
    assert_eq!(fault, Some(CaptureError::Backend("boom".into())));
}

#[tokio::test]
async fn overload_is_visible_on_health_before_the_stream_is_drained() {
    let steps = (0..10u8)
        .map(|i| Step::Bytes(vec![i; 3_200]))
        .chain([Step::Wait(secs(30.0))])
        .collect();
    let backend = ScriptedBackend::new(steps);
    let source = CaptureSource::builder(FMT)
        .ring_depth(secs(0.2))
        .backend(Box::new(backend))
        .build();
    let mut health = source.health();
    let stream = Box::new(source).capture();

    let state = health_until(&mut health, |h| matches!(h, CaptureHealth::Faulted(_))).await;
    assert!(
        matches!(state, CaptureHealth::Faulted(CaptureError::Overloaded(_))),
        "got {state:?}"
    );
    let (chunks, fault) = drain(stream).await;
    assert_eq!(chunks.len(), 2, "accepted audio drains before the fault");
    assert_eq!(fault.map(CaptureHealth::Faulted), Some(state));
}

#[tokio::test]
async fn open_failure_is_visible_on_health() {
    let source = CaptureSource::builder(FMT)
        .backend(Box::new(ScriptedBackend::unavailable("no mic")))
        .build();
    let health = source.health();
    let stream = Box::new(source).capture();

    let expected = CaptureError::DeviceUnavailable("no mic".into());
    assert_eq!(
        final_health(health).await,
        Some(CaptureHealth::Faulted(expected.clone()))
    );
    let (chunks, fault) = drain(stream).await;
    assert!(chunks.is_empty());
    assert_eq!(fault, Some(expected));
}

#[tokio::test]
async fn graceful_stop_ends_health_once_the_backend_released() {
    let backend = ScriptedBackend::new(vec![Step::Silence(secs(0.2)), Step::Wait(secs(30.0))]);
    let finished = backend.finished();
    let source = CaptureSource::builder(FMT)
        .backend(Box::new(backend))
        .build();
    let mut health = source.health();
    let stop = source.stop_handle();
    let stream = Box::new(source).capture();

    health_until(&mut health, |h| *h == CaptureHealth::Capturing).await;
    stop.stop();
    assert_eq!(final_health(health).await, Some(CaptureHealth::Ended));
    assert!(
        finished.load(Ordering::Acquire),
        "health ends after the backend"
    );
    let (chunks, fault) = drain(stream).await;
    assert!(fault.is_none());
    assert_eq!(
        chunks.len(),
        2,
        "graceful stop still drains every chunk once"
    );
}

#[tokio::test]
async fn abort_ends_health_once_the_backend_released() {
    let backend = ScriptedBackend::new(vec![Step::Silence(secs(0.2)), Step::Wait(secs(30.0))]);
    let finished = backend.finished();
    let source = CaptureSource::builder(FMT)
        .backend(Box::new(backend))
        .build();
    let mut health = source.health();
    let stream = Box::new(source).capture();

    health_until(&mut health, |h| *h == CaptureHealth::Capturing).await;
    drop(stream);
    assert_eq!(final_health(health).await, Some(CaptureHealth::Ended));
    assert!(
        finished.load(Ordering::Acquire),
        "health ends after the backend"
    );
}

/// A backend that loses its producer without an outcome (a bug or a panic on
/// its thread).
struct VanishingBackend;

impl CaptureBackend for VanishingBackend {
    fn start(self: Box<Self>, _spec: CaptureSpec, mut producer: Producer) {
        producer.push(vec![0u8; 3_200].into());
        std::thread::spawn(move || drop(producer));
    }
}

#[tokio::test]
async fn a_backend_that_drops_its_producer_faults_instead_of_hanging() {
    let source = CaptureSource::builder(FMT)
        .backend(Box::new(VanishingBackend))
        .build();
    let health = source.health();
    let stream = Box::new(source).capture();

    let state = final_health(health).await;
    assert!(
        matches!(
            state,
            Some(CaptureHealth::Faulted(CaptureError::Backend(_)))
        ),
        "got {state:?}"
    );
    let (chunks, fault) = drain(stream).await;
    assert_eq!(chunks.len(), 1, "audio pushed before the loss still drains");
    assert!(matches!(fault, Some(CaptureError::Backend(_))));
}
