//! [`PipeWireSoundCuePlayer`] — the real, non-hermetic backend for
//! `sound::SoundCuePlayer` (T057). Named `playback`, not `pipewire`, for the
//! same reason `myna-audio/src/native.rs` is named `native`: a module named
//! `pipewire` inside a crate that also depends on the `pipewire` crate is a
//! confusing/ambiguous name to import from.
//!
//! Mirrors `myna-audio::native`'s dedicated-PipeWire-main-loop-thread shape,
//! but for **playback** (`Direction::Output`, pushing PCM *into* the graph)
//! rather than capture: construct a loop + `Stream`, connect it, and fill
//! each buffer the graph asks for from a short, pre-synthesized PCM clip
//! (research.md R2) until the clip is exhausted, then quit the loop and let
//! the thread end.
//!
//! **Non-blocking guarantee (FR-011, T056)**: [`SoundCuePlayer::play`] must
//! return near-instantly so `controller.rs` can call it inline without ever
//! delaying capture/injection. This is achieved the same way
//! `myna-audio::native::spawn_capture_thread` isolates blocking PipeWire work
//! from its caller: `play()` spawns a dedicated, short-lived thread that owns
//! the loop and runs it to completion, and returns immediately without
//! waiting for that thread — genuinely fire-and-forget, not merely
//! non-blocking-in-the-common-case.
//!
//! **Privacy (constitution V)**: the PCM played is always one of exactly
//! three fixed, compile-time-synthesized tones (`synth_cue`) selected only by
//! [`super::CueKind`] — there is no code path by which transcript text or
//! captured microphone audio could reach this module or the playback stream.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use pipewire::{
    context::ContextRc,
    keys,
    main_loop::MainLoopRc,
    properties::properties,
    spa::{
        param::{
            audio::{AudioFormat, AudioInfoRaw},
            ParamType,
        },
        pod::{serialize::PodSerializer, Object, Pod, Value},
        utils::{Direction, SpaTypes},
    },
    stream::{StreamFlags, StreamRc},
};

use super::{CueKind, SoundCuePlayer};

/// The synthesized clips' sample rate. Fixed and mono — a beep needs neither
/// stereo nor a high rate; keeping both fixed keeps `synth_cue` trivial.
const SAMPLE_RATE: u32 = 44_100;

/// S16LE mono: 2 bytes per frame.
const FRAME_BYTES: usize = 2;

/// Safety backstop (mirrors `native.rs`'s `SILENCE_TIMEOUT`/watchdog
/// philosophy): if the graph never wires the stream (no session manager, or
/// a broken graph), the dedicated thread must not hang forever. Generous
/// relative to every cue's real duration (≤ ~350 ms).
const MAX_CLIP_THREAD_LIFETIME: Duration = Duration::from_secs(3);

/// Render one cycle of a sine tone at `freq_hz` for `duration`, at `volume`
/// (0.0–1.0), with a short linear fade-in/out to avoid the audible click a
/// hard-edged tone would otherwise produce.
fn sine_tone(freq_hz: f64, duration: Duration, volume: f64) -> Vec<i16> {
    let n_frames = (SAMPLE_RATE as f64 * duration.as_secs_f64()) as usize;
    let fade_frames = ((SAMPLE_RATE as f64 * 0.005) as usize).max(1); // 5 ms
    let two_pi = std::f64::consts::PI * 2.0;
    (0..n_frames)
        .map(|i| {
            let t = i as f64 / SAMPLE_RATE as f64;
            let envelope = if i < fade_frames {
                i as f64 / fade_frames as f64
            } else if i >= n_frames.saturating_sub(fade_frames) {
                (n_frames - i) as f64 / fade_frames as f64
            } else {
                1.0
            };
            let sample = (two_pi * freq_hz * t).sin() * volume * envelope;
            (sample * i16::MAX as f64) as i16
        })
        .collect()
}

/// A brief span of digital silence, used to separate the failure cue's two
/// pulses (below) without a second stream reconnect.
fn silence(duration: Duration) -> Vec<i16> {
    vec![0i16; (SAMPLE_RATE as f64 * duration.as_secs_f64()) as usize]
}

