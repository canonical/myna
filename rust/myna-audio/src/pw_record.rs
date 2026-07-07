//! [`PwRecordBackend`] (plan T51) — live PipeWire capture via a `pw-record`
//! subprocess: the Rust port of the Python `MicSource` prototype
//! (`myna/testbed/sources.py`), behind the [`CaptureBackend`] seam.
//!
//! `pw-record --raw` writes PCM to stdout in exactly the requested
//! rate/channels/format — PipeWire's own graph link does the resample/downmix,
//! which is how this backend meets the produce-exactly-the-negotiated-format
//! contract (audio-adapter-api §7). Audio streams from the pipe straight into
//! the adapter's ring; nothing touches disk (§1.2).
//!
//! Lifecycle: reads are bounded (250 ms) so the stop flag is honored promptly
//! (§5); on stop or consumer-gone the process is killed and everything already
//! read drains. `pw-record` records until killed, so an EOF *we didn't ask
//! for* means the device/daemon went away — a fault, with the child's stderr
//! attached for diagnosis.
//!
//! What it can't do (needs the native backend, T52): channel-index
//! pick/downmix (`spec.channels` is rejected, never silently mis-captured)
//! and device enumeration.

use std::process::Stdio;
use std::time::Duration;

use bytes::Bytes;
use myna_core::CaptureError;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::backend::{CaptureBackend, CaptureSpec, Producer};

/// Bounded-read interval: the worst-case stop latency, inside the ~250 ms
/// promptness contract (§5). In practice reads return far sooner — a live
/// node produces continuously (silence is still data).
const READ_TIMEOUT: Duration = Duration::from_millis(250);

/// Live microphone capture via a `pw-record` subprocess.
#[derive(Default)]
pub struct PwRecordBackend {
    /// Test seam: a program + args standing in for `pw-record` (which the
    /// test environment may not have, or may have no device for).
    command: Option<Vec<String>>,
}

impl PwRecordBackend {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn with_command(command: Vec<String>) -> Self {
        Self { command: Some(command) }
    }

    fn build_command(&self, spec: &CaptureSpec) -> Command {
        if let Some(parts) = &self.command {
            let mut cmd = Command::new(&parts[0]);
            cmd.args(&parts[1..]);
            return cmd;
        }
        let mut cmd = Command::new("pw-record");
        cmd.arg("--raw")
            .arg("--rate")
            .arg(spec.format.sample_rate_hz.to_string())
            .arg("--channels")
            .arg(spec.format.channels.to_string())
            .arg("--format")
            .arg("s16");
        if let Some(target) = &spec.target {
            cmd.arg("--target").arg(target);
        }
        cmd.arg("-"); // raw PCM to stdout
        cmd
    }
}

impl CaptureBackend for PwRecordBackend {
    fn start(self: Box<Self>, spec: CaptureSpec, mut producer: Producer) -> Result<(), CaptureError> {
        if spec.channels.is_some() {
            return Err(CaptureError::Backend(
                "pw-record cannot pick channel indices; channel selection needs the native backend (T52)"
                    .into(),
            ));
        }
        if spec.format.sample_width_bytes != 2 {
            // --format s16 is the only encoding in the format universe today
            // (audio-adapter-api §2, pending T33).
            return Err(CaptureError::UnsupportedFormat(spec.format));
        }

        let mut cmd = self.build_command(&spec);
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| CaptureError::DeviceUnavailable(format!("cannot spawn pw-record: {e}")))?;
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");

