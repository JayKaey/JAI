//! Small DSP building blocks shared by capture (mic → 16 kHz), STT pre-processing and TTS output.
//! Everything here is allocation-free on the hot path and unit-tested.

use std::f32::consts::{FRAC_1_SQRT_2, PI};

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
}

pub fn to_dbfs(level: f32) -> f32 {
    20.0 * level.max(1e-6).log10()
}

/// Streaming sample-rate converter.
///
/// Downsampling runs a Blackman-windowed sinc low-pass (cut-off just below the target Nyquist)
/// before fractional linear interpolation, so 44.1/48 kHz microphones don't alias into the
/// 16 kHz VAD/Whisper band. Upsampling (24 kHz TTS → 48 kHz device) uses interpolation only.
/// State persists across calls, so arbitrary chunk sizes can be fed.
pub struct StreamResampler {
    step: f64,
    taps: Vec<f32>,
    /// Double-length history so the last `taps.len()` samples are always one contiguous slice.
    history: Vec<f32>,
    history_pos: usize,
    prev: f32,
    curr: f32,
    /// Position of the next output sample, relative to `prev`, in input-sample units.
    frac: f64,
}

impl StreamResampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let in_rate = in_rate.max(1);
        let out_rate = out_rate.max(1);
        let ratio = in_rate as f64 / out_rate as f64;
        let taps = if ratio > 1.0 + 1e-9 {
            let n = (24 * ratio.ceil() as usize + 1).clamp(33, 129);
            // Normalised cut-off (cycles per input sample): 85 % of the output Nyquist.
            design_lowpass(n, 0.5 / ratio as f32 * 0.85)
        } else {
            Vec::new()
        };
        let len = taps.len();
        Self {
            step: ratio,
            taps,
            history: vec![0.0; len * 2],
            history_pos: 0,
            prev: 0.0,
            curr: 0.0,
            frac: 1.0,
        }
    }

    pub fn is_passthrough(&self) -> bool {
        (self.step - 1.0).abs() < 1e-9
    }

    pub fn reset(&mut self) {
        self.history.iter_mut().for_each(|s| *s = 0.0);
        self.history_pos = 0;
        self.prev = 0.0;
        self.curr = 0.0;
        self.frac = 1.0;
    }

    /// Appends the resampled version of `input` to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.is_passthrough() {
            out.extend_from_slice(input);
            return;
        }
        out.reserve((input.len() as f64 / self.step) as usize + 2);
        for &x in input {
            let filtered = self.filter(x);
            self.prev = self.curr;
            self.curr = filtered;
            while self.frac < 1.0 {
                out.push(self.prev + (self.curr - self.prev) * self.frac as f32);
                self.frac += self.step;
            }
            self.frac -= 1.0;
        }
    }

    #[inline]
    fn filter(&mut self, x: f32) -> f32 {
        let n = self.taps.len();
        if n == 0 {
            return x;
        }
        self.history[self.history_pos] = x;
        self.history[self.history_pos + n] = x;
        self.history_pos = (self.history_pos + 1) % n;
        // history[pos .. pos + n] holds the last n samples, oldest first.
        let window = &self.history[self.history_pos..self.history_pos + n];
        window
            .iter()
            .zip(self.taps.iter().rev())
            .map(|(s, t)| s * t)
            .sum()
    }
}

/// One-shot conversion helper.
pub fn resample(input: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    let mut rs = StreamResampler::new(in_rate, out_rate);
    let mut out = Vec::with_capacity(input.len() * out_rate as usize / in_rate.max(1) as usize + 8);
    rs.process(input, &mut out);
    // Flush the filter/interpolator delay with a little silence.
    let tail = vec![0.0f32; rs.taps.len() / 2 + 2];
    rs.process(&tail, &mut out);
    let expected = (input.len() as f64 * out_rate as f64 / in_rate.max(1) as f64).round() as usize;
    let delay = (rs.taps.len() / 2) as f64 / rs.step;
    let skip = (delay.round() as usize).min(out.len());
    out.drain(..skip);
    out.truncate(expected);
    out
}