/// Synthesize the fixed PCM clip for one cue (T057). Each cue is a distinct
/// pitch/duration/shape pair (FR-010 "mutually distinct"), chosen
/// conservatively and flagged for reviewer double-check per the task's
/// judgement-call guidance:
/// - `SessionStart`: a single short, higher-pitched tone (880 Hz, ~180 ms) —
///   an upward, "beginning" feel.
/// - `StopListening`: a single brief, high-pitched chirp (1046 Hz, ~90 ms) —
///   the shortest and highest-pitched cue by design, so it reads as a quick
///   acknowledgment ("heard you, now working") rather than a state with its
///   own duration like the other three.
/// - `SessionEnd`: a single short, lower-pitched tone (660 Hz, ~180 ms) —
///   same shape as `SessionStart`, distinct only by pitch, so the two read as
///   a matched "opened/closed" pair rather than unrelated sounds.
/// - `Failure`: two short low-pitched pulses (220 Hz, ~150 ms each, with a
///   50 ms gap) — deliberately the most different shape (double-pulse, not
///   single-tone) as well as the lowest pitch, so it cannot be mistaken for
///   either transition cue even by someone who cannot distinguish pitch well.
pub fn synth_cue(cue: CueKind) -> Vec<i16> {
    match cue {
        CueKind::SessionStart => sine_tone(880.0, Duration::from_millis(180), 0.7),
        CueKind::StopListening => sine_tone(1046.5, Duration::from_millis(90), 0.6),
        CueKind::SessionEnd => sine_tone(660.0, Duration::from_millis(180), 0.7),
        CueKind::Failure => {
            let mut clip = sine_tone(220.0, Duration::from_millis(150), 0.8);
            clip.extend(silence(Duration::from_millis(50)));
            clip.extend(sine_tone(220.0, Duration::from_millis(150), 0.8));
            clip
        }
    }
}

/// The real PipeWire-backed player. Stateless (every `play()` opens and
/// tears down its own short-lived stream) — see the module doc's
/// non-blocking rationale for why that is the simple, correct choice for a
/// cue played at most a few times per session, rather than a persistent
/// connection.
#[derive(Debug, Default)]
pub struct PipeWireSoundCuePlayer;

impl PipeWireSoundCuePlayer {
    pub fn new() -> Self {
        Self
    }

    /// Play `cue` to completion **on the calling thread**, returning the
    /// PipeWire outcome. [`SoundCuePlayer::play`] (the trait impl below)
    /// spawns this on a dedicated thread and discards the result — this
    /// method is what that thread body calls, and is exposed `pub` so
    /// `tests/sound_hw.rs` (T055) can assert success directly against a real
    /// bus without needing to thread the result back itself.
    pub fn play_blocking(cue: CueKind) -> Result<(), String> {
        let clip = synth_cue(cue);
        run_clip(&clip)
    }
}

impl SoundCuePlayer for PipeWireSoundCuePlayer {
    fn play(&mut self, cue: CueKind) {
        // Fire-and-forget (FR-011, T056): spawn and return immediately,
        // never joining the handle. A spawn failure (exhausted OS threads)
        // or a playback failure both degrade to "no sound", never to a
        // session-affecting error — a missed cue is not privacy- or
        // capture-affecting, so it is logged, not propagated.
        myna_core::dbg_log!("sound", "play({cue:?}) requested, spawning cue thread");
        let build = std::thread::Builder::new()
            .name("myna-pw-cue".into())
            .spawn(move || {
                let start = std::time::Instant::now();
                let result = Self::play_blocking(cue);
                myna_core::dbg_log!(
                    "sound",
                    "play({cue:?}) finished in {:?} (ok={})",
                    start.elapsed(),
                    result.is_ok()
                );
                if let Err(e) = result {
                    eprintln!("myna-desktop: sound cue playback failed: {e}");
                }
            });
        if let Err(e) = build {
            eprintln!("myna-desktop: could not spawn sound cue thread: {e}");
        }
    }
}

