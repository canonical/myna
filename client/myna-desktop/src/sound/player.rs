//! The [`Chime`] that plays Myna's own cues, compiled into the daemon from
//! `sounds/<set>/` (made by `dev/synth_cues.py`), and the [`Previewer`] that
//! plays a whole set when Myna Settings asks for it.
//!
//! Playing happens on a thread of its own, one job at a time: a cue takes as
//! long as its sound, and the controller must not wait for it. A cue that
//! cannot be played is logged and dropped; dictation never depends on it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use myna_audio::playback::{self, Clip};
use myna_core::SoundSet;

use super::{Chime, Cue};
use crate::live::Live;

/// Jobs waiting behind the one playing; more than a session's worth is a
/// burst, and the excess is dropped.
const BACKLOG: usize = 3;

/// A preview plays the cues in a session's order.
const PREVIEW: [Cue; 3] = [Cue::Start, Cue::Stop, Cue::Error];

/// The silence between a preview's cues, so each is heard as its own event.
const PREVIEW_GAP: Duration = Duration::from_millis(500);

/// The Ogg Vorbis file each cue of each set plays.
fn sound(set: SoundSet, cue: Cue) -> &'static [u8] {
    macro_rules! set {
        ($dir:literal) => {
            match cue {
                Cue::Start => include_bytes!(concat!("../../sounds/", $dir, "/start.oga")),
                Cue::Stop => include_bytes!(concat!("../../sounds/", $dir, "/stop.oga")),
                Cue::Error => include_bytes!(concat!("../../sounds/", $dir, "/error.oga")),
            }
        };
    }
    match set {
        SoundSet::Myna => set!("myna"),
        SoundSet::Tine => set!("tine"),
        SoundSet::Hum => set!("hum"),
    }
}

fn decode(set: SoundSet, cue: Cue) -> Result<Clip, String> {
    Clip::decode_ogg(std::io::Cursor::new(sound(set, cue))).map_err(|e| e.to_string())
}

enum Job {
    /// A session cue, from the set in force when it plays.
    Cue(Cue),
    /// Every cue of one set, in order.
    Preview(SoundSet),
}

pub struct Player {
    jobs: mpsc::SyncSender<Job>,
    previewing: Arc<AtomicBool>,
}

impl Player {
    /// Start the player thread, playing through PipeWire from whichever set
    /// `set` holds at each cue.
    pub fn spawn(set: Live<SoundSet>) -> std::io::Result<Self> {
        Self::spawn_with(
            set,
            |clip| playback::play(clip).map_err(|e| e.to_string()),
            std::thread::sleep,
        )
    }

    /// A player with `output` in place of PipeWire and no real pauses, for
    /// tests of what drives it.
    #[cfg(test)]
    pub(crate) fn spawn_with_output(
        set: Live<SoundSet>,
        output: impl FnMut(&Clip) -> Result<(), String> + Send + 'static,
    ) -> std::io::Result<Self> {
        Self::spawn_with(set, output, |_| {})
    }

    fn spawn_with(
        set: Live<SoundSet>,
        mut output: impl FnMut(&Clip) -> Result<(), String> + Send + 'static,
        pause: impl Fn(Duration) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (jobs, queue) = mpsc::sync_channel::<Job>(BACKLOG);
        let previewing = Arc::new(AtomicBool::new(false));
        let mut play = move |set: SoundSet, cue: Cue| {
            if let Err(why) = decode(set, cue).and_then(|clip| output(&clip)) {
                myna_core::info_log!("sound", "{set:?} {cue:?} cue not played: {why}");
            }
        };
        std::thread::Builder::new()
            .name("myna-sound".into())
            .spawn({
                let previewing = previewing.clone();
                move || {
                    for job in queue {
                        match job {
                            Job::Cue(cue) => play(set.get(), cue),
                            Job::Preview(preview) => {
                                for (i, cue) in PREVIEW.into_iter().enumerate() {
                                    if i > 0 {
                                        pause(PREVIEW_GAP);
                                    }
                                    play(preview, cue);
                                }
                                previewing.store(false, Ordering::Release);
                            }
                        }
                    }
                }
            })?;
        Ok(Self { jobs, previewing })
    }

    /// A handle that previews sets on this player, for the D-Bus object.
    pub fn previewer(&self) -> Previewer {
        Previewer {
            jobs: self.jobs.clone(),
            previewing: self.previewing.clone(),
        }
    }
}

impl Chime for Player {
    fn play(&self, cue: Cue) {
        let _ = self.jobs.try_send(Job::Cue(cue));
    }
}

/// Plays a whole set, start, stop and error, on the daemon's player.
#[derive(Clone)]
pub struct Previewer {
    jobs: mpsc::SyncSender<Job>,
    previewing: Arc<AtomicBool>,
}

/// A preview is already queued or playing, or the player is backlogged.
/// Refused rather than queued: a second press would replay the set over the
/// end of the first.
#[derive(Debug, PartialEq, Eq)]
pub struct PreviewBusy;

