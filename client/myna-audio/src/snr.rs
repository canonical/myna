//! Blind signal-to-noise estimate for the stats tap: WADA-SNR (Kim and Stern,
//! "Robust signal-to-noise ratio estimation based on waveform amplitude
//! distribution analysis", Interspeech 2008). Speech amplitudes follow a
//! Gamma(0.4) distribution and noise a Gaussian one; the statistic
//! `G = ln(mean|x|) - mean(ln|x|)` moves monotonically from one to the other
//! as the SNR rises, and is unchanged by gain.
//!
//! Measured on the speech band: rumble below a few hundred hertz costs a
//! model nothing, and counting it read fans and road noise as noisy input.

use myna_core::PcmChunk;

use crate::voice::{HighPass, FRAME};

/// The cutoff that keeps rumble out of the estimate.
pub const HIGH_PASS_HZ: f32 = 150.0;

/// Audio the estimate needs before it means anything.
const MIN_SECONDS: f64 = 0.5;

/// Floor for `ln|x|`, so an exact zero inside a frame does not reach -inf.
const MIN_MAGNITUDE: f64 = 1e-10;

/// The table's first entry is this SNR; entries step by 1 dB.
const TABLE_MIN_DB: f32 = -10.0;

/// `G` for Gamma(0.4) speech plus Gaussian noise at -10..=60 dB SNR, by
/// Monte Carlo over 4M samples. Below -10 dB the curve is flat.
const WADA_G: [f32; 71] = [
    0.4123, 0.4136, 0.4151, 0.4173, 0.4204, 0.4241, 0.4289, 0.4351, //
    0.4425, 0.4513, 0.4620, 0.4745, 0.4889, 0.5053, 0.5237, 0.5438, //
    0.5659, 0.5894, 0.6141, 0.6401, 0.6676, 0.6959, 0.7247, 0.7538, //
    0.7832, 0.8127, 0.8420, 0.8711, 0.8998, 0.9284, 0.9561, 0.9831, //
    1.0096, 1.0355, 1.0608, 1.0852, 1.1087, 1.1316, 1.1534, 1.1746, //
    1.1949, 1.2144, 1.2330, 1.2509, 1.2684, 1.2851, 1.3010, 1.3161, //
    1.3305, 1.3445, 1.3578, 1.3705, 1.3826, 1.3944, 1.4057, 1.4164, //
    1.4267, 1.4366, 1.4460, 1.4549, 1.4635, 1.4717, 1.4794, 1.4867, //
    1.4939, 1.5006, 1.5072, 1.5133, 1.5193, 1.5250, 1.5304,
];

/// Per-session WADA-SNR estimate of a capture. Feed it every chunk in order.
#[derive(Clone, Debug)]
pub struct SnrEstimator {
    filter: HighPass,
    channels: usize,
    frame_samples: usize,
    min_samples: u64,
    heard: Wada,
}

impl SnrEstimator {
    pub fn new(sample_rate_hz: u32, channels: usize) -> Self {
        let channels = channels.max(1);
        let rate = f64::from(sample_rate_hz.max(1));
        Self {
            filter: HighPass::new(sample_rate_hz, channels, HIGH_PASS_HZ),
            channels,
            frame_samples: ((rate * FRAME.as_secs_f64()) as usize * channels).max(channels),
            min_samples: (rate * MIN_SECONDS) as u64 * channels as u64,
            heard: Wada::default(),
        }
    }

    /// Observe one chunk. S16LE only, like the rest of the tap. A frame of
    /// digital silence (a muted input, a Bluetooth profile switch) still runs
    /// through the filter but stays out of the estimate.
    pub fn observe(&mut self, chunk: &PcmChunk) {
        if chunk.format.sample_width_bytes != 2 {
            return;
        }
        for frame in chunk.data.chunks(self.frame_samples * 2) {
            let mut silent = true;
            let mut this = Wada::default();
            for (i, s) in frame.chunks_exact(2).enumerate() {
                let raw = i16::from_le_bytes([s[0], s[1]]);
                silent &= raw == 0;
                this.add(
                    self.filter
                        .filter(i % self.channels, f32::from(raw) / 32768.0),
                );
            }
            if !silent {
                self.heard.merge(&this);
            }
        }
    }

    /// The session's SNR in dB, clamped to -10..=60; `None` until half a
    /// second of non-silent audio has been heard.
    pub fn snr_db(&self) -> Option<f32> {
        (self.heard.count >= self.min_samples)
            .then(|| self.heard.snr_db())
            .flatten()
    }
}

/// The running sums behind `G`.
#[derive(Clone, Copy, Debug, Default)]
struct Wada {
    sum_abs: f64,
    sum_ln: f64,
    count: u64,
}

