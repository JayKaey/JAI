//! What tells one voice from another beyond the long-term spectrum.
//!
//! The reference transfer (`fish_zero_shot_tts::SpectralTransfer`) equalises a voice's
//! long-term envelope to its clip, so the envelope's similarity says little about whose voice
//! it is: Ultron's scored 0.99 against his clip while Sir heard "Jarvis, lower". These are the
//! measurable things an ear goes by instead: where the pitch sits and how far it moves, whether
//! a second voice runs an octave below it, how dark the sound is, how rough, and how fast it
//! speaks. `Features::measure` works on any speech (a clip or a synthesised sentence);
//! `apart` is the distance between two voices the verifier asks for.

use crate::dsp::Biquad;
use crate::fish_zero_shot_tts::{f0_stats, fft, pitch_track};

/// The rate the measurements run at, the spectral frame and the hop.
const RATE: u32 = 16_000;
const FRAME: usize = 1024;
const HOP: usize = 320;
/// Frames this far under the loudest one are pauses.
const PAUSE_DB: f32 = 35.0;
const FLOOR_DB: f32 = -50.0;
/// The period search: from 320 Hz down to 35 Hz (a second voice an octave under an 88 Hz one
/// runs at 44 Hz), over a window of 32 ms.
const PERIOD_WINDOW: usize = 512;
const SHORTEST: usize = RATE as usize / 320;
const LONGEST: usize = RATE as usize / 35;
/// A dip of the normalised difference under this is a period (forgiving on purpose: a layered
/// voice is less periodic than a clean one).
const PERIOD_DIP: f32 = 0.45;
/// From this share of the sound repeating only every second period on, a frame counts as
/// doubled.
const DOUBLED_SHARE: f32 = 0.05;

/// A voice as measured on a stretch of speech.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Features {
    /// Median F0 (Hz) and the width of the intonation (standard deviation, semitones).
    pub f0_hz: f32,
    pub f0_width_st: f32,
    /// The share of the speech frames with a clear pitch (a layered or rough voice has fewer).
    pub voiced: f32,
    /// The share of the periodic sound that repeats only every second pitch period (a second
    /// voice an octave below, vocal fry), in dB against the whole: under -30 for a clean voice,
    /// around -15 for a clearly layered one.
    pub subharmonic_db: f32,
    /// The share of the periodic frames in which that is plainly so.
    pub doubled: f32,
    /// The spectral centre of gravity of the speech between 60 Hz and 8 kHz.
    pub centroid_hz: f32,
    /// The share of the energy under 250 Hz and above 3 kHz, in dB against the whole.
    pub low_db: f32,
    pub high_db: f32,
    /// Roughness: how deep the 500 Hz - 4 kHz band is modulated at 25-70 Hz (0 = smooth).
    pub roughness: f32,
    /// Syllables per second of speech, and the share of the time between the first and the
    /// last word that is pause.
    pub syllable_rate: f32,
    pub pause_share: f32,
}

fn db(power: f32) -> f32 {
    10.0 * power.max(1e-20).log10()
}

/// One 20 ms of speech: its period as the forgiving search finds it (Hz), and, where two of
/// those periods fit the search, the share of the sound that repeats only every second one.
struct Period {
    f0: f32,
    odd_share: Option<f32>,
}

