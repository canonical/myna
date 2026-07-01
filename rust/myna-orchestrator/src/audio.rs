//! The audio-capture boundary (plan T41) — the `AudioSource` trait from
//! `docs/audio-adapter-api.md` §3, plus the [`WavFileSource`] mock used to drive
//! the orchestrator end-to-end before the real PipeWire adapter (Matias, T20)
//! lands. The trait signature matches the audio-adapter API exactly, so that
//! crate drops in behind it unchanged.
//!
//! Invariants honoured (audio-adapter-api §1): the client owns capture and
//! pushes PCM; the source produces **exactly** its declared [`AudioFormat`] and
//! never resamples; nothing is persisted (a WAV file is read, but no audio is
//! written); a fatal capture fault is an `Err` item on the stream, not a silent
//! stall.

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use futures_util::Stream;
use myna_core::{AudioFormat, PcmChunk};
use thiserror::Error;

/// A capture-side fault (audio-adapter-api §4). Surfaced as an `Err` stream item
/// so the dictation service turns it into a terminal session error rather than a
/// silent stall.
#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("audio device unavailable: {0}")]
    DeviceUnavailable(String),
    #[error("requested format {0:?} cannot be produced")]
    UnsupportedFormat(AudioFormat),
    #[error("capture backend failed: {0}")]
    Backend(String),
}

/// The stream a source yields once capturing: chunks until a clean end (`None`)
/// or a fatal fault (one `Err`, then `None`).
pub type CaptureStream = Pin<Box<dyn Stream<Item = Result<PcmChunk, CaptureError>> + Send>>;

/// A source of push-side PCM (audio-adapter-api §3). The dictation service sets
/// the exact [`AudioFormat`] from the STT service's advertised capabilities; the
/// source produces exactly that and nothing else.
pub trait AudioSource: Send {
    /// The exact format this source emits.
    fn format(&self) -> AudioFormat;

    /// Begin capture, consuming the source.
    fn capture(self: Box<Self>) -> CaptureStream;
}

/// A cheap, cloneable graceful-stop handle (audio-adapter-api §5): setting it
/// makes an in-flight [`WavFileSource`] capture **drain then end** (stream
/// yields `None`), which the orchestrator reads as end-of-audio — the clean
/// hotkey-release path. Dropping the stream instead is the abort path.
#[derive(Clone, Debug, Default)]
pub struct StopHandle(Arc<AtomicBool>);

impl StopHandle {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Streams an uncompressed PCM WAV file as chunks in its native format — the
/// WAV-backed mock for the orchestrator demo and tests. `realtime` paces chunks
/// at capture rate (mimicking a live mic for lab-style runs); disabled, it
/// streams as fast as the consumer accepts (fast tests). Mirrors the Python
/// `WavFileSource`.
pub struct WavFileSource {
    format: AudioFormat,
    data: Bytes,
    chunk_seconds: f64,
    realtime: bool,
    stop: StopHandle,
}

impl WavFileSource {
    /// Open and parse a WAV file, reading its PCM into memory (clips are seconds
    /// long). Fails if the file is missing or not uncompressed PCM.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, CaptureError> {
        let path: PathBuf = path.as_ref().to_path_buf();
        let bytes = std::fs::read(&path)
            .map_err(|e| CaptureError::DeviceUnavailable(format!("{}: {e}", path.display())))?;
        let (format, data) = parse_wav(&bytes)
            .map_err(|e| CaptureError::Backend(format!("{}: {e}", path.display())))?;
        Ok(Self { format, data, chunk_seconds: 0.1, realtime: false, stop: StopHandle::default() })
    }

    /// Set the chunk size in seconds (default 0.1 s ≈ the prototype's ~100 ms).
    pub fn with_chunk_seconds(mut self, seconds: f64) -> Self {
        self.chunk_seconds = seconds;
        self
    }

    /// Pace chunks at real time (default off).
    pub fn realtime(mut self, realtime: bool) -> Self {
        self.realtime = realtime;
        self
    }

    /// A handle to stop this source gracefully (drain then end).
    pub fn stop_handle(&self) -> StopHandle {
        self.stop.clone()
    }
}

/// Internal per-chunk cursor for the capture stream.
struct WavCursor {
    data: Bytes,
    pos: usize,
    bytes_per_chunk: usize,
    format: AudioFormat,
    realtime: bool,
    stop: StopHandle,
}

impl AudioSource for WavFileSource {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn capture(self: Box<Self>) -> CaptureStream {
        let frame_bytes = (self.format.channels as usize)
            .saturating_mul(self.format.sample_width_bytes as usize)
            .max(1);
        let frames_per_chunk =
            ((self.format.sample_rate_hz as f64) * self.chunk_seconds).round().max(1.0) as usize;
        let bytes_per_chunk = frames_per_chunk.saturating_mul(frame_bytes).max(frame_bytes);

        let cursor = WavCursor {
            data: self.data,
            pos: 0,
            bytes_per_chunk,
            format: self.format,
            realtime: self.realtime,
            stop: self.stop,
        };