impl Wada {
    fn add(&mut self, sample: f32) {
        let magnitude = f64::from(sample.abs());
        self.sum_abs += magnitude;
        self.sum_ln += magnitude.max(MIN_MAGNITUDE).ln();
        self.count += 1;
    }

    fn merge(&mut self, other: &Wada) {
        self.sum_abs += other.sum_abs;
        self.sum_ln += other.sum_ln;
        self.count += other.count;
    }

    fn snr_db(&self) -> Option<f32> {
        if self.sum_abs <= 0.0 {
            return None;
        }
        let n = self.count as f64;
        Some(snr_for(((self.sum_abs / n).ln() - self.sum_ln / n) as f32))
    }
}

/// Linear interpolation of the table, clamped at both ends.
fn snr_for(g: f32) -> f32 {
    let i = WADA_G.partition_point(|&t| t < g);
    if i == 0 {
        return TABLE_MIN_DB;
    }
    if i == WADA_G.len() {
        return TABLE_MIN_DB + (WADA_G.len() - 1) as f32;
    }
    let (lo, hi) = (WADA_G[i - 1], WADA_G[i]);
    TABLE_MIN_DB + (i - 1) as f32 + (g - lo) / (hi - lo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use myna_core::AudioFormat;

    const RATE: u32 = 16_000;

    /// xorshift64*: deterministic and dependency-free.
    struct Rng(u64);

    impl Rng {
        fn uniform(&mut self) -> f64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            ((self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }

        fn gauss(&mut self) -> f64 {
            let (u, v) = (self.uniform(), self.uniform());
            (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
        }

        /// Gamma(shape < 1) by Marsaglia and Tsang's boost.
        fn gamma(&mut self, shape: f64) -> f64 {
            let d = shape + 1.0 - 1.0 / 3.0;
            let c = 1.0 / (9.0 * d).sqrt();
            let boosted = loop {
                let x = self.gauss();
                let v = (1.0 + c * x).powi(3);
                if v > 0.0 && self.uniform().ln() < 0.5 * x * x + d - d * v + d * v.ln() {
                    break d * v;
                }
            };
            boosted * self.uniform().powf(1.0 / shape)
        }
    }

    /// Gamma(0.4) "speech" at unit RMS plus white noise at `snr_db`.
    fn mixture(seconds: f64, snr_db: f64, seed: u64) -> Vec<f64> {
        let mut rng = Rng(seed);
        let n = (seconds * f64::from(RATE)) as usize;
        let speech: Vec<f64> = (0..n)
            .map(|_| {
                let sign = if rng.uniform() < 0.5 { -1.0 } else { 1.0 };
                sign * rng.gamma(0.4)
            })
            .collect();
        let rms = (speech.iter().map(|s| s * s).sum::<f64>() / n as f64).sqrt();
        let sigma = 10f64.powf(-snr_db / 20.0);
        speech
            .iter()
            .map(|s| s / rms + sigma * rng.gauss())
            .collect()
    }

    fn chunk(samples: &[f64], gain: f64) -> PcmChunk {
        let data: Vec<u8> = samples
            .iter()
            .flat_map(|s| {
                ((s * gain * 32768.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes()
            })
            .collect();
        PcmChunk::new(Bytes::from(data), AudioFormat::default())
    }

    fn estimate(chunks: &[PcmChunk]) -> Option<f32> {
        let mut snr = SnrEstimator::new(RATE, 1);
        for c in chunks {
            snr.observe(c);
        }
        snr.snr_db()
    }

    #[test]
    fn the_statistic_tracks_the_true_snr_of_speech_in_noise() {
        for (snr, seed) in [(0.0, 1), (10.0, 2), (20.0, 3), (30.0, 4)] {
            let mut wada = Wada::default();
            for s in mixture(4.0, snr, seed) {
                wada.add(s as f32);
            }
            let got = wada.snr_db().unwrap();
            assert!(
                (f64::from(got) - snr).abs() < 1.5,
                "true {snr} dB, estimated {got}"
            );
        }
        assert_eq!(Wada::default().snr_db(), None);
    }

    #[test]
    fn noise_lowers_the_estimate() {
        let estimates: Vec<f32> = [30.0, 15.0, 5.0]
            .iter()
            .map(|&snr| estimate(&[chunk(&mixture(4.0, snr, 10), 0.05)]).unwrap())
            .collect();
        assert!(
            estimates.windows(2).all(|w| w[0] > w[1] + 3.0),
            "{estimates:?}"
        );
    }

    #[test]
    fn gain_does_not_move_the_estimate() {
        let signal = mixture(4.0, 10.0, 4);
        let quiet = estimate(&[chunk(&signal, 0.01)]).unwrap();
        let hot = estimate(&[chunk(&signal, 0.2)]).unwrap();
        assert!((quiet - hot).abs() < 1.0, "quiet {quiet}, hot {hot}");
    }

    #[test]
    fn noise_alone_reads_as_the_floor_of_the_table() {
        let mut rng = Rng(5);
        let noise: Vec<f64> = (0..RATE * 2).map(|_| rng.gauss() * 0.01).collect();
        assert!(estimate(&[chunk(&noise, 1.0)]).unwrap() < -5.0);
    }

    /// Brown noise, a fan's or a road's spectrum, as loud as the speech,
    /// costs well under half what white noise as loud costs.
    #[test]
    fn rumble_below_the_speech_band_is_not_noise() {
        let speech = mixture(4.0, 60.0, 6);
        let mut rng = Rng(9);
        let mut level = 0.0;
        let brown: Vec<f64> = speech
            .iter()
            .map(|_| {
                level = 0.995 * level + rng.gauss();
                level
            })
            .collect();
        let white: Vec<f64> = speech.iter().map(|_| rng.gauss()).collect();
        let at_unit_power = |noise: &[f64]| {
            let rms = (noise.iter().map(|b| b * b).sum::<f64>() / noise.len() as f64).sqrt();
            let mixed: Vec<f64> = speech.iter().zip(noise).map(|(s, b)| s + b / rms).collect();
            estimate(&[chunk(&mixed, 0.05)]).unwrap()
        };
        let clean = estimate(&[chunk(&speech, 0.05)]).unwrap();
        let (rumble, hiss) = (at_unit_power(&brown), at_unit_power(&white));
        assert!(
            clean - rumble < (clean - hiss) / 2.0,
            "clean {clean}, rumble {rumble}, hiss {hiss}"
        );
    }

    #[test]
    fn digital_silence_is_left_out() {
        let signal = mixture(2.0, 10.0, 7);
        let alone = estimate(&[chunk(&signal, 0.05)]).unwrap();
        let after_a_switch =
            estimate(&[chunk(&vec![0.0; RATE as usize], 1.0), chunk(&signal, 0.05)]).unwrap();
        assert!(
            (alone - after_a_switch).abs() < 0.5,
            "{alone} vs {after_a_switch}"
        );
    }

    /// One sound in a 20 ms frame keeps the whole frame: silence is skipped
    /// a frame at a time, not sample by sample.
    #[test]
    fn a_frame_with_any_sound_counts_whole() {
        let sparse: Vec<f64> = (0..RATE as usize / 2)
            .map(|n| if n % 320 == 0 { 0.1 } else { 0.0 })
            .collect();
        assert!(estimate(&[chunk(&sparse, 1.0)]).is_some());
    }

    #[test]
    fn channels_are_filtered_apart() {
        let mono = mixture(2.0, 10.0, 11);
        let stereo: Vec<f64> = mono.iter().flat_map(|&s| [s, s]).collect();
        let format = AudioFormat {
            channels: 2,
            ..AudioFormat::default()
        };
        let data = chunk(&stereo, 0.05).data;
        let mut snr = SnrEstimator::new(RATE, 2);
        snr.observe(&PcmChunk::new(data, format));
        let both = snr.snr_db().unwrap();
        let one = estimate(&[chunk(&mono, 0.05)]).unwrap();
        assert!((both - one).abs() < 0.01, "stereo {both}, mono {one}");
    }

    #[test]
    fn nothing_is_claimed_before_half_a_second_of_audio() {
        let signal = mixture(0.4, 10.0, 8);
        assert_eq!(estimate(&[chunk(&signal, 0.05)]), None);
        assert_eq!(estimate(&[chunk(&vec![0.0; RATE as usize * 3], 1.0)]), None);
        assert!(estimate(&[chunk(&mixture(0.6, 10.0, 8), 0.05)]).is_some());
    }

    #[test]
    fn the_table_is_clamped_and_interpolated() {
        assert_eq!(snr_for(0.0), -10.0);
        assert_eq!(snr_for(9.0), 60.0);
        assert_eq!(snr_for(WADA_G[20]), 10.0);
        let midway = snr_for((WADA_G[20] + WADA_G[21]) / 2.0);
        assert!((midway - 10.5).abs() < 1e-4, "{midway}");
    }

    #[test]
    fn other_sample_widths_are_ignored() {
        let mut snr = SnrEstimator::new(RATE, 1);
        let format = AudioFormat {
            sample_width_bytes: 4,
            ..AudioFormat::default()
        };
        snr.observe(&PcmChunk::new(Bytes::from(vec![1u8; 64_000]), format));
        assert_eq!(snr.snr_db(), None);
    }
}