/// The periods of `x` (16 kHz), one per `HOP` of speech (frames quieter than `gate` are left
/// out). With the period T: a voice alone is the same one period on; a second voice an octave
/// below is the same only two periods on, and what it adds to the difference at T is four
/// times the energy of its half-harmonics. Both lags are fitted, so a moving pitch is not
/// taken for a second voice.
fn periods(x: &[f32], gate: f32) -> Vec<Period> {
    let mut out = Vec::new();
    let mut d = vec![0.0f32; LONGEST + 2];
    let mut start = 0;
    while start + PERIOD_WINDOW + LONGEST + 2 <= x.len() {
        let w = &x[start..start + PERIOD_WINDOW + LONGEST + 2];
        start += HOP;
        if crate::dsp::rms(&w[..PERIOD_WINDOW]) < gate {
            continue;
        }
        for (tau, slot) in d.iter_mut().enumerate().skip(1) {
            *slot = (0..PERIOD_WINDOW).map(|j| (w[j] - w[j + tau]).powi(2)).sum();
        }
        // Cumulative-mean normalised (YIN): the first dip from the short periods up.
        let mut running = 0.0f32;
        let mut first = None;
        let mut cmnd = vec![1.0f32; LONGEST + 2];
        for tau in 1..LONGEST + 2 {
            running += d[tau];
            cmnd[tau] = d[tau] * tau as f32 / running.max(1e-12);
        }
        let mut tau = SHORTEST;
        while tau <= LONGEST {
            if cmnd[tau] < PERIOD_DIP {
                while tau < LONGEST && cmnd[tau + 1] < cmnd[tau] {
                    tau += 1;
                }
                first = Some(tau);
                break;
            }
            tau += 1;
        }
        let Some(period) = first else {
            out.push(Period { f0: 0.0, odd_share: None });
            continue;
        };
        // The difference at a lag against the energy it compares: 0 = the same, 1 = unrelated.
        let own: f32 = w[..PERIOD_WINDOW].iter().map(|s| s * s).sum();
        let unlike = |lag: usize| d[lag] / (own + w[lag..lag + PERIOD_WINDOW].iter().map(|s| s * s).sum::<f32>()).max(1e-12);
        let fitted = |lag: usize, slack: usize| (lag.saturating_sub(slack).max(1)..=(lag + slack).min(LONGEST + 1)).map(unlike).fold(f32::MAX, f32::min);
        let odd_share = (2 * period + 3 <= LONGEST + 1).then(|| ((fitted(period, 1) - fitted(2 * period, 3)) / 2.0).max(0.0));
        out.push(Period { f0: RATE as f32 / period as f32, odd_share });
    }
    out
}