impl Previewer {
    pub fn preview(&self, set: SoundSet) -> Result<(), PreviewBusy> {
        if self.previewing.swap(true, Ordering::AcqRel) {
            return Err(PreviewBusy);
        }
        self.jobs.try_send(Job::Preview(set)).map_err(|_| {
            self.previewing.store(false, Ordering::Release);
            PreviewBusy
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    const CUES: [Cue; 3] = [Cue::Start, Cue::Stop, Cue::Error];

    /// Every cue of every set decodes, is short, and starts and ends in near
    /// silence: a clip that begins or stops above zero clicks. How loud it is
    /// is the next test's.
    #[test]
    fn each_cue_is_a_short_clip_that_fades_in_and_out() {
        for set in SoundSet::ALL {
            for cue in CUES {
                let clip = decode(set, cue).unwrap_or_else(|e| panic!("{set:?} {cue:?}: {e}"));
                let ms = clip.duration().as_millis();
                assert!((300..=600).contains(&ms), "{set:?} {cue:?} lasts {ms} ms");
                let edge = (clip.rate() / 1000 * clip.channels()) as usize;
                let samples = clip.samples();
                let loudest = |part: &[f32]| part.iter().fold(0f32, |m, s| m.max(s.abs()));
                assert!(
                    loudest(&samples[..edge]) < 0.01,
                    "{set:?} {cue:?} starts loud"
                );
                assert!(
                    loudest(&samples[samples.len() - edge..]) < 0.001,
                    "{set:?} {cue:?} ends loud"
                );
            }
        }
    }

    /// BS.1770 K-weighted loudness of a 48 kHz mono clip, gated as
    /// `dev/synth_cues.py` measures it: 50 ms blocks overlapping by half and a
    /// -20 dB relative gate, since a cue is shorter than the standard's 400 ms
    /// block.
    fn loudness(samples: &[f32]) -> f64 {
        let biquad = |x: &[f64], b: [f64; 3], a: [f64; 2]| {
            let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
            x.iter()
                .map(|&x0| {
                    let y0 = b[0] * x0 + b[1] * x1 + b[2] * x2 - a[0] * y1 - a[1] * y2;
                    (x2, x1, y2, y1) = (x1, x0, y1, y0);
                    y0
                })
                .collect::<Vec<_>>()
        };
        let x: Vec<f64> = samples.iter().map(|&s| f64::from(s)).collect();
        let shelf = biquad(
            &x,
            [1.53512485958697, -2.69169618940638, 1.19839281085285],
            [-1.69065929318241, 0.73248077421585],
        );
        let y = biquad(
            &shelf,
            [1.0, -2.0, 1.0],
            [-1.99004745483398, 0.99007225036621],
        );
        let block = 2400;
        let power: Vec<f64> = (0..y.len() - block)
            .step_by(block / 2)
            .map(|i| y[i..i + block].iter().map(|v| v * v).sum::<f64>() / block as f64)
            .collect();
        let gate = power.iter().cloned().fold(0.0, f64::max) * 0.01;
        let kept: Vec<f64> = power.into_iter().filter(|&p| p > gate).collect();
        -0.691 + 10.0 * (kept.iter().sum::<f64>() / kept.len() as f64).log10()
    }

    /// Every set is equally loud as heard, not by peak: a band-limited set
    /// peaks lower than a struck one at the same loudness. Start at -16 LUFS,
    /// stop 2 LU quieter, error 1 LU louder, as `dev/synth_cues.py` renders
    /// them, and no cue clips.
    #[test]
    fn each_cue_is_loudness_matched_and_never_clips() {
        for set in SoundSet::ALL {
            for (cue, target) in [(Cue::Start, -16.0), (Cue::Stop, -18.0), (Cue::Error, -15.0)] {
                let clip = decode(set, cue).unwrap();
                assert_eq!(
                    (clip.rate(), clip.channels()),
                    (48_000, 1),
                    "{set:?} {cue:?}"
                );
                let lufs = loudness(clip.samples());
                assert!(
                    (lufs - target).abs() < 0.5,
                    "{set:?} {cue:?} is {lufs:.1} LUFS, not {target}"
                );
                let peak = clip.samples().iter().fold(0f32, |m, s| m.max(s.abs()));
                assert!(peak < 0.95, "{set:?} {cue:?} peaks at {peak}");
            }
        }
    }

    #[test]
    fn every_cue_of_every_set_is_its_own_sound() {
        let all: Vec<_> = SoundSet::ALL
            .into_iter()
            .flat_map(|set| CUES.map(|cue| sound(set, cue)))
            .collect();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    /// What the output was handed, as (sample count, first samples) so two
    /// clips compare without holding whole buffers.
    fn fingerprint(clip: &Clip) -> (usize, Vec<f32>) {
        (clip.samples().len(), clip.samples()[..4800].to_vec())
    }

    fn expected(set: SoundSet, cue: Cue) -> (usize, Vec<f32>) {
        fingerprint(&decode(set, cue).unwrap())
    }

    type Heard = mpsc::Receiver<(usize, Vec<f32>)>;

    /// A player whose output reports each clip and whose pauses are recorded
    /// instead of slept.
    fn recording_player(set: Live<SoundSet>) -> (Player, Heard, Arc<Mutex<Vec<Duration>>>) {
        let (played, heard) = mpsc::channel();
        let pauses = Arc::new(Mutex::new(Vec::new()));
        let player = Player::spawn_with(
            set,
            move |clip| {
                played.send(fingerprint(clip)).unwrap();
                Ok(())
            },
            {
                let pauses = pauses.clone();
                move |gap| pauses.lock().unwrap().push(gap)
            },
        )
        .unwrap();
        (player, heard, pauses)
    }

    fn next(heard: &Heard) -> (usize, Vec<f32>) {
        heard
            .recv_timeout(Duration::from_secs(5))
            .expect("a clip plays")
    }

    /// Each cue reaches the output as its own clip, in order, and one that
    /// fails to play does not stop the ones after it.
    #[test]
    fn the_player_plays_each_cue_in_turn_past_a_failure() {
        let (played, heard) = mpsc::channel();
        let mut first = true;
        let chime = Player::spawn_with(
            Live::new(SoundSet::Myna),
            move |clip| {
                played.send(fingerprint(clip)).unwrap();
                if std::mem::take(&mut first) {
                    return Err("no sink".into());
                }
                Ok(())
            },
            |_| {},
        )
        .unwrap();

        for cue in CUES {
            chime.play(cue);
        }
        for cue in CUES {
            assert_eq!(next(&heard), expected(SoundSet::Myna, cue));
        }
    }

    /// A set picked in Settings is heard from the next cue, no restart.
    #[test]
    fn a_cue_plays_from_the_set_in_force_when_it_plays() {
        let set = Live::new(SoundSet::Myna);
        let (chime, heard, _) = recording_player(set.clone());
        chime.play(Cue::Start);
        assert_eq!(next(&heard), expected(SoundSet::Myna, Cue::Start));
        set.set(SoundSet::Hum);
        chime.play(Cue::Stop);
        assert_eq!(next(&heard), expected(SoundSet::Hum, Cue::Stop));
    }

    /// A preview plays the set it names, whatever the setting holds, as
    /// start, stop and error with a gap before each but the first.
    #[test]
    fn a_preview_plays_the_named_set_in_session_order_with_gaps() {
        let (player, heard, pauses) = recording_player(Live::new(SoundSet::Myna));
        assert_eq!(player.previewer().preview(SoundSet::Tine), Ok(()));
        for cue in CUES {
            assert_eq!(next(&heard), expected(SoundSet::Tine, cue));
        }
        assert_eq!(*pauses.lock().unwrap(), [PREVIEW_GAP, PREVIEW_GAP]);
    }

    /// A second preview while one plays is refused, not stacked; once the
    /// first has finished the next is welcome.
    #[test]
    fn a_preview_during_a_preview_is_refused_until_it_ends() {
        let (gate, opened) = mpsc::channel::<()>();
        let (played, heard) = mpsc::channel();
        let player = Player::spawn_with(
            Live::new(SoundSet::Myna),
            move |clip| {
                opened.recv().unwrap();
                played.send(fingerprint(clip)).unwrap();
                Ok(())
            },
            |_| {},
        )
        .unwrap();
        let previewer = player.previewer();

        assert_eq!(previewer.preview(SoundSet::Hum), Ok(()));
        assert_eq!(previewer.preview(SoundSet::Tine), Err(PreviewBusy));
        for cue in CUES {
            gate.send(()).unwrap();
            assert_eq!(next(&heard), expected(SoundSet::Hum, cue));
        }
        // The flag clears just after the last clip; give the thread a moment.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while previewer.preview(SoundSet::Tine).is_err() {
            assert!(std::time::Instant::now() < deadline, "preview never freed");
            std::thread::yield_now();
        }
        gate.send(()).unwrap();
        assert_eq!(next(&heard), expected(SoundSet::Tine, Cue::Start));
    }

    /// A full backlog refuses the preview and does not leave it marked as
    /// playing, or no preview would ever be accepted again.
    #[test]
    fn a_backlogged_player_refuses_a_preview_and_recovers() {
        let (gate, opened) = mpsc::channel::<()>();
        let player = Player::spawn_with(
            Live::new(SoundSet::Myna),
            move |_| {
                opened.recv().map_err(|e| e.to_string())?;
                Ok(())
            },
            |_| {},
        )
        .unwrap();
        // One playing (blocked on the gate) and BACKLOG waiting.
        player.play(Cue::Start);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut queued = 0;
        while queued < BACKLOG {
            assert!(std::time::Instant::now() < deadline, "backlog never filled");
            if player.jobs.try_send(Job::Cue(Cue::Stop)).is_ok() {
                queued += 1;
            }
        }
        let previewer = player.previewer();
        assert_eq!(previewer.preview(SoundSet::Hum), Err(PreviewBusy));
        assert!(!previewer.previewing.load(Ordering::Acquire));
        drop(gate);
    }
}
