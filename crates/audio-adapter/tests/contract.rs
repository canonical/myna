//! Contract tests (G1–G15, contracts/audio-adapter-api.md) against MockBackend.
//!
//! Each test uses a unique node id: streams are registered globally per node
//! (FR-003), and tests run concurrently in one process.

use myna_audio_adapter::{
    open_stream_with_backend, AudioStream, MockBackend, NodeSelector, StreamConfig, StreamEvent,
    StreamItem,
};
use std::time::{Duration, Instant};

const TONE: i16 = 1000; // MockBackend's constant amplitude
const FADE_CEILING: i16 = 200; // a faded boundary sample must be well below TONE

fn open(node_id: &str, config: StreamConfig) -> AudioStream {
    open_stream_with_backend(&config, Box::new(MockBackend::with_node_id(node_id))).unwrap()
}

fn frames(items: &[StreamItem]) -> Vec<&myna_audio_adapter::AudioFrame> {
    items
        .iter()
        .filter_map(|i| match i {
            StreamItem::Frame(f) => Some(f),
            _ => None,
        })
        .collect()
}

fn first_samples(frame: &myna_audio_adapter::AudioFrame) -> &[i16] {
    bytemuck::cast_slice(&frame.data)
}

#[test]
fn g1_frames_match_target_format() {
    let mut stream = open("g1-node", StreamConfig::default());
    let items = stream.read_timeout(Duration::from_millis(200)).unwrap();
    assert!(!items.is_empty());
    for frame in frames(&items) {
        assert_eq!(frame.format, *stream.target_format());
    }
}

#[test]
fn g2_frames_contiguous_and_non_overlapping() {
    let mut stream = open("g2-node", StreamConfig::default());
    let mut collected = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(150);
    while Instant::now() < deadline {
        collected.extend(stream.read_timeout(Duration::from_millis(50)).unwrap());
    }
    let frames = frames(&collected);
    assert!(frames.len() >= 3, "expected several frames");
    for pair in frames.windows(2) {
        assert_eq!(
            pair[1].timestamp,
            pair[0].timestamp + pair[0].duration(),
            "timestamps must be contiguous"
        );
        assert_eq!(pair[1].seq, pair[0].seq + 1, "seq must be gapless");
    }
}

#[test]
fn g3_overrun_drops_oldest_reports_once_and_smooths_splice() {
    let config = StreamConfig {
        max_buffer_duration: Duration::from_millis(30),
        ..StreamConfig::default()
    };
    let mut stream = open("g3-node", config);

    // Let the producer outrun the consumer to force drop-oldest.
    std::thread::sleep(Duration::from_millis(150));

    let items = stream.read_timeout(Duration::from_millis(200)).unwrap();
    let overruns: Vec<_> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| match item {
            StreamItem::Event(StreamEvent::Overrun { dropped }) => Some((i, *dropped)),
            _ => None,
        })
        .collect();
    assert_eq!(overruns.len(), 1, "exactly one Overrun per loss span");
    let (idx, dropped) = overruns[0];
    assert!(dropped > Duration::ZERO);

    // The first frame delivered after the Overrun has a smoothed (faded-in)
    // head; audio away from the splice is untouched.
    let post = items[idx..]
        .iter()
        .find_map(|i| match i {
            StreamItem::Frame(f) => Some(f),
            _ => None,
        })
        .expect("frames must follow the overrun report");
    let samples = first_samples(post);
    assert!(
        samples[0].abs() < FADE_CEILING,
        "splice head not smoothed: {}",
        samples[0]
    );
    assert_eq!(samples[samples.len() / 2], TONE, "mid-frame audio altered");
}

#[test]
fn g4_underrun_fills_silence_reports_event_and_smooths_boundaries() {
    let mut mock = MockBackend::with_node_id("g4-node");
    mock.gap_after = Some(Duration::from_millis(50));
    mock.gap_duration = Some(Duration::from_millis(40));
    let mut stream = open_stream_with_backend(&StreamConfig::default(), Box::new(mock)).unwrap();

    let mut collected = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < deadline {
        collected.extend(stream.read_timeout(Duration::from_millis(50)).unwrap());
    }

    // Underrun event with the real filled duration.
    let filled = collected
        .iter()
        .find_map(|i| match i {
            StreamItem::Event(StreamEvent::Underrun { filled }) => Some(*filled),
            _ => None,
        })
        .expect("expected an Underrun event");
    assert_eq!(filled, Duration::from_millis(40));

    // Timeline stays continuous through the silence fill.
    let frames = frames(&collected);
    for pair in frames.windows(2) {
        assert_eq!(pair[1].timestamp, pair[0].timestamp + pair[0].duration());
        assert_eq!(pair[1].seq, pair[0].seq + 1);
    }

    // A genuinely silent span exists, and the first real frame after it has a
    // smoothed head.
    let silence_idx = frames
        .iter()
        .position(|f| first_samples(f).iter().all(|s| *s == 0))
        .expect("expected a silence-fill frame");
    let post = frames
        .iter()
        .skip(silence_idx + 1)
        .find(|f| first_samples(f).iter().any(|s| *s != 0))
        .expect("expected real audio after the fill");
    let samples = first_samples(post);
    assert!(
        samples[0].abs() < FADE_CEILING,
        "fill boundary not smoothed: {}",
        samples[0]
    );
}