impl Features {
    /// Measures `samples` (mono, any rate). None with too little speech or no pitch at all.
    pub fn measure(samples: &[f32], rate: u32) -> Option<Self> {
        let x = if rate == RATE { samples.to_vec() } else { crate::dsp::resample(samples, rate, RATE) };
        if x.len() < FRAME * 4 {
            return None;
        }
        let starts: Vec<usize> = (0..=(x.len() - FRAME) / HOP).map(|f| f * HOP).collect();
        let level: Vec<f32> = starts.iter().map(|s| db(crate::dsp::rms(&x[*s..*s + FRAME]).powi(2))).collect();
        let loudest = level.iter().copied().fold(f32::MIN, f32::max);
        let speech = |l: f32| l > FLOOR_DB && l > loudest - PAUSE_DB;
        let window: Vec<f32> = (0..FRAME).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FRAME as f32).cos()).collect();
        let bin_hz = RATE as f32 / FRAME as f32;
        let mut long_term = vec![0.0f64; FRAME / 2 + 1];
        let (mut re, mut im) = (vec![0.0f32; FRAME], vec![0.0f32; FRAME]);
        let mut frames = 0usize;
        for (start, l) in starts.iter().zip(&level) {
            if !speech(*l) {
                continue;
            }
            for i in 0..FRAME {
                re[i] = x[start + i] * window[i];
                im[i] = 0.0;
            }
            fft(&mut re, &mut im, false);
            for (k, a) in long_term.iter_mut().enumerate() {
                *a += (re[k] * re[k] + im[k] * im[k]) as f64;
            }
            frames += 1;
        }
        if frames < 8 {
            return None;
        }
        let found = periods(&x, 10f32.powf((loudest - PAUSE_DB).max(FLOOR_DB) / 20.0));
        let periodic: Vec<f32> = found.iter().map(|p| p.f0).filter(|f| *f > 0.0).collect();
        // The pitch: the strict tracker's where it hears one (a clean voice), else what the
        // forgiving search found.
        let (f0_hz, f0_width_st, voiced) = match f0_stats(&pitch_track(samples, rate)) {
            Some(stats) => (stats.median_hz, stats.std_st, stats.voiced_ratio),
            None => {
                if periodic.len() < 5 {
                    return None;
                }
                let mut sorted = periodic.clone();
                sorted.sort_by(f32::total_cmp);
                let median = sorted[sorted.len() / 2];
                let st: Vec<f32> = periodic.iter().map(|f| 12.0 * (f / median).log2()).collect();
                let mean = st.iter().sum::<f32>() / st.len() as f32;
                (median, (st.iter().map(|s| (s - mean).powi(2)).sum::<f32>() / st.len() as f32).sqrt(), periodic.len() as f32 / found.len().max(1) as f32)
            }
        };
        let odd: Vec<f32> = found.iter().filter_map(|p| p.odd_share).collect();
        let band = |lo: f32, hi: f32| -> f64 { long_term.iter().enumerate().filter(|(k, _)| (*k as f32 * bin_hz) >= lo && (*k as f32 * bin_hz) < hi).map(|(_, p)| *p).sum() };
        let whole = band(60.0, 8000.0).max(1e-30);
        let centroid = long_term.iter().enumerate().filter(|(k, _)| (60.0..8000.0).contains(&(*k as f32 * bin_hz))).map(|(k, p)| k as f64 * bin_hz as f64 * p).sum::<f64>() / whole;
        let (syllable_rate, pause_share) = rhythm(&x);
        Some(Self {
            f0_hz,
            f0_width_st,
            voiced,
            subharmonic_db: if odd.is_empty() { -40.0 } else { db(odd.iter().sum::<f32>() / odd.len() as f32).max(-40.0) },
            doubled: if odd.is_empty() { 0.0 } else { odd.iter().filter(|share| **share > DOUBLED_SHARE).count() as f32 / odd.len() as f32 },
            centroid_hz: centroid as f32,
            low_db: db((band(60.0, 250.0) / whole) as f32),
            high_db: db((band(3000.0, 8000.0) / whole) as f32),
            roughness: roughness(&x),
            syllable_rate,
            pause_share,
        })
    }

    pub fn describe(&self) -> String {
        format!(
            "F0 {:.1} Hz (width {:.2} st, {:.0} % clear), half-harmonics {:+.1} dB ({:.0} % of frames doubled), centre {:.0} Hz (low {:+.1} dB, high {:+.1} dB), roughness {:.3}, {:.2} syllables/s, {:.0} % pause",
            self.f0_hz,
            self.f0_width_st,
            self.voiced * 100.0,
            self.subharmonic_db,
            self.doubled * 100.0,
            self.centroid_hz,
            self.low_db,
            self.high_db,
            self.roughness,
            self.syllable_rate,
            self.pause_share * 100.0
        )
    }
}

/// How deep the 500 Hz - 4 kHz band of `x` (16 kHz) is modulated at 25-70 Hz: under the pitch
/// of either voice, where a second voice an octave down and vocal fry beat. 0 = smooth; a tone
/// modulated fully at 40 Hz gives about 1.
fn roughness(x: &[f32]) -> f32 {
    let r = RATE as f32;
    let (mut hp, mut lp, mut smooth) = (Biquad::high_pass(500.0, r), Biquad::low_pass(4000.0, r), Biquad::low_pass(150.0, r));
    // The band's envelope at 1 kHz.
    let envelope: Vec<f32> = x.iter().map(|s| smooth.process(lp.process(hp.process(*s)).abs())).step_by(16).collect();
    const N: usize = 512;
    if envelope.len() < N {
        return 0.0;
    }
    let window: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N as f32).cos()).collect();
    let loudest = envelope.iter().copied().fold(0.0f32, f32::max).max(1e-9);
    let (mut re, mut im) = (vec![0.0f32; N], vec![0.0f32; N]);
    let (mut sum, mut count) = (0.0f32, 0usize);
    let bin_hz = 1000.0 / N as f32;
    for start in (0..=envelope.len() - N).step_by(N / 2) {
        let frame = &envelope[start..start + N];
        let mean = frame.iter().sum::<f32>() / N as f32;
        // Stretches of speech only.
        if mean < loudest * 0.08 {
            continue;
        }
        for i in 0..N {
            re[i] = frame[i] * window[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im, false);
        let steady = (re[0] * re[0] + im[0] * im[0]).sqrt().max(1e-9);
        let beat: f32 = (0..N / 2).filter(|k| (25.0..=70.0).contains(&(*k as f32 * bin_hz))).map(|k| re[k] * re[k] + im[k] * im[k]).sum::<f32>().sqrt();
        // A full sinusoidal modulation puts half its depth on either side of the carrier; the
        // Hann window spreads it over one and a half bins.
        sum += 2.0 * beat / steady / 1.5f32.sqrt();
        count += 1;
    }
    if count == 0 { 0.0 } else { sum / count as f32 }
}

