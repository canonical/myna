use myna_audio_adapter::{open_stream, StreamConfig};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = StreamConfig::default();
    let start = Instant::now();
    let mut stream = open_stream(&config)?;

    let mut first_frame_time: Option<Duration> = None;
    let mut last_real_timestamp = Duration::ZERO;
    let mut read_count = 0;

    let test_start = Instant::now();
    while test_start.elapsed() < Duration::from_secs(5) {
        let items = stream.read_timeout(Duration::from_millis(100))?;
        for item in items {
            match item {
                myna_audio_adapter::StreamItem::Frame(frame) => {
                    if first_frame_time.is_none() {
                        first_frame_time = Some(start.elapsed());
                    }
                    last_real_timestamp = frame.timestamp + frame.duration;
                    read_count += 1;
                }
                myna_audio_adapter::StreamItem::Event(event) => {
                    eprintln!("event: {:?}", event);
                }
                _ => {}
            }
        }
    }

    let close_start = Instant::now();
    stream.close()?;
    let close_time = close_start.elapsed();

    let first_frame_ms = first_frame_time.map(|d| d.as_secs_f64() * 1000.0).unwrap_or(f64::INFINITY);
    let lag = if last_real_timestamp > Duration::ZERO {
        let capture_elapsed = start.elapsed();
        (capture_elapsed.saturating_sub(last_real_timestamp)).as_secs_f64() * 1000.0
    } else {
        f64::INFINITY
    };

    println!("first_frame_ms = {first_frame_ms:.1}");
    println!("steady_state_lag_ms = {lag:.1}");
    println!("close_ms = {}", close_time.as_secs_f64() * 1000.0);
    println!("frames_read = {read_count}");

    let mut fail = false;
    if first_frame_ms > 100.0 {
        eprintln!("FAIL: first frame took {first_frame_ms:.1} ms (limit 100 ms)");
        fail = true;
    }
    if lag > 100.0 {
        eprintln!("FAIL: steady-state lag {lag:.1} ms (limit 100 ms)");
        fail = true;
    }
    if close_time > Duration::from_millis(200) {
        eprintln!("FAIL: close took {:?} (limit 200 ms)", close_time);
        fail = true;
    }

    if fail {
        std::process::exit(1);
    }
    Ok(())
}