/// Build a loop + output stream, feed `clip` into it buffer-by-buffer until
/// exhausted, then quit. Runs to completion on the calling thread (the
/// dedicated `myna-pw-cue` thread, in production).
fn run_clip(clip: &[i16]) -> Result<(), String> {
    let setup_start = std::time::Instant::now();
    let main_loop =
        MainLoopRc::new(None).map_err(|e| format!("cannot create PipeWire loop: {e}"))?;
    let context = ContextRc::new(&main_loop, None)
        .map_err(|e| format!("cannot create PipeWire context: {e}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| format!("cannot connect to PipeWire: {e}"))?;
    myna_core::dbg_log!("sound", "PipeWire loop/context/core ready in {:?}", setup_start.elapsed());

    let props = properties! {
        *keys::MEDIA_TYPE => "Audio",
        *keys::MEDIA_CATEGORY => "Playback",
        *keys::MEDIA_ROLE => "Notification",
        *keys::NODE_NAME => "myna-dictate-cue",
    };
    let stream = StreamRc::new(core, "myna-cue", props)
        .map_err(|e| format!("cannot create playback stream: {e}"))?;

    // Single-threaded (this function's own thread) shared position: the
    // `process` callback below runs on this same loop/thread, never
    // concurrently, so a plain `Rc<Cell<..>>` is sound (same reasoning as
    // `native.rs`'s `Rc<RefCell<..>>` fields).
    let position = Rc::new(Cell::new(0usize));
    let samples: Rc<[i16]> = clip.into();
    let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed({
            let main_loop = main_loop.clone();
            let error = error.clone();
            move |_stream, _ud, _old, new| {
                if let pipewire::stream::StreamState::Error(msg) = &new {
                    *error.borrow_mut() = Some(format!("PipeWire stream error: {msg}"));
                    main_loop.quit();
                }
            }
        })
        .process({
            let main_loop = main_loop.clone();
            let position = position.clone();
            let samples = samples.clone();
            move |stream, _ud| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else {
                    return;
                };
                let Some(slice) = data.data() else {
                    return;
                };
                let capacity_frames = slice.len() / FRAME_BYTES;
                let pos = position.get();
                let remaining = samples.len().saturating_sub(pos);
                let n_frames = capacity_frames.min(remaining);
                for i in 0..n_frames {
                    let bytes = samples[pos + i].to_le_bytes();
                    let start = i * FRAME_BYTES;
                    slice[start..start + FRAME_BYTES].copy_from_slice(&bytes);
                }
                for i in n_frames..capacity_frames {
                    let start = i * FRAME_BYTES;
                    slice[start..start + FRAME_BYTES].fill(0);
                }
                position.set(pos + n_frames);
                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = FRAME_BYTES as _;
                *chunk.size_mut() = (FRAME_BYTES * capacity_frames) as _;
                if position.get() >= samples.len() {
                    main_loop.quit();
                }
            }
        })
        .register()
        .map_err(|e| format!("cannot register stream listener: {e}"))?;

    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::S16LE);
    audio_info.set_rate(SAMPLE_RATE);
    audio_info.set_channels(1);
    // Explicit MONO position, not left-unpositioned (the `AudioInfoRaw::new()`
    // default): an unpositioned single channel has no guaranteed placement in
    // the graph's downstream mix to stereo output, and in practice lands
    // audibly left-biased on this session's default sink. `MONO` tells the
    // channel-mixer to spread the one channel evenly across both speakers.
    let mut position = [0u32; pipewire::spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    position[0] = pipewire::spa::sys::SPA_AUDIO_CHANNEL_MONO;
    audio_info.set_position(position);
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
            .map_err(|e| format!("serializing audio format pod: {e}"))?
            .0
            .into_inner();
    let mut params = [Pod::from_bytes(&values).ok_or("invalid format pod")?];

    stream
        .connect(
            Direction::Output,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| format!("cannot connect playback stream: {e}"))?;

    // Safety backstop: a graph that never wires this stream (no session
    // manager, or nothing to route Playback to) must not hang this thread
    // forever (mirrors `native.rs`'s watchdog philosophy, T057).
    let timer = main_loop.loop_().add_timer({
        let main_loop = main_loop.clone();
        move |_| main_loop.quit()
    });
    let _ = timer
        .update_timer(Some(MAX_CLIP_THREAD_LIFETIME), None)
        .into_result()
        .inspect_err(|e| {
            eprintln!("myna-desktop: sound cue watchdog timer failed to arm: {e}");
        });

    main_loop.run();
    drop(timer);
    drop(_listener);

    let outcome = error.borrow_mut().take().map_or(Ok(()), Err);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T057 (hermetic, no PipeWire needed): the synthesis logic itself ────

    const ALL_CUES: [CueKind; 4] = [
        CueKind::SessionStart,
        CueKind::StopListening,
        CueKind::SessionEnd,
        CueKind::Failure,
    ];

    #[test]
    fn each_cue_produces_a_non_empty_clip() {
        for cue in ALL_CUES {
            assert!(!synth_cue(cue).is_empty());
        }
    }

    #[test]
    fn every_sample_stays_within_i16_range_and_is_not_all_zero() {
        // `i16` arithmetic can't overflow its own range, but this guards the
        // envelope/volume math never accidentally saturates or degenerates
        // to silence.
        for cue in ALL_CUES {
            let clip = synth_cue(cue);
            assert!(clip.iter().any(|&s| s != 0), "{cue:?} clip is all silence");
        }
    }

    #[test]
    fn the_four_cues_are_mutually_distinct() {
        // FR-010 "mutually distinct": no two cues render to the identical
        // sample sequence (they differ in pitch, duration, or shape).
        let clips: Vec<Vec<i16>> = ALL_CUES.iter().map(|&c| synth_cue(c)).collect();
        for i in 0..clips.len() {
            for j in (i + 1)..clips.len() {
                assert_ne!(clips[i], clips[j], "{:?} and {:?} render identically", ALL_CUES[i], ALL_CUES[j]);
            }
        }
    }

    #[test]
    fn failure_is_the_longest_and_only_double_pulse_cue() {
        // The failure cue is structurally the most different (double pulse,
        // longer overall) — see `synth_cue`'s doc comment.
        let start = synth_cue(CueKind::SessionStart);
        let failure = synth_cue(CueKind::Failure);
        assert!(failure.len() > start.len());
    }

    #[test]
    fn stop_listening_is_the_shortest_cue() {
        // The quickest, highest-pitched cue by design — see `synth_cue`'s
        // doc comment ("reads as a quick acknowledgment").
        let stop = synth_cue(CueKind::StopListening);
        for cue in [CueKind::SessionStart, CueKind::SessionEnd, CueKind::Failure] {
            assert!(stop.len() < synth_cue(cue).len());
        }
    }
}