/// Syllables per second of speech and the share of pauses, from the loudness of `x` (16 kHz)
/// every 10 ms: a syllable is a peak that stands 4 dB above the dip before it.
fn rhythm(x: &[f32]) -> (f32, f32) {
    let step = RATE as usize / 100;
    let raw: Vec<f32> = x.chunks(step).map(|c| db(crate::dsp::rms(c).powi(2))).collect();
    if raw.len() < 20 {
        return (0.0, 0.0);
    }
    // Smoothed over 50 ms.
    let level: Vec<f32> = (0..raw.len())
        .map(|i| {
            let near = &raw[i.saturating_sub(2)..(i + 3).min(raw.len())];
            near.iter().sum::<f32>() / near.len() as f32
        })
        .collect();
    let loudest = level.iter().copied().fold(f32::MIN, f32::max);
    let speech = |l: f32| l > FLOOR_DB && l > loudest - PAUSE_DB;
    let (Some(first), Some(last)) = (level.iter().position(|l| speech(*l)), level.iter().rposition(|l| speech(*l))) else { return (0.0, 0.0) };
    let spoken = level[first..=last].iter().filter(|l| speech(**l)).count();
    let pause_share = 1.0 - spoken as f32 / (last - first + 1) as f32;
    let (mut syllables, mut dip, mut rising) = (0usize, level[first], false);
    for i in first + 1..=last {
        if level[i] > level[i - 1] {
            rising = true;
        } else if level[i] < level[i - 1] {
            // A peak just passed: it counts when it stood clear of the dip before it.
            if rising && speech(level[i - 1]) && level[i - 1] - dip >= 4.0 {
                syllables += 1;
                dip = level[i];
            }
            rising = false;
            dip = dip.min(level[i]);
        }
    }
    (syllables as f32 / (spoken as f32 / 100.0).max(0.1), pause_share)
}