fn design_lowpass(n: usize, cutoff: f32) -> Vec<f32> {
    let m = (n - 1) as f32 / 2.0;
    let mut taps: Vec<f32> = (0..n)
        .map(|i| {
            let x = i as f32 - m;
            let sinc = if x.abs() < 1e-6 {
                2.0 * cutoff
            } else {
                (2.0 * PI * cutoff * x).sin() / (PI * x)
            };
            let w = 0.42 - 0.5 * (2.0 * PI * i as f32 / (n - 1) as f32).cos()
                + 0.08 * (4.0 * PI * i as f32 / (n - 1) as f32).cos();
            sinc * w
        })
        .collect();
    let sum: f32 = taps.iter().sum();
    if sum.abs() > 1e-9 {
        taps.iter_mut().for_each(|t| *t /= sum);
    }
    taps
}

/// RBJ audio-EQ-cookbook biquad (transposed direct form II).
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn from_coeffs(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    pub fn high_pass(freq: f32, sample_rate: f32) -> Self {
        let w0 = 2.0 * PI * freq / sample_rate;
        let alpha = w0.sin() / (2.0 * FRAC_1_SQRT_2);
        let cos = w0.cos();
        Self::from_coeffs(
            (1.0 + cos) / 2.0,
            -(1.0 + cos),
            (1.0 + cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }

    /// Second-order Butterworth low-pass (RBJ cookbook, Q = 1/√2).
    pub fn low_pass(freq: f32, sample_rate: f32) -> Self {
        let w0 = 2.0 * PI * freq / sample_rate;
        let alpha = w0.sin() / (2.0 * FRAC_1_SQRT_2);
        let cos = w0.cos();
        Self::from_coeffs((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Soft limiter: transparent below 0.8, smoothly saturating towards ±1.0 above.
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.8;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        let over = (a - KNEE) / (1.0 - KNEE);
        x.signum() * (KNEE + (1.0 - KNEE) * over.tanh())
    }
}

/// Peak 0.85 (-1.4 dBFS): the level quiet or mumbled speech is lifted to before Whisper.
pub const ASR_PEAK_TARGET: f32 = 0.85;
/// +30 dB: the most a whispered utterance is boosted.
const ASR_MAX_GAIN: f32 = 31.6;

/// Peak-safe auto-gain for speech recognition: one linear gain that brings the robust peak
/// (99.9th percentile, so a single click cannot hold quiet speech down) to `ASR_PEAK_TARGET`.
/// Linear means the frequency balance and the vowel formants stay exactly as spoken; only the
/// rare samples above the target (the clicks the percentile ignored) are rounded off by a soft
/// knee that never reaches full scale. Returns the gain applied.
pub fn normalize_for_asr(audio: &mut [f32]) -> f32 {
    if audio.is_empty() {
        return 1.0;
    }
    let mut magnitudes: Vec<f32> = audio.iter().map(|s| s.abs()).collect();
    let k = ((magnitudes.len() as f32 * 0.999) as usize).min(magnitudes.len() - 1);
    let robust_peak = *magnitudes.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1;
    if robust_peak < 1e-5 {
        return 1.0;
    }
    let gain = (ASR_PEAK_TARGET / robust_peak).clamp(0.5, ASR_MAX_GAIN);
    for s in audio.iter_mut() {
        *s = peak_safe(*s * gain);
    }
    gain
}

/// Transparent up to `ASR_PEAK_TARGET`; above it a tanh knee into the last 0.15 (never 1.0).
fn peak_safe(x: f32) -> f32 {
    let a = x.abs();
    if a <= ASR_PEAK_TARGET {
        x
    } else {
        let head = 1.0 - ASR_PEAK_TARGET;
        x.signum() * (ASR_PEAK_TARGET + head * 0.98 * ((a - ASR_PEAK_TARGET) / head).tanh())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asr_gain_keeps_the_vowel_formants_against_the_consonants() {
        // A 400 Hz "o" formant and a 4 kHz fricative at -30 dBFS: after the gain stage the
        // vowel-to-consonant balance is exactly what was spoken (no pre-emphasis tilt).
        let vowel = sine(400.0, 16_000, 0.5, 0.02);
        let fric = sine(4000.0, 16_000, 0.5, 0.01);
        let mut mix: Vec<f32> = vowel.iter().zip(&fric).map(|(a, b)| a + b).collect();
        let gain = normalize_for_asr(&mut mix);
        assert!(gain > 20.0);
        let lifted_vowel: Vec<f32> = vowel.iter().map(|v| v * gain).collect();
        let lifted_fric: Vec<f32> = fric.iter().map(|v| v * gain).collect();
        let rebuilt: Vec<f32> = lifted_vowel.iter().zip(&lifted_fric).map(|(a, b)| a + b).collect();
        let error = mix.iter().zip(&rebuilt).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(error < 0.02, "linear up to the target: {error}");
        assert!(peak(&mix) <= ASR_PEAK_TARGET + 0.01 && peak(&mix) > 0.8, "peak {}", peak(&mix));
    }

    fn sine(freq: f32, rate: u32, secs: f32, amp: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| amp * (2.0 * PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    fn zero_crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0)).count()
    }

    #[test]
    fn downsample_48k_to_16k_preserves_speech_band() {
        let input = sine(440.0, 48_000, 1.0, 0.5);
        let out = resample(&input, 48_000, 16_000);
        assert!((out.len() as i64 - 16_000).abs() <= 2, "len {}", out.len());
        let body = &out[800..15_200];
        let level = rms(body);
        assert!((level - 0.5 * FRAC_1_SQRT_2).abs() < 0.02, "rms {level}");
        let zc = zero_crossings(body) as f32 / (body.len() as f32 / 16_000.0);
        assert!((zc / 2.0 - 440.0).abs() < 5.0, "freq {}", zc / 2.0);
    }

    #[test]
    fn downsample_rejects_content_above_target_nyquist() {
        // 12 kHz would alias to 4 kHz without the anti-aliasing filter.
        let input = sine(12_000.0, 48_000, 0.5, 0.5);
        let out = resample(&input, 48_000, 16_000);
        assert!(rms(&out[400..out.len() - 400]) < 0.01);
    }

    #[test]
    fn streaming_matches_chunking() {
        let input = sine(1000.0, 44_100, 0.3, 0.4);
        let mut a = StreamResampler::new(44_100, 16_000);
        let mut whole = Vec::new();
        a.process(&input, &mut whole);
        let mut b = StreamResampler::new(44_100, 16_000);
        let mut chunked = Vec::new();
        for chunk in input.chunks(441) {
            b.process(chunk, &mut chunked);
        }
        assert_eq!(whole.len(), chunked.len());
        assert!(whole.iter().zip(&chunked).all(|(x, y)| (x - y).abs() < 1e-6));
    }

    #[test]
    fn upsample_24k_to_48k_doubles_length() {
        let input = sine(300.0, 24_000, 0.5, 0.3);
        let out = resample(&input, 24_000, 48_000);
        assert!((out.len() as i64 - 24_000).abs() <= 2);
        assert!((rms(&out[1000..23_000]) - 0.3 * FRAC_1_SQRT_2).abs() < 0.02);
    }

    #[test]
    fn high_pass_removes_dc() {
        let mut hp = Biquad::high_pass(80.0, 16_000.0);
        let x: Vec<f32> = (0..16_000).map(|_| hp.process(0.5)).collect();
        assert!(x[8_000..].iter().all(|s| s.abs() < 0.01));
    }

    #[test]
    fn soft_clip_is_bounded_and_transparent() {
        assert_eq!(soft_clip(0.3), 0.3);
        assert!(soft_clip(5.0) <= 1.0 && soft_clip(-5.0) >= -1.0);
    }

    #[test]
    fn asr_normalisation_lifts_mumbles_and_never_clips() {
        // A mumble at -40 dBFS peak with one loud click.
        let mut quiet = sine(220.0, 16_000, 1.0, 0.01);
        quiet[100] = 0.9;
        let gain = normalize_for_asr(&mut quiet);
        assert!(gain > 20.0, "gain {gain}");
        assert!(peak(&quiet) < 1.0, "the click is rounded off below full scale");
        // +30 dB at most (a whisper is lifted, room noise is not blown up).
        assert!(to_dbfs(rms(&quiet)) > -15.0, "rms {:.1} dBFS", to_dbfs(rms(&quiet)));
        // Normal speech level is brought to a 0.85 peak, not over it.
        let mut loud = sine(220.0, 16_000, 1.0, 0.95);
        normalize_for_asr(&mut loud);
        assert!((peak(&loud) - ASR_PEAK_TARGET).abs() < 0.01, "{}", peak(&loud));
        let mut silence = vec![0.0f32; 1000];
        assert_eq!(normalize_for_asr(&mut silence), 1.0);
    }
}