        Box::pin(futures_util::stream::unfold(cursor, |mut c| async move {
            // Graceful stop (hotkey release): drain done, end the stream.
            if c.stop.is_stopped() || c.pos >= c.data.len() {
                return None;
            }
            let end = (c.pos + c.bytes_per_chunk).min(c.data.len());
            let chunk = PcmChunk::new(c.data.slice(c.pos..end), c.format);
            c.pos = end;
            if c.realtime {
                tokio::time::sleep(chunk.duration()).await;
            }
            Some((Ok(chunk), c))
        }))
    }
}

/// Minimal RIFF/WAVE parser: returns the PCM [`AudioFormat`] and the `data`
/// bytes. Accepts only uncompressed PCM (format tag 1) — the client owns any
/// conversion, so a compressed file is a `Backend` error, never silently
/// decoded.
fn parse_wav(bytes: &[u8]) -> Result<(AudioFormat, Bytes), String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut fmt: Option<AudioFormat> = None;
    let mut data: Option<Bytes> = None;
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let size = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap()) as usize;
        let body = i + 8;
        let body_end = body.saturating_add(size).min(bytes.len());
        match id {
            b"fmt " => {
                let b = &bytes[body..body_end];
                if b.len() < 16 {
                    return Err("truncated fmt chunk".into());
                }
                let tag = u16::from_le_bytes([b[0], b[1]]);
                if tag != 1 {
                    return Err(format!("unsupported WAV format tag {tag} (only PCM=1)"));
                }
                let channels = u16::from_le_bytes([b[2], b[3]]);
                let sample_rate = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                let bits = u16::from_le_bytes([b[14], b[15]]);
                fmt = Some(AudioFormat {
                    sample_rate_hz: sample_rate,
                    channels: channels.max(1) as u8,
                    sample_width_bytes: (bits / 8).max(1) as u8,
                });
            }
            b"data" => data = Some(Bytes::copy_from_slice(&bytes[body..body_end])),
            _ => {}
        }
        // Chunks are word-aligned: an odd size is followed by a pad byte.
        i = body + size + (size & 1);
    }
    match (fmt, data) {
        (Some(f), Some(d)) => Ok((f, d)),
        (None, _) => Err("missing fmt chunk".into()),
        (_, None) => Err("missing data chunk".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    /// Build a minimal canonical PCM WAV (44-byte header + data) for tests.
    fn wav(format: AudioFormat, data: &[u8]) -> Vec<u8> {
        let byte_rate = format.bytes_per_second();
        let block_align = (format.channels as u16) * (format.sample_width_bytes as u16);
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&(format.channels as u16).to_le_bytes());
        out.extend_from_slice(&format.sample_rate_hz.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&((format.sample_width_bytes as u16) * 8).to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    fn temp_wav(bytes: &[u8]) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("myna-wav-{}-{}.wav", std::process::id(), nanos));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn parses_format_from_header() {
        let fmt = AudioFormat::default();
        let bytes = wav(fmt, &[0u8; 3200]);
        let (parsed, data) = parse_wav(&bytes).unwrap();
        assert_eq!(parsed, fmt);
        assert_eq!(data.len(), 3200);
    }

    #[test]
    fn rejects_non_pcm() {
        let mut bytes = wav(AudioFormat::default(), &[0u8; 10]);
        // Corrupt the format tag (offset 20) to 3 (IEEE float).
        bytes[20] = 3;
        assert!(parse_wav(&bytes).unwrap_err().contains("format tag"));
    }

    #[tokio::test]
    async fn chunks_cover_all_data_in_declared_format() {
        let fmt = AudioFormat::default(); // 16 kHz mono s16 → 32 000 B/s
        let bytes = wav(fmt, &vec![7u8; 32_000]); // 1 s
        let path = temp_wav(&bytes);
        let source = WavFileSource::new(&path).unwrap().with_chunk_seconds(0.1);
        assert_eq!(source.format(), fmt);

        let mut stream = Box::new(source).capture();
        let mut total = 0usize;
        let mut count = 0usize;
        while let Some(item) = stream.next().await {
            let chunk = item.unwrap();
            assert_eq!(chunk.format, fmt);
            total += chunk.data.len();
            count += 1;
        }
        assert_eq!(total, 32_000, "every PCM byte is delivered");
        assert_eq!(count, 10, "1 s of audio at 0.1 s chunks");
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn stop_handle_ends_capture_gracefully() {
        let fmt = AudioFormat::default();
        let bytes = wav(fmt, &vec![0u8; 32_000]);
        let path = temp_wav(&bytes);
        let source = WavFileSource::new(&path).unwrap();
        let stop = source.stop_handle();
        stop.stop(); // release before any chunk is pulled
        let mut stream = Box::new(source).capture();
        assert!(stream.next().await.is_none(), "stopped source drains to nothing");
        std::fs::remove_file(&path).ok();
    }
}