#[test]
fn g7_open_is_idempotent_per_node() {
    let config = StreamConfig::default();
    let mut s1 = open("g7-node", config.clone());
    std::thread::sleep(Duration::from_millis(60));
    // Drain what the stream captured so far and remember where the timeline is.
    let drained = s1.read().unwrap();
    let last_ts = frames(&drained)
        .last()
        .map(|f| f.timestamp + f.duration())
        .expect("stream should have produced frames");

    // A second open on the same node — even via a different backend instance —
    // must return the already-open stream, not start a new capture: its reads
    // continue the shared timeline instead of restarting at zero.
    let mut s2 = open("g7-node", config);
    let items = s2.read_timeout(Duration::from_millis(100)).unwrap();
    let frames = frames(&items);
    assert!(!frames.is_empty());
    assert!(
        frames[0].timestamp >= last_ts,
        "fresh stream detected (timeline restarted at {:?}) — open was not idempotent",
        frames[0].timestamp
    );
}

#[test]
fn g8_close_releases_resources_promptly_for_all_handles() {
    let config = StreamConfig::default();
    let s1 = open("g8-node", config.clone());
    let mut s2 = open("g8-node", config); // same underlying stream (G7)

    let start = Instant::now();
    s1.close().unwrap();
    assert!(start.elapsed() < Duration::from_millis(200), "close too slow");

    // Close is effective for every handle: buffers cleared, no new frames.
    assert!(s2.is_closed());
    std::thread::sleep(Duration::from_millis(30));
    let items = s2.read().unwrap();
    assert!(
        frames(&items).is_empty(),
        "frames delivered after close: {}",
        items.len()
    );
}

#[test]
fn g9_first_frame_within_latency_target() {
    let start = Instant::now();
    let mut stream = open("g9-node", StreamConfig::default());
    let items = stream.read_timeout(Duration::from_millis(500)).unwrap();
    assert!(!items.is_empty(), "should receive at least one item");
    assert!(
        start.elapsed() < Duration::from_millis(100),
        "first frame took too long: {:?}",
        start.elapsed()
    );
}

#[test]
fn no_device_errors_cleanly() {
    let config = StreamConfig {
        node: NodeSelector::ByName("nonexistent".into()),
        ..StreamConfig::default()
    };
    match open_stream_with_backend(&config, Box::new(MockBackend::with_node_id("nd-node"))) {
        Err(err) => assert!(matches!(err, myna_audio_adapter::Error::NoDevice)),
        Ok(_) => panic!("expected NoDevice error"),
    }
}

#[test]
fn device_lost_event_reaches_consumer() {
    let mut mock = MockBackend::with_node_id("dl-node");
    mock.lose_after = Some(Duration::from_millis(50));
    let mut stream = open_stream_with_backend(&StreamConfig::default(), Box::new(mock)).unwrap();

    let deadline = Instant::now() + Duration::from_millis(500);
    let mut found = false;
    while Instant::now() < deadline && !found {
        let items = stream.read_timeout(Duration::from_millis(50)).unwrap();
        found = items
            .iter()
            .any(|i| matches!(i, StreamItem::Event(StreamEvent::DeviceLost { .. })));
    }
    assert!(found, "expected DeviceLost event");
}

#[test]
fn preprocess_flags_are_rejected_until_implemented() {
    let config = StreamConfig {
        preprocess: myna_audio_adapter::PreprocessConfig {
            denoise: true,
            ..Default::default()
        },
        ..StreamConfig::default()
    };
    match open_stream_with_backend(&config, Box::new(MockBackend::with_node_id("pp-node"))) {
        Err(err) => assert!(matches!(err, myna_audio_adapter::Error::Backend(_))),
        Ok(_) => panic!("preprocess flags must not be silently ignored"),
    }
}