        tokio::spawn(async move {
            // Drain stderr concurrently so a chatty child can't block on it;
            // its content is only surfaced on a fault.
            let stderr_task = tokio::spawn(async move {
                let mut text = String::new();
                let _ = stderr.read_to_string(&mut text).await;
                text
            });

            // ~100 ms of audio per read: the adapter re-chunks anyway.
            let mut buf = vec![0u8; (spec.format.bytes_per_second() / 10).max(256) as usize];
            let mut fault: Option<CaptureError> = None;
            let mut eof = false;
            loop {
                if spec.stop.is_stopped() {
                    break;
                }
                match timeout(READ_TIMEOUT, stdout.read(&mut buf)).await {
                    Err(_elapsed) => continue, // bounded wait: re-check stop
                    Ok(Ok(0)) => {
                        eof = true;
                        break;
                    }
                    Ok(Ok(n)) => {
                        if !producer.push(Bytes::copy_from_slice(&buf[..n])) {
                            break; // consumer gone
                        }
                    }
                    Ok(Err(e)) => {
                        fault =
                            Some(CaptureError::Backend(format!("reading from pw-record: {e}")));
                        break;
                    }
                }
            }

            // On stop/abort/fault the child is still recording — kill it. On
            // EOF it already exited and start_kill is a harmless no-op error.
            let _ = child.start_kill();
            let status = child.wait().await;
            // Bounded: an orphaned grandchild can hold the stderr pipe open
            // past the kill; losing its output must not wedge the finish.
            let stderr_text = timeout(Duration::from_millis(500), stderr_task)
                .await
                .ok()
                .and_then(|joined| joined.ok())
                .unwrap_or_default();

            if eof && fault.is_none() && !spec.stop.is_stopped() {
                // pw-record records until killed; an unrequested EOF means
                // the device or the PipeWire daemon went away mid-capture.
                let mut detail = match status {
                    Ok(s) => format!(" ({s})"),
                    Err(_) => String::new(),
                };
                let stderr_text = stderr_text.trim();
                if !stderr_text.is_empty() {
                    let brief: String = stderr_text.chars().take(200).collect();
                    detail.push_str(&format!(": {brief}"));
                }
                fault = Some(CaptureError::DeviceUnavailable(format!(
                    "pw-record exited mid-capture{detail}"
                )));
            }
            producer.finish(fault);
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CaptureSource;
    use futures_util::StreamExt;
    use myna_core::{AudioFormat, AudioSource, PcmChunk};
    use std::time::Duration;

    const FMT: AudioFormat =
        AudioFormat { sample_rate_hz: 16_000, channels: 1, sample_width_bytes: 2 };

    fn sh(script: &str) -> Box<PwRecordBackend> {
        Box::new(PwRecordBackend::with_command(vec![
            "sh".into(),
            "-c".into(),
            script.into(),
        ]))
    }

    async fn drain(
        mut stream: myna_core::CaptureStream,
    ) -> (Vec<PcmChunk>, Option<CaptureError>) {
        let mut chunks = Vec::new();
        let mut fault = None;
        while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.next())
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
    async fn stop_kills_the_subprocess_and_drains_cleanly() {
        // An endless producer (a stand-in for a live mic). Small ring so the
        // endless push just ages out (drop-oldest) instead of growing.
        let source = CaptureSource::builder(FMT)
            .ring_depth(Duration::from_millis(200))
            .backend(sh("exec cat /dev/zero"))
            .build();
        let mut stats = source.stats();
        let stop = source.stop_handle();
        let stream = Box::new(source).capture();

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if stats.borrow_and_update().captured >= Duration::from_millis(200) {
                    break;
                }
                stats.changed().await.unwrap();
            }
        })
        .await
        .expect("no audio arrived from the subprocess");
        stop.stop();

        let (chunks, fault) = drain(stream).await;
        assert!(fault.is_none(), "graceful stop is a clean end: {fault:?}");
        assert!(!chunks.is_empty(), "captured audio drains after the stop");
    }

    #[tokio::test]
    async fn unrequested_eof_is_a_device_fault_with_stderr_attached() {
        // The child emits 0.2 s of audio, complains, and dies — a vanished
        // device. Captured audio still drains before the single Err.
        let source = CaptureSource::builder(FMT)
            .backend(sh("head -c 6400 /dev/zero; echo doom >&2; exit 1"))
            .build();
        let (chunks, fault) = drain(Box::new(source).capture()).await;
        let total: usize = chunks.iter().map(|c| c.data.len()).sum();
        assert_eq!(total, 6_400);
        match fault {
            Some(CaptureError::DeviceUnavailable(msg)) => {
                assert!(msg.contains("doom"), "stderr in the fault: {msg}");
            }
            other => panic!("expected DeviceUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_failure_is_device_unavailable() {
        let backend =
            Box::new(PwRecordBackend::with_command(vec!["/nonexistent/pw-record".into()]));
        let source = CaptureSource::builder(FMT).backend(backend).build();
        let (chunks, fault) = drain(Box::new(source).capture()).await;
        assert!(chunks.is_empty());
        assert!(matches!(fault, Some(CaptureError::DeviceUnavailable(_))));
    }

    #[tokio::test]
    async fn channel_selection_is_rejected_not_miscaptured() {
        let source = CaptureSource::builder(FMT)
            .channels(vec![9, 10])
            .backend(Box::new(PwRecordBackend::new()))
            .build();
        let (chunks, fault) = drain(Box::new(source).capture()).await;
        assert!(chunks.is_empty());
        match fault {
            Some(CaptureError::Backend(msg)) => assert!(msg.contains("T52")),
            other => panic!("expected Backend rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_s16_width_is_unsupported() {
        let odd = AudioFormat { sample_rate_hz: 16_000, channels: 1, sample_width_bytes: 4 };
        let source = CaptureSource::builder(odd).backend(sh("cat /dev/zero")).build();
        let (chunks, fault) = drain(Box::new(source).capture()).await;
        assert!(chunks.is_empty());
        assert!(matches!(fault, Some(CaptureError::UnsupportedFormat(_))));
    }
}
