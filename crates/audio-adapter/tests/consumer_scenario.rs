//! Consumer-scenario test (G15, FR-020): replays the Speech Controller's
//! push-to-talk call pattern from contracts/audio-adapter-api.md §Known
//! consumer against MockBackend. The same flow runs against a real server and
//! virtual node as `speech_controller_session_flow` in the sandbox suite
//! (tasks T048).

use myna_audio_adapter::backend::AudioBackend;
use myna_audio_adapter::{
    open_stream_with_backend, MockBackend, StreamConfig, StreamEvent, StreamItem,
};
use std::time::{Duration, Instant};

#[test]
fn speech_controller_call_pattern() {
    // 1. Settings time: enumerate nodes and inspect metadata.
    let backend = MockBackend::with_node_id("consumer-node");
    let nodes = backend.enumerate().expect("enumerate should succeed");
    assert!(!nodes.is_empty());
    assert!(!nodes[0].name.is_empty() && !nodes[0].description.is_empty());
    assert!(!nodes[0].supported_formats.is_empty());

    // 2. Session start (hotkey press): open the stream.
    let config = StreamConfig::default();
    let mut stream =
        open_stream_with_backend(&config, Box::new(backend)).expect("open should succeed");

    // 3. Recording: timed read loop, matching items the way the Speech
    //    Controller does (frames to inference, events to session logic).
    let mut audio_bytes = 0usize;
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(250) {
        for item in stream
            .read_timeout(Duration::from_millis(50))
            .expect("read should not error")
        {
            match item {
                StreamItem::Frame(frame) => {
                    assert_eq!(frame.format, *stream.target_format());
                    audio_bytes += frame.data.len(); // "send to inference"
                }
                StreamItem::Event(StreamEvent::DeviceLost { .. }) => {
                    panic!("mock should not lose the device in this scenario")
                }
                StreamItem::Event(_) => {} // diagnostics (Overrun/Underrun/VAD)
                _ => {}
            }
        }
    }
    assert!(audio_bytes > 0, "should have streamed audio to inference");

    // Idempotent re-open mid-session returns the same underlying stream:
    // its timeline continues instead of restarting at zero.
    let mut again = open_stream_with_backend(
        &config,
        Box::new(MockBackend::with_node_id("consumer-node")),
    )
    .expect("second open should succeed");
    let items = again.read_timeout(Duration::from_millis(100)).unwrap();
    let first_frame = items.iter().find_map(|i| match i {
        StreamItem::Frame(f) => Some(f),
        _ => None,
    });
    if let Some(frame) = first_frame {
        assert!(
            frame.timestamp >= Duration::from_millis(100),
            "timeline restarted — open was not idempotent"
        );
    }

    // 4. Session end (hotkey release): close, releasing the source.
    let closed_at = Instant::now();
    stream.close().expect("close should succeed");
    assert!(closed_at.elapsed() < Duration::from_millis(200));
    assert!(again.is_closed(), "close must apply to the shared stream");
}
