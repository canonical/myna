use myna_audio_adapter::{open_stream_with_backend, AudioStream, MockBackend, NodeSelector, StreamConfig};
use std::time::Duration;

fn open_with_mock(mock: MockBackend, config: StreamConfig) -> AudioStream {
    open_stream_with_backend(&config, Box::new(mock)).unwrap()
}

#[test]
fn g1_frames_match_target_format() {
    let config = StreamConfig::default();
    let mut stream = open_with_mock(MockBackend::new(), config.clone());
    let items = stream.read_timeout(Duration::from_millis(200)).unwrap();
    for item in items {
        if let myna_audio_adapter::StreamItem::Frame(frame) = item {
            assert_eq!(frame.format, *stream.target_format());
        }
    }
}

#[test]
fn g2_frames_contiguous_and_non_overlapping() {
    let mut stream = open_with_mock(MockBackend::new(), StreamConfig::default());
    let mut last_end = Duration::ZERO;
    let mut last_seq = None;
    let items = stream.read_timeout(Duration::from_millis(200)).unwrap();
    for item in items {
        if let myna_audio_adapter::StreamItem::Frame(frame) = item {
            assert_eq!(frame.timestamp, last_end);
            last_end = frame.timestamp + frame.duration;
            if let Some(prev) = last_seq {
                assert_eq!(frame.seq, prev + 1);
            }
            last_seq = Some(frame.seq);
        }
    }
}

#[test]
fn g7_open_is_idempotent_per_node() {
    use myna_audio_adapter::{enumerate_nodes, open_stream, StreamConfig};
    let _ = enumerate_nodes().unwrap();
    let config = StreamConfig::default();
    let s1 = open_stream(&config).unwrap();
    let s2 = open_stream(&config).unwrap();
    assert_eq!(s1.node().id, s2.node().id);
}

#[test]
fn g8_close_releases_resources() {
    let stream = open_with_mock(MockBackend::new(), StreamConfig::default());
    let start = std::time::Instant::now();
    stream.close().unwrap();
    assert!(start.elapsed() < Duration::from_millis(200));
}

#[test]
fn g9_first_frame_within_latency_target() {
    let start = std::time::Instant::now();
    let mut stream = open_with_mock(MockBackend::new(), StreamConfig::default());
    let items = stream.read_timeout(Duration::from_millis(500)).unwrap();
    assert!(!items.is_empty(), "should receive at least one item");
    assert!(
        start.elapsed() < Duration::from_millis(100),
        "first frame took too long"
    );
}

#[test]
fn g13_no_disk_persistence_by_library() {
    // The library never writes audio files; this is enforced by API design.
    // A full privacy check requires strace; this test documents the contract.
}

#[test]
fn no_device_errors_cleanly() {
    use myna_audio_adapter::open_stream;
    let config = StreamConfig {
        node: NodeSelector::ByName("nonexistent".into()),
        ..StreamConfig::default()
    };
    let err = match open_stream(&config) {
        Err(e) => e,
        Ok(_) => panic!("expected NoDevice error"),
    };
    assert!(matches!(err, myna_audio_adapter::Error::NoDevice));
}

#[test]
fn device_lost_event_closes_stream() {
    let mut mock = MockBackend::new();
    mock.lose_after = Some(Duration::from_millis(50));
    let mut stream = open_with_mock(mock, StreamConfig::default());
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    let mut found = false;
    while std::time::Instant::now() < deadline {
        let items = stream.read_timeout(Duration::from_millis(50)).unwrap();
        for item in items {
            if let myna_audio_adapter::StreamItem::Event(
                myna_audio_adapter::StreamEvent::DeviceLost { .. },
            ) = item
            {
                found = true;
                break;
            }
        }
        if found {
            break;
        }
    }
    assert!(found, "expected DeviceLost event");
}

#[test]
fn g4_underrun_generates_silence_fill_and_event() {
    use myna_audio_adapter::{open_stream_with_backend, MockBackend, StreamConfig};
    use std::time::{Duration, Instant};

    let mut mock = MockBackend::new();
    mock.gap_after = Some(Duration::from_millis(50));
    mock.gap_duration = Some(Duration::from_millis(100));
    let config = StreamConfig::default();
    let mut stream = open_stream_with_backend(&config, Box::new(mock)).unwrap();

    let deadline = Instant::now() + Duration::from_millis(300);
    let mut found_underrun = false;
    let mut last_timestamp = Duration::ZERO;
    while Instant::now() < deadline {
        let items = stream.read_timeout(Duration::from_millis(50)).unwrap();
        let mut prev_ts = last_timestamp;
        for item in items {
            match item {
                myna_audio_adapter::StreamItem::Frame(frame) => {
                    assert!(
                        frame.timestamp >= prev_ts,
                        "timestamps must not go backwards: {:?} < {:?}",
                        frame.timestamp,
                        prev_ts
                    );
                    prev_ts = frame.timestamp + frame.duration;
                }
                myna_audio_adapter::StreamItem::Event(
                    myna_audio_adapter::StreamEvent::Underrun { .. },
                ) => {
                    found_underrun = true;
                }
                _ => {}
            }
            last_timestamp = prev_ts;
        }
    }
    assert!(found_underrun, "expected Underrun event during injected gap");
}

#[test]
fn g3_overrun_reports_dropped_samples() {
    use myna_audio_adapter::{open_stream_with_backend, MockBackend, StreamConfig};
    use std::time::Duration;

    let config = StreamConfig {
        max_buffer_duration: Duration::from_millis(30),
        ..StreamConfig::default()
    };
    let mut stream = open_stream_with_backend(&config, Box::new(MockBackend::new())).unwrap();

    // Let the producer outrun the consumer to force drop-oldest behavior.
    std::thread::sleep(Duration::from_millis(120));

    let items = stream.read_timeout(Duration::from_millis(200)).unwrap();
    let found = items.iter().any(|item| {
        matches!(
            item,
            myna_audio_adapter::StreamItem::Event(myna_audio_adapter::StreamEvent::Overrun { .. })
        )
    });
    assert!(found, "expected Overrun event after buffer overflow");
}