/// How far apart two voices are on the things an ear goes by, each difference in units of what
/// is clearly audible: 2 semitones of pitch, 6 dB of half-harmonics, 15 % of the spectral
/// centre, 0.08 of roughness, 15 % of the speaking rate. Around 1 two voices are told apart
/// with attention; from 3 nobody confuses them.
pub fn apart(a: &Features, b: &Features) -> f32 {
    let pitch = (12.0 * (a.f0_hz / b.f0_hz.max(1.0)).log2()).abs() / 2.0;
    let layer = (a.subharmonic_db - b.subharmonic_db).abs() / 6.0;
    let colour = (a.centroid_hz / b.centroid_hz.max(1.0)).ln().abs() / 0.15f32.ln_1p();
    let rough = (a.roughness - b.roughness).abs() / 0.08;
    let pace = (a.syllable_rate / b.syllable_rate.max(0.1)).ln().abs() / 0.15f32.ln_1p();
    pitch + layer + colour + rough + pace
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const R: u32 = 48_000;

    /// "Speech": syllables of a pulse train at `f0` (`syllable_secs` each, a quarter of it
    /// gap), through a formant; `octave` mixes in the same train an octave below.
    fn voice(f0: f32, octave: f32, syllable_secs: f32, secs: f32) -> Vec<f32> {
        let mut formant = crate::jarvis_voice_dsp::Bq::peaking(900.0, 4.0, 18.0, R as f32);
        let train = |turns: f32| (0..20).map(|h| (TAU * turns.fract() * (h + 1) as f32).sin() / (h + 1) as f32).sum::<f32>();
        (0..(secs * R as f32) as usize)
            .map(|i| {
                let t = i as f32 / R as f32;
                let inside = (t / syllable_secs).fract();
                let gate = if inside < 0.75 { (std::f32::consts::PI * inside / 0.75).sin().sqrt() } else { 0.0 };
                formant.process((train(t * f0) + octave * train(t * f0 / 2.0)) * 0.08) * gate
            })
            .collect()
    }

    #[test]
    fn pitch_layer_colour_and_pace_are_measured() {
        let plain = Features::measure(&voice(120.0, 0.0, 0.25, 3.0), R).expect("speech");
        assert!((plain.f0_hz - 120.0).abs() < 3.0, "{}", plain.describe());
        assert!(plain.subharmonic_db < -25.0 && plain.doubled < 0.05, "a clean voice has no half-harmonics: {}", plain.describe());
        // Four syllables a second of time, a quarter of it pause: 5.3 a second of speech.
        assert!((plain.syllable_rate * (1.0 - plain.pause_share) - 4.0).abs() < 0.6, "{}", plain.describe());
        assert!((plain.pause_share - 0.25).abs() < 0.12, "{}", plain.describe());
        // A second voice an octave below: the half-harmonics come up by well over 6 dB, a good
        // part of the frames is doubled, the band beats at the lower voice's rate.
        let layered = Features::measure(&voice(120.0, 0.35, 0.25, 3.0), R).expect("speech");
        assert!(layered.subharmonic_db > plain.subharmonic_db + 8.0 && layered.doubled > 0.3, "{} / {}", layered.describe(), plain.describe());
        assert!(layered.roughness > plain.roughness + 0.03, "{} / {}", layered.describe(), plain.describe());
        // A lower, slower voice; a voice whose pitch glides is still one voice.
        let deep = Features::measure(&voice(88.0, 0.0, 0.32, 3.0), R).expect("speech");
        assert!((deep.f0_hz - 88.0).abs() < 3.0 && deep.syllable_rate < plain.syllable_rate - 0.5, "{}", deep.describe());
        let gliding: Vec<f32> = {
            let mut phase = 0.0f32;
            (0..3 * R as usize)
                .map(|i| {
                    let f = 110.0 * 2f32.powf(3.0 * (TAU * 2.0 * i as f32 / R as f32).sin() / 12.0);
                    phase = (phase + f / R as f32).fract();
                    (0..12).map(|h| (TAU * phase * (h + 1) as f32).sin() / (h + 1) as f32).sum::<f32>() * 0.1
                })
                .collect()
        };
        let glide = Features::measure(&gliding, R).expect("speech");
        assert!(glide.subharmonic_db < -20.0 && glide.doubled < 0.1 && glide.f0_width_st > 1.5, "{}", glide.describe());
        // The same voice is no distance from itself; a deeper, layered, slower one is far off.
        assert!(apart(&plain, &plain) < 1e-3);
        let other = Features::measure(&voice(88.0, 0.35, 0.32, 3.0), R).expect("speech");
        assert!(apart(&plain, &other) > 4.0, "{:.2}: {} / {}", apart(&plain, &other), plain.describe(), other.describe());
        assert!((apart(&plain, &other) - apart(&other, &plain)).abs() < 1e-3);
        // Silence is nobody's voice.
        assert!(Features::measure(&vec![0.0; R as usize], R).is_none());
    }
}
