use myna_audio_adapter::{enumerate_nodes, open_stream, StreamConfig};
use std::time::Duration;

#[test]
fn speech_controller_call_pattern() {
    // 1. Settings: enumerate nodes.
    let nodes = enumerate_nodes().expect("enumerate should work in test-util mode");
    assert!(!nodes.is_empty(), "at least one mock node should exist");

    // 2. Session start: open default stream.
    let config = StreamConfig::default();
    let mut stream = open_stream(&config).expect("open_stream should succeed");

    // 3. Incremental streaming reads with event/error handling.
    let mut frames = 0;
    let mut _events = 0;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(250) {
        let items = stream.read_timeout(Duration::from_millis(50)).expect("read should not error");
        for item in items {
            match item {
                myna_audio_adapter::StreamItem::Frame(_) => frames += 1,
                myna_audio_adapter::StreamItem::Event(_) => _events += 1,
                _ => {}
            }
        }
    }

    assert!(frames > 0, "should have received audio frames");

    // Idempotent re-open returns the same stream.
    let stream2 = open_stream(&config).expect("second open should succeed");
    assert_eq!(stream.node().id, stream2.node().id);

    // 4. Session end: close stream.
    stream.close().expect("close should succeed");
}
