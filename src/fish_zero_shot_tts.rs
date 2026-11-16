//! Reference transfer for the film voice: everything JAI's Kokoro voice can borrow from the
//! recording of the film voice (`assets/jai_voice_reference.mp3`), offline and in pure Rust.
//!
//! What this is — and is not. Fish Audio's model `c231dcd3116a4c0984e3bced753c1274` clones the
//! voice on Fish Audio's servers with a neural speaker encoder; that model cannot run here (it is
//! a hosted voice, and JAI makes no cloud TTS calls). Kokoro has no speaker encoder either, so
//! this module transfers the *measurable* parts of the recording onto Kokoro's British RP voices:
//!
//! * **A — voice blend calibration** (`calibrate`, `--voice-calibrate`): the clip's sentence is
//!   synthesised with blends of `bm_fable`, `bm_george` and `bm_lewis`; the blend whose mastered
//!   output is closest to the clip (spectral-envelope shape and F0 level / range) is stored in the
//!   profile cache and used by the `bm_jai_movie_1to1` alias from then on.
//! * **B — 128-band spectral envelope transfer** (`SpectralTransfer`): the clip's long-term mel
//!   envelope (128 bands, 60 Hz – 12 kHz, 2048-point Hann STFT at 48 kHz with a 512 hop) is imposed
//!   on every clause: `H[k] = clamp(α · (E_ref[k] − E_syn[k]), ±9 dB)` with α = 0.78, where E_syn
//!   is the chain's own envelope (this clause blended with a running average, so the timbre does
//!   not wander) and both are shape-only (mean-removed). The gains are applied by STFT
//!   overlap-add (√Hann analysis / synthesis, 75 % overlap). The clause's level is then locked to
//!   the clip's speech RMS (never below −21 dBFS).
//! * **C — F0 excursion flattening** (`flatten_f0`): a YIN pitch track and TD-PSOLA compress the
//!   pitch movement around the clause's median by 35 % (the film voice's level, unhurried RP
//!   intonation), keeping duration and timbre; 0.98× tempo and 70 ms micro-pauses live in the
//!   chain and the clause joiner.
//! * **D — closed-loop verification** (`verify`, `--voice-verify`, the `closed_loop_voice_*` test):
//!   synthesises the clip's own sentence, masters it, and reports the spectral-envelope similarity
//!   (Pearson correlation of the mean-removed 128-band log-mel envelopes) and the F0 statistics
//!   against the clip — before and after the transfer, and on a sentence the transfer never saw.
//!
//! The analysis of the clip is cached in `assets/jai_reference_profile.bin` (keyed by the
//! clip's size and modification time).

use crate::jai_voice_dsp::{db_to_gain, gain_to_db, limit};
use std::f32::consts::PI;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex};
use tracing::{info, warn};

/// The rate the envelope is analysed and transferred at (the chain's rate).
pub const ANALYSIS_RATE: u32 = 48_000;
pub const FFT_SIZE: usize = 2048;
pub const HOP: usize = 512;
pub const MEL_BANDS: usize = 128;
const MEL_LOW_HZ: f32 = 60.0;
const MEL_HIGH_HZ: f32 = 12_000.0;
/// Layer B: the transfer strength and its limit per band.
pub const TRANSFER_ALPHA: f32 = 0.78;
pub const MAX_TRANSFER_DB: f32 = 9.0;
/// Layer B: how much of a clause's own envelope enters E_syn (the rest is the running average).
const CLAUSE_WEIGHT: f32 = 0.4;
/// The running average spans about this much speech.
const AVERAGE_SECS: f32 = 20.0;
/// The level lock never targets less than the chain's own speech level.
pub const MIN_TARGET_RMS_DB: f32 = -21.0;
const MAX_LEVEL_DB: f32 = 6.0;
/// Layer C: the default compression of F0 excursions around the clause median.
pub const DEFAULT_F0_FLATTEN: f32 = 0.35;
/// Speech frames: louder than −45 dBFS and within 35 dB of the loudest frame.
const GATE_DB: f32 = -45.0;
const RELATIVE_GATE_DB: f32 = 35.0;
/// Where the clip's own words are written down, beside the clip. They are the film's, like the
/// recording itself, so they are Sir's file and no part of the code.
pub const TRANSCRIPT_PATH: &str = "assets/jai_voice_reference.txt";

/// The clip's own words (the closed-loop test synthesises them); without the file, the
/// held-out sentence stands in.
pub fn reference_transcript() -> String {
    let words = std::fs::read_to_string(TRANSCRIPT_PATH).unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
    if words.is_empty() {
        HELD_OUT_SENTENCE.to_string()
    } else {
        words
    }
}
/// A sentence the transfer never saw (generalisation check).
pub const HELD_OUT_SENTENCE: &str = "Good evening, Sir. The research agent has finished, and the report is ready whenever you are.";
/// Where the clip's analysis is cached.
pub const PROFILE_PATH: &str = "assets/jai_reference_profile.bin";
/// The verification target for the mastered clip sentence (both voices).
pub const SIMILARITY_TARGET: f32 = 0.97;

// --------------------------------------------------------------------------------- FFT ----

/// In-place iterative radix-2 FFT (`re.len()` a power of two; `inverse` without the 1/n scale).
pub fn fft(re: &mut [f32], im: &mut [f32], inverse: bool) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let angle = sign * 2.0 * std::f64::consts::PI / len as f64;
        let (wr, wi) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (xr, xi) = (re[b] as f64 * cr - im[b] as f64 * ci, re[b] as f64 * ci + im[b] as f64 * cr);
                re[b] = (re[a] as f64 - xr) as f32;
                im[b] = (im[a] as f64 - xi) as f32;
                re[a] = (re[a] as f64 + xr) as f32;
                im[a] = (im[a] as f64 + xi) as f32;
                let next = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = next;
            }
        }
        len <<= 1;
    }
}

/// A periodic Hann window.
fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos()).collect()
}

fn hz_to_mel(f: f32) -> f32 {
    2595.0 * (1.0 + f / 700.0).log10()
}

fn mel_to_hz(m: f32) -> f32 {
    700.0 * (10f32.powf(m / 2595.0) - 1.0)
}

/// The centre frequency of each mel band (Hz).
pub fn band_centres() -> [f32; MEL_BANDS] {
    let (lo, hi) = (hz_to_mel(MEL_LOW_HZ), hz_to_mel(MEL_HIGH_HZ));
    std::array::from_fn(|b| mel_to_hz(lo + (hi - lo) * (b as f32 + 1.0) / (MEL_BANDS as f32 + 1.0)))
}

/// Triangular mel filters over the FFT bins (each at least two bins wide, so the low bands are
/// never empty at a 23 Hz bin spacing).
struct MelBank {
    filters: Vec<Vec<(usize, f32)>>,
}

impl MelBank {
    fn new(rate: u32) -> Self {
        let (lo, hi) = (hz_to_mel(MEL_LOW_HZ), hz_to_mel(MEL_HIGH_HZ));
        let edge = |k: f32| mel_to_hz(lo + (hi - lo) * k / (MEL_BANDS as f32 + 1.0));
        let bin_hz = rate as f32 / FFT_SIZE as f32;
        let filters = (0..MEL_BANDS)
            .map(|b| {
                let (left, centre, right) = (edge(b as f32), edge(b as f32 + 1.0), edge(b as f32 + 2.0));
                let half = ((right - left) / 2.0).max(2.0 * bin_hz);
                let (left, right) = (centre - half, centre + half);
                let first = (left / bin_hz).floor().max(1.0) as usize;
                let last = ((right / bin_hz).ceil() as usize).min(FFT_SIZE / 2);
                (first..=last)
                    .filter_map(|k| {
                        let f = k as f32 * bin_hz;
                        let w = if f <= centre { (f - left) / (centre - left) } else { (right - f) / (right - centre) };
                        (w > 0.0).then_some((k, w))
                    })
                    .collect()
            })
            .collect();
        Self { filters }
    }

    fn apply(&self, power: &[f32]) -> [f32; MEL_BANDS] {
        std::array::from_fn(|b| self.filters[b].iter().map(|(k, w)| power[*k] * w).sum::<f32>() / self.filters[b].iter().map(|(_, w)| w).sum::<f32>().max(1e-9))
    }
}

static MEL_48K: LazyLock<MelBank> = LazyLock::new(|| MelBank::new(ANALYSIS_RATE));
static WINDOW: LazyLock<Vec<f32>> = LazyLock::new(|| hann(FFT_SIZE));

/// The 48 kHz version of `samples`.
fn at_48k(samples: &[f32], rate: u32) -> Vec<f32> {
    if rate == ANALYSIS_RATE {
        samples.to_vec()
    } else {
        crate::dsp::resample(samples, rate, ANALYSIS_RATE)
    }
}

/// The long-term 128-band log-mel envelope (dB) of the speech frames, and seconds of speech.
/// None with under 0.25 s of speech.
pub fn mel_envelope(samples: &[f32], rate: u32) -> Option<([f32; MEL_BANDS], f32)> {
    let x = at_48k(samples, rate);
    if x.len() < FFT_SIZE {
        return None;
    }
    let frames: Vec<usize> = (0..=(x.len() - FFT_SIZE) / HOP).map(|f| f * HOP).collect();
    let energy: Vec<f32> = frames.iter().map(|s| gain_to_db(crate::dsp::rms(&x[*s..*s + FFT_SIZE]))).collect();
    let loudest = energy.iter().copied().fold(f32::MIN, f32::max);
    let mut acc = [0.0f64; MEL_BANDS];
    let mut kept = 0usize;
    let (mut re, mut im) = (vec![0.0f32; FFT_SIZE], vec![0.0f32; FFT_SIZE]);
    let mut power = vec![0.0f32; FFT_SIZE / 2 + 1];
    for (start, db) in frames.iter().zip(&energy) {
        if *db < GATE_DB || *db < loudest - RELATIVE_GATE_DB {
            continue;
        }
        for i in 0..FFT_SIZE {
            re[i] = x[start + i] * WINDOW[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im, false);
        for (k, p) in power.iter_mut().enumerate() {
            *p = re[k] * re[k] + im[k] * im[k];
        }
        for (a, m) in acc.iter_mut().zip(MEL_48K.apply(&power)) {
            *a += m as f64;
        }
        kept += 1;
    }
    let secs = kept as f32 * HOP as f32 / ANALYSIS_RATE as f32;
    if secs < 0.25 {
        return None;
    }
    Some((std::array::from_fn(|b| 10.0 * ((acc[b] / kept as f64).max(1e-20) as f32).log10()), secs))
}

/// The envelope's shape: its mean across the bands removed.
fn shape(env: &[f32; MEL_BANDS]) -> [f32; MEL_BANDS] {
    let mean = env.iter().sum::<f32>() / MEL_BANDS as f32;
    env.map(|v| v - mean)
}

/// How alike two envelopes are in shape (timbre balance): the Pearson correlation of the
/// mean-removed log-mel envelopes, 1 = identical shape.
pub fn spectral_similarity(a: &[f32; MEL_BANDS], b: &[f32; MEL_BANDS]) -> f32 {
    let (a, b) = (shape(a), shape(b));
    let dot: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb).max(1e-9)
}

// -------------------------------------------------------------------------------- pitch ----

/// The F0 track: one value per 10 ms hop (0 = unvoiced).
#[derive(Debug, Clone)]
pub struct PitchTrack {
    /// Input samples per value.
    pub hop: usize,
    pub f0: Vec<f32>,
    /// Hops loud enough to be speech (voiced or not).
    pub speech_hops: usize,
}

const YIN_RATE: u32 = 16_000;
const F0_MIN: f32 = 60.0;
const F0_MAX: f32 = 320.0;
const YIN_THRESHOLD: f32 = 0.15;

/// YIN at 16 kHz (40 ms frames, 10 ms hop), reported in hops of the input `rate`.
pub fn pitch_track(samples: &[f32], rate: u32) -> PitchTrack {
    let x = if rate == YIN_RATE { samples.to_vec() } else { crate::dsp::resample(samples, rate, YIN_RATE) };
    let (frame, hop) = (640usize, 160usize);
    let (tau_min, tau_max) = ((YIN_RATE as f32 / F0_MAX) as usize, (YIN_RATE as f32 / F0_MIN) as usize);
    let gate = db_to_gain(GATE_DB);
    let mut f0 = Vec::new();
    let mut start = 0;
    let mut d = vec![0.0f32; tau_max + 2];
    let mut speech_hops = 0usize;
    while start + frame + tau_max + 2 <= x.len() {
        let w = &x[start..start + frame + tau_max + 2];
        if crate::dsp::rms(&w[..frame]) < gate {
            f0.push(0.0);
            start += hop;
            continue;
        }
        speech_hops += 1;
        for (tau, slot) in d.iter_mut().enumerate().take(tau_max + 2).skip(1) {
            *slot = (0..frame).map(|j| (w[j] - w[j + tau]).powi(2)).sum();
        }
        // Cumulative mean normalised difference.
        let mut running = 0.0f32;
        let mut cmnd = vec![1.0f32; tau_max + 2];
        for tau in 1..tau_max + 2 {
            running += d[tau];
            cmnd[tau] = d[tau] * tau as f32 / running.max(1e-12);
        }
        let mut found = None;
        let mut tau = tau_min.max(2);
        while tau <= tau_max {
            if cmnd[tau] < YIN_THRESHOLD {
                while tau < tau_max && cmnd[tau + 1] < cmnd[tau] {
                    tau += 1;
                }
                found = Some(tau);
                break;
            }
            tau += 1;
        }
        let value = match found {
            Some(t) if t > 1 && t <= tau_max => {
                // Parabolic interpolation of the dip.
                let (a, b, c) = (cmnd[t - 1], cmnd[t], cmnd[t + 1]);
                let den = a - 2.0 * b + c;
                let shift = if den.abs() > 1e-9 { 0.5 * (a - c) / den } else { 0.0 };
                YIN_RATE as f32 / (t as f32 + shift.clamp(-0.5, 0.5))
            }
            _ => 0.0,
        };
        f0.push(value);
        start += hop;
    }
    // A 3-point median removes octave blips.
    let raw = f0.clone();
    for i in 1..raw.len().saturating_sub(1) {
        if raw[i - 1] > 0.0 && raw[i] > 0.0 && raw[i + 1] > 0.0 {
            let mut v = [raw[i - 1], raw[i], raw[i + 1]];
            v.sort_by(f32::total_cmp);
            f0[i] = v[1];
        }
    }
    PitchTrack { hop: (rate as usize) / 100, f0, speech_hops }
}

/// F0 statistics of a track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct F0Stats {
    pub mean_hz: f32,
    pub median_hz: f32,
    /// Standard deviation in semitones (the width of the intonation).
    pub std_st: f32,
    /// Share of the speech frames (above the gate) that are voiced.
    pub voiced_ratio: f32,
}

fn semitones(f: f32, reference: f32) -> f32 {
    12.0 * (f / reference).log2()
}

pub fn f0_stats(track: &PitchTrack) -> Option<F0Stats> {
    let voiced: Vec<f32> = track.f0.iter().copied().filter(|f| *f > 0.0).collect();
    if voiced.len() < 5 {
        return None;
    }
    let mut sorted = voiced.clone();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[sorted.len() / 2];
    let mean = voiced.iter().sum::<f32>() / voiced.len() as f32;
    let st: Vec<f32> = voiced.iter().map(|f| semitones(*f, median)).collect();
    let mst = st.iter().sum::<f32>() / st.len() as f32;
    let std = (st.iter().map(|s| (s - mst).powi(2)).sum::<f32>() / st.len() as f32).sqrt();
    Some(F0Stats { mean_hz: mean, median_hz: median, std_st: std, voiced_ratio: voiced.len() as f32 / track.speech_hops.max(1) as f32 })
}

/// Layer C: compresses the F0 movement around the clause's median by `amount` (0 = unchanged,
/// 0.35 = 35 % flatter) with TD-PSOLA, keeping the duration, and lifts a question's end by
/// `rise_st`. Unvoiced sound passes through.
pub fn flatten_f0(input: &[f32], rate: u32, amount: f32, rise_st: f32) -> Vec<f32> {
    reshape_with(input, rate, amount, rise_st, |_| 1.0)
}

/// How far a question's last syllables rise (semitones): the flattening lets go over the
/// clause's last `RISE_TAIL_SECS` and the voice glides up this much by its end.
pub const QUESTION_RISE_ST: f32 = 1.5;
const RISE_TAIL_SECS: f32 = 0.4;

/// How much of the distance between a JAI clause's median F0 and the clip's the lock closes
/// (1 = every clause exactly on the clip's median; a little less keeps a trace of the
/// sentence's own melody between clauses).
pub const F0_LOCK: f32 = 0.9;
/// The lock never moves a clause further than this many semitones: a clause whose pitch was
/// tracked wrongly is not dragged an octave away.
pub const F0_LOCK_MAX_ST: f32 = 2.5;

/// The pitch step that pulls a clause whose median F0 is `clause_hz` towards `target_hz`.
pub fn lock_step(clause_hz: f32, target_hz: f32) -> f32 {
    if clause_hz <= 0.0 || target_hz <= 0.0 {
        return 1.0;
    }
    2f32.powf((semitones(target_hz, clause_hz) * F0_LOCK).clamp(-F0_LOCK_MAX_ST, F0_LOCK_MAX_ST) / 12.0)
}

/// Layer C for the JAI voice: the flattening, and with `target_hz` (the clip's median F0 as
/// this stage must produce it) the F0 lock — the clause's whole contour moved so that its
/// median sits on the clip's. One pitch track serves both. `rise_st`: a question's end rise
/// (`QUESTION_RISE_ST`; 0 for a statement).
pub fn lock_f0(input: &[f32], rate: u32, amount: f32, target_hz: Option<f32>, rise_st: f32) -> Vec<f32> {
    match target_hz {
        Some(target) => reshape_with(input, rate, amount, rise_st, |clause| lock_step(clause.median_hz, target)),
        None => flatten_f0(input, rate, amount, rise_st),
    }
}

/// TD-PSOLA on the pitch alone: the F0 movement around the clause's median is compressed by
/// `amount` and the whole contour moved by the step chosen from the clause's own F0 statistics
/// (0.8 = a major third lower). Duration and the spectral envelope stay: the vowels keep their
/// formants, which a resampling shift drags along with the pitch.
pub fn reshape_with(input: &[f32], rate: u32, amount: f32, rise_st: f32, step: impl Fn(&F0Stats) -> f32) -> Vec<f32> {
    let n = input.len();
    if n < rate as usize / 5 {
        return input.to_vec();
    }
    let track = pitch_track(input, rate);
    let Some(stats) = f0_stats(&track) else { return input.to_vec() };
    let ratio = step(&stats).clamp(0.5, 2.0);
    if amount <= 0.0 && (ratio - 1.0).abs() < 1e-3 && rise_st <= 0.0 {
        return input.to_vec();
    }
    let hop = track.hop.max(1);
    let f0_at = |i: usize| -> f32 {
        let k = (i / hop).min(track.f0.len().saturating_sub(1));
        track.f0.get(k).copied().unwrap_or(0.0)
    };
    let tail = (RISE_TAIL_SECS * rate as f32) as usize;
    let target = |f: f32, at: usize| -> f32 {
        // A question's end: the flattening lets go and the voice glides up.
        let end = if rise_st > 0.0 { ((at + tail) as f32 - n as f32) / tail as f32 } else { 0.0 }.clamp(0.0, 1.0);
        let flat = amount.clamp(0.0, 1.0) * (1.0 - end);
        ratio * stats.median_hz * 2f32.powf((semitones(f, stats.median_hz) * (1.0 - flat) + rise_st * end * end) / 12.0)
    };
    // Analysis marks: pitch-synchronous in voiced runs (snapped to the waveform's peaks), every
    // 5 ms elsewhere. (position, half-width, voiced)
    let unvoiced_step = (rate as usize / 200).max(8);
    let mut marks: Vec<(usize, usize, bool)> = Vec::new();
    let mut t = 0usize;
    let mut in_voiced = false;
    while t < n {
        let f = f0_at(t);
        if f > 0.0 {
            let period = ((rate as f32 / f).round() as usize).max(8);
            let reach = if in_voiced { period / 5 } else { period / 2 };
            let (lo, hi) = (t.saturating_sub(reach), (t + reach).min(n - 1));
            let peak = (lo..=hi).max_by(|a, b| input[*a].abs().total_cmp(&input[*b].abs())).unwrap_or(t);
            let at = if marks.last().is_some_and(|m| peak <= m.0) { t } else { peak };
            marks.push((at, period, true));
            in_voiced = true;
            t = at + period;
        } else {
            marks.push((t, unvoiced_step, false));
            in_voiced = false;
            t += unvoiced_step;
        }
    }
    let mut out = vec![0.0f32; n];
    let mut weight = vec![0.0f32; n];
    let mut s = marks.first().map(|m| m.0 as f32).unwrap_or(0.0);
    let mut i = 0usize;
    while (s as usize) < n {
        while i + 1 < marks.len() && (marks[i + 1].0 as f32 - s).abs() <= (marks[i].0 as f32 - s).abs() {
            i += 1;
        }
        let (a, half, voiced) = marks[i];
        let centre = s.round() as isize;
        for j in -(half as isize)..(half as isize) {
            let (src, dst) = (a as isize + j, centre + j);
            if src < 0 || dst < 0 || src as usize >= n || dst as usize >= n {
                continue;
            }
            let w = 0.5 + 0.5 * (PI * j as f32 / half as f32).cos();
            out[dst as usize] += input[src as usize] * w;
            weight[dst as usize] += w;
        }
        let step = if voiced { rate as f32 / target(f0_at(a), a).max(F0_MIN * 0.75) } else { half as f32 };
        s += step.max(4.0);
    }
    for (o, w) in out.iter_mut().zip(&weight) {
        *o = if *w > 1e-3 { *o / w.max(0.5) } else { 0.0 };
    }
    out
}

/// The F0 flattening the chain applies (set from the config; milli-units).
static F0_FLATTEN: AtomicU32 = AtomicU32::new((DEFAULT_F0_FLATTEN * 1000.0) as u32);

pub fn set_f0_flatten(amount: f32) {
    F0_FLATTEN.store((amount.clamp(0.0, 0.8) * 1000.0) as u32, Ordering::Release);
}

/// The flattening for a configured amount: a value Sir set himself wins; the default gives way
/// to the calibrated one.
pub fn effective_f0_flatten(configured: f32, calibrated: Option<f32>) -> f32 {
    if (configured - DEFAULT_F0_FLATTEN).abs() > 1e-4 {
        configured
    } else {
        calibrated.unwrap_or(configured)
    }
}

pub fn f0_flatten() -> f32 {
    F0_FLATTEN.load(Ordering::Acquire) as f32 / 1000.0
}

// ----------------------------------------------------------------- the reference profile ----

/// Everything measured on the clip.
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceProfile {
    /// Long-term 128-band log-mel envelope of the speech (dB).
    pub envelope_db: [f32; MEL_BANDS],
    /// Speech RMS (dBFS) and crest factor (dB).
    pub rms_db: f32,
    pub crest_db: f32,
    pub speech_secs: f32,
    pub f0: F0Stats,
    /// The calibrated Kokoro blend (Layer A), once `calibrate` ran.
    pub blend: Option<String>,
    /// The calibrated F0 flattening (Layer C), once `calibrate` ran.
    pub flatten: Option<f32>,
    /// The clip this was measured on: (size, modified seconds since the epoch).
    pub source: (u64, u64),
}

const PROFILE_MAGIC: &[u8; 4] = b"JRVP";
/// 3: the envelope has 128 bands (it had 64).
const PROFILE_VERSION: u32 = 3;

impl ReferenceProfile {
    pub fn describe(&self) -> String {
        format!(
            "{:.1} s of speech, RMS {:.1} dBFS, crest {:.1} dB, F0 mean {:.1} Hz (median {:.1} Hz, σ {:.2} st, {:.0} % voiced){}",
            self.speech_secs,
            self.rms_db,
            self.crest_db,
            self.f0.mean_hz,
            self.f0.median_hz,
            self.f0.std_st,
            self.f0.voiced_ratio * 100.0,
            self.blend.as_deref().map(|b| format!(", calibrated blend {b}")).unwrap_or_default()
                + &self.flatten.map(|f| format!(", F0 flattening {:.0} %", f * 100.0)).unwrap_or_default()
        )
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = PROFILE_MAGIC.to_vec();
        out.extend_from_slice(&PROFILE_VERSION.to_le_bytes());
        out.extend_from_slice(&self.source.0.to_le_bytes());
        out.extend_from_slice(&self.source.1.to_le_bytes());
        for v in self.envelope_db.iter().chain(&[self.rms_db, self.crest_db, self.speech_secs, self.f0.mean_hz, self.f0.median_hz, self.f0.std_st, self.f0.voiced_ratio]) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.flatten.unwrap_or(-1.0).to_le_bytes());
        let blend = self.blend.clone().unwrap_or_default();
        out.extend_from_slice(&(blend.len() as u32).to_le_bytes());
        out.extend_from_slice(blend.as_bytes());
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut at = 0usize;
        let mut take = |n: usize| -> Option<&[u8]> {
            let s = bytes.get(at..at + n)?;
            at += n;
            Some(s)
        };
        if take(4)? != PROFILE_MAGIC {
            return None;
        }
        if u32::from_le_bytes(take(4)?.try_into().ok()?) != PROFILE_VERSION {
            return None;
        }
        let size = u64::from_le_bytes(take(8)?.try_into().ok()?);
        let modified = u64::from_le_bytes(take(8)?.try_into().ok()?);
        let mut floats = [0.0f32; MEL_BANDS + 7];
        for f in floats.iter_mut() {
            *f = f32::from_le_bytes(take(4)?.try_into().ok()?);
        }
        let flatten = f32::from_le_bytes(take(4)?.try_into().ok()?);
        let len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        let blend = String::from_utf8(take(len)?.to_vec()).ok()?;
        let rest = &floats[MEL_BANDS..];
        Some(Self {
            envelope_db: std::array::from_fn(|b| floats[b]),
            rms_db: rest[0],
            crest_db: rest[1],
            speech_secs: rest[2],
            f0: F0Stats { mean_hz: rest[3], median_hz: rest[4], std_st: rest[5], voiced_ratio: rest[6] },
            blend: (!blend.is_empty()).then_some(blend),
            flatten: (flatten >= 0.0).then_some(flatten),
            source: (size, modified),
        })
    }

    /// Analyses a decoded clip (mono, any rate).
    pub fn analyze(samples: &[f32], rate: u32, source: (u64, u64)) -> Option<Self> {
        let (envelope_db, speech_secs) = mel_envelope(samples, rate)?;
        let level = crate::voice_reference::analyze(samples, rate)?;
        let f0 = f0_stats(&pitch_track(samples, rate))?;
        Some(Self { envelope_db, rms_db: level.rms_db, crest_db: level.crest_db, speech_secs, f0, blend: None, flatten: None, source })
    }
}

/// (size, modified) of a file, the cache key.
fn file_key(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some((meta.len(), modified))
}

/// The clip's profile: from the cache when it matches the clip, else analysed (and cached).
pub fn load_profile(clip: &Path, cache: &Path) -> Result<ReferenceProfile, String> {
    let key = file_key(clip).ok_or_else(|| format!("{} is not readable", clip.display()))?;
    if let Some(cached) = std::fs::read(cache).ok().and_then(|b| ReferenceProfile::from_bytes(&b)) {
        if cached.source == key {
            return Ok(cached);
        }
    }
    let (pcm, rate) = crate::voice_reference::decode(clip)?;
    let profile = ReferenceProfile::analyze(&pcm, rate, key).ok_or("the clip holds no speech")?;
    if let Err(e) = std::fs::write(cache, profile.to_bytes()) {
        warn!("[VOICE] Could not cache the reference profile at {}: {e}", cache.display());
    }
    Ok(profile)
}

/// The cached profile next to the clip JAI found (never in unit tests).
static PROFILE: LazyLock<Option<ReferenceProfile>> = LazyLock::new(|| {
    if cfg!(test) {
        return None;
    }
    let Some(clip) = crate::voice_reference::reference_path() else {
        info!("[VOICE] No reference clip; drop the film voice at assets/jai_voice_reference.mp3 to transfer its timbre.");
        return None;
    };
    let started = std::time::Instant::now();
    match load_profile(&clip, Path::new(PROFILE_PATH)) {
        Ok(p) => {
            info!("[VOICE] Reference clip {} ready in {} ms: {}", clip.display(), started.elapsed().as_millis(), p.describe());
            Some(p)
        }
        Err(e) => {
            warn!("[VOICE] Reference clip {}: {e}", clip.display());
            None
        }
    }
});

pub fn profile() -> Option<&'static ReferenceProfile> {
    PROFILE.as_ref()
}

/// The calibrated blend the `bm_jai_movie_1to1` alias uses (None: not calibrated).
pub fn calibrated_blend() -> Option<String> {
    profile().and_then(|p| p.blend.clone())
}

// --------------------------------------------------------------------------- transfer ----

/// Layer B: imposes the clip's envelope on the chain's output, clause by clause.
pub struct SpectralTransfer {
    reference: [f32; MEL_BANDS],
    target_rms_db: f32,
    average: Option<[f32; MEL_BANDS]>,
    speech_secs: f32,
    /// How much of the way to the clip's envelope a clause is moved, and by how many dB a band
    /// at most.
    alpha: f32,
    max_db: f32,
}

impl SpectralTransfer {
    pub fn new(profile: &ReferenceProfile) -> Self {
        Self::with(profile, TRANSFER_ALPHA, MAX_TRANSFER_DB)
    }

    /// A transfer that goes `alpha` of the way to the clip's envelope, `max_db` a band at most.
    pub fn with(profile: &ReferenceProfile, alpha: f32, max_db: f32) -> Self {
        Self { reference: shape(&profile.envelope_db), target_rms_db: profile.rms_db.max(MIN_TARGET_RMS_DB), average: None, speech_secs: 0.0, alpha: alpha.clamp(0.0, 1.0), max_db: max_db.max(0.0) }
    }

    #[cfg(test)]
    pub fn target_rms_db(&self) -> f32 {
        self.target_rms_db
    }

    /// The per-band gains (dB) for a clause with this envelope (also updates the running average).
    pub fn gains_for(&mut self, clause: &[f32; MEL_BANDS], speech_secs: f32) -> [f32; MEL_BANDS] {
        let clause = shape(clause);
        let w = 1.0 - (-speech_secs / AVERAGE_SECS).exp();
        let average = match self.average {
            Some(a) => std::array::from_fn(|b| a[b] + w * (clause[b] - a[b])),
            None => clause,
        };
        self.average = Some(average);
        self.speech_secs += speech_secs;
        let syn: [f32; MEL_BANDS] = std::array::from_fn(|b| CLAUSE_WEIGHT * clause[b] + (1.0 - CLAUSE_WEIGHT) * average[b]);
        let raw: [f32; MEL_BANDS] = std::array::from_fn(|b| (self.alpha * (self.reference[b] - syn[b])).clamp(-self.max_db, self.max_db));
        // A 1-2-3-2-1 smoothing across neighbouring bands keeps the filter free of ripple (about
        // as wide in Hz as the 1-2-1 over the 64 bands the transfer began with).
        const KERNEL: [f32; 5] = [1.0, 2.0, 3.0, 2.0, 1.0];
        std::array::from_fn(|b| {
            let at = |k: usize| raw[(b + k).saturating_sub(2).min(MEL_BANDS - 1)];
            KERNEL.iter().enumerate().map(|(k, w)| w * at(k)).sum::<f32>() / KERNEL.iter().sum::<f32>()
        })
    }

    /// Transfers the envelope onto a mastered clause and locks its level (stays under the
    /// −1 dBFS limiter).
    pub fn process(&mut self, audio: &mut Vec<f32>, rate: u32) {
        // A clause too short to measure ("Sir,", "On it.") gets the filter of the ones before it.
        let gains = match mel_envelope(audio, rate) {
            Some((env, secs)) => Some(self.gains_for(&env, secs)),
            None => self.average.map(|average| self.gains_for(&average, 0.0)),
        };
        if let Some(gains) = gains {
            *audio = apply_band_gains(audio, rate, &gains);
        }
        if let Some(now) = crate::voice_reference::speech_rms_db(audio, rate) {
            let gain = db_to_gain((self.target_rms_db - now).clamp(-MAX_LEVEL_DB, MAX_LEVEL_DB));
            audio.iter_mut().for_each(|s| *s = limit(*s * gain));
        }
    }
}

/// Filters `audio` with per-band gains (dB, at the mel band centres; interpolated between them,
/// held beyond the ends) by STFT overlap-add with √Hann windows at 75 % overlap.
pub fn apply_band_gains(audio: &[f32], rate: u32, gains_db: &[f32; MEL_BANDS]) -> Vec<f32> {
    if audio.is_empty() {
        return Vec::new();
    }
    let centres_mel: Vec<f32> = band_centres().iter().map(|f| hz_to_mel(*f)).collect();
    let bin_hz = rate as f32 / FFT_SIZE as f32;
    let gains: Vec<f32> = (0..=FFT_SIZE / 2)
        .map(|k| {
            let m = hz_to_mel(k as f32 * bin_hz);
            let db = if m <= centres_mel[0] {
                gains_db[0]
            } else if m >= centres_mel[MEL_BANDS - 1] {
                gains_db[MEL_BANDS - 1]
            } else {
                let i = centres_mel.iter().position(|c| *c > m).unwrap_or(MEL_BANDS - 1);
                let t = (m - centres_mel[i - 1]) / (centres_mel[i] - centres_mel[i - 1]);
                gains_db[i - 1] + t * (gains_db[i] - gains_db[i - 1])
            };
            db_to_gain(db)
        })
        .collect();
    let window: Vec<f32> = WINDOW.iter().map(|w| w.sqrt()).collect();
    let hop = FFT_SIZE / 4;
    // Padded so every sample is covered by four frames.
    let pad = FFT_SIZE;
    let mut x = vec![0.0f32; pad];
    x.extend_from_slice(audio);
    x.extend(std::iter::repeat_n(0.0, pad + FFT_SIZE));
    let mut y = vec![0.0f32; x.len()];
    let (mut re, mut im) = (vec![0.0f32; FFT_SIZE], vec![0.0f32; FFT_SIZE]);
    let mut start = 0;
    while start + FFT_SIZE <= x.len() {
        for i in 0..FFT_SIZE {
            re[i] = x[start + i] * window[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im, false);
        for k in 0..=FFT_SIZE / 2 {
            re[k] *= gains[k];
            im[k] *= gains[k];
            if k > 0 && k < FFT_SIZE / 2 {
                re[FFT_SIZE - k] = re[k];
                im[FFT_SIZE - k] = -im[k];
            }
        }
        fft(&mut re, &mut im, true);
        // 1/n for the inverse FFT, and √Hann² at 75 % overlap sums to 2.
        let scale = 1.0 / (FFT_SIZE as f32 * 2.0);
        for i in 0..FFT_SIZE {
            y[start + i] += re[i] * window[i] * scale;
        }
        start += hop;
    }
    y[pad..pad + audio.len()].to_vec()
}

static TRANSFER: LazyLock<Option<Mutex<SpectralTransfer>>> = LazyLock::new(|| profile().map(|p| Mutex::new(SpectralTransfer::new(p))));

/// Stage 5 of the chain: the envelope transfer and level lock (no-op without a clip).
pub fn transfer_clause(audio: &mut Vec<f32>, rate: u32) {
    if let Some(t) = TRANSFER.as_ref() {
        t.lock().unwrap_or_else(|e| e.into_inner()).process(audio, rate);
    }
}

/// Analyses (or loads) the clip in the background at startup.
pub fn preload() {
    std::thread::Builder::new()
        .name("voice-reference".into())
        .spawn(move || {
            LazyLock::force(&TRANSFER);
            if let Some(f) = profile().and_then(|p| p.flatten) {
                if (f0_flatten() - DEFAULT_F0_FLATTEN).abs() < 1e-4 {
                    set_f0_flatten(f);
                    info!("[VOICE] Calibrated F0 flattening: {:.0} %", f * 100.0);
                }
            }
        })
        .ok();
}

// ------------------------------------------------------------ calibration & verification ----

/// How far a mastered clause is from the clip: (spectral similarity, F0 median distance in
/// semitones, F0 width ratio).
#[derive(Debug, Clone, Copy)]
pub struct Match {
    pub similarity: f32,
    pub f0_offset_st: f32,
    pub f0_width_ratio: f32,
    pub f0: Option<F0Stats>,
}

impl Match {
    pub fn measure(audio: &[f32], rate: u32, reference: &ReferenceProfile) -> Option<Self> {
        let (env, _) = mel_envelope(audio, rate)?;
        let f0 = f0_stats(&pitch_track(audio, rate));
        Some(Self {
            similarity: spectral_similarity(&env, &reference.envelope_db),
            f0_offset_st: f0.map(|f| semitones(f.median_hz, reference.f0.median_hz)).unwrap_or(12.0),
            f0_width_ratio: f0.map(|f| f.std_st / reference.f0.std_st.max(0.05)).unwrap_or(0.0),
            f0,
        })
    }

    /// Lower is closer (the calibration's objective).
    pub fn distance(&self) -> f32 {
        (1.0 - self.similarity) * 10.0 + self.f0_offset_st.abs() * 0.3 + (self.f0_width_ratio - 1.0).abs() * 0.5
    }

    pub fn describe(&self) -> String {
        format!(
            "spectral similarity {:.3}, F0 {:+.2} st from the clip, intonation width ×{:.2}{}",
            self.similarity,
            self.f0_offset_st,
            self.f0_width_ratio,
            self.f0.map(|f| format!(" (median {:.1} Hz)", f.median_hz)).unwrap_or_default()
        )
    }
}

/// The blends Layer A tries: Fable, George and Lewis in quarter steps.
pub fn candidate_blends() -> Vec<String> {
    let mut out = Vec::new();
    for f in 0..=4 {
        for g in 0..=(4 - f) {
            let l = 4 - f - g;
            let parts: Vec<String> = [("bm_fable", f), ("bm_george", g), ("bm_lewis", l)]
                .iter()
                .filter(|(_, w)| *w > 0)
                .map(|(v, w)| format!("{v}:{:.2}", *w as f32 / 4.0))
                .collect();
            out.push(parts.join("+"));
        }
    }
    out
}

/// Synthesises `text` with Kokoro and runs JAI's chain up to (not including) the transfer.
/// `lock_to_hz`: the clip's median F0 the clause is locked to (None: no lock).
pub(crate) fn synth_dry(kokoro: &mut crate::tts::Kokoro, phonemizer: &crate::phonemizer::Phonemizer, text: &str, voice: &str, speed: f32, lock_to_hz: Option<f32>) -> Result<Vec<f32>, String> {
    let ipa = phonemizer.phonemize(&crate::tts::normalize_for_speech(text));
    let audio = kokoro.synthesize(&ipa, voice, crate::jai_voice_dsp::synthesis_speed(speed))?;
    Ok(crate::jai_voice_dsp::master_dry_to(&audio, crate::tts::KOKORO_RATE, lock_to_hz, crate::jai_voice_dsp::is_question(text)))
}

/// What another setting must be closer by before it replaces the calibrated one: the voice Sir
/// is used to does not change for a hair's breadth.
pub const KEEP_MARGIN: f32 = 0.15;

/// How far `voice` is from the clip, on the clip's own sentence and on one it does not hold (a
/// setting that only fits the one sentence is no match — measured 2026-11-05: the blend closest
/// on the clip's sentence alone spoke the other one 4.5 semitones too low). Returns the match
/// on the clip's sentence and the mean distance over both.
fn fit(kokoro: &mut crate::tts::Kokoro, phonemizer: &crate::phonemizer::Phonemizer, reference: &ReferenceProfile, voice: &str, speed: f32) -> Result<(Match, f32), String> {
    let mut measure = |sentence: &str| -> Result<Match, String> {
        let audio = synth_dry(kokoro, phonemizer, sentence, voice, speed, Some(reference.f0.median_hz))?;
        Match::measure(&audio, crate::jai_voice_dsp::MASTER_RATE, reference).ok_or_else(|| "the synthesised sentence holds no speech".to_string())
    };
    let on_clip = measure(&reference_transcript())?;
    let held_out = measure(HELD_OUT_SENTENCE)?;
    Ok((on_clip, (on_clip.distance() + held_out.distance()) / 2.0))
}

/// Layer A: picks the blend closest to the clip and stores it in the profile cache. The blend
/// and the flattening already calibrated stay unless another is closer by `KEEP_MARGIN`.
pub fn calibrate(cfg: &crate::config::JaiConfig) -> Result<(String, Match), String> {
    let clip = crate::voice_reference::reference_path().ok_or("no reference clip (assets/jai_voice_reference.mp3)")?;
    let mut reference = load_profile(&clip, Path::new(PROFILE_PATH))?;
    let phonemizer = crate::phonemizer::Phonemizer::new(cfg.espeak_path.as_deref());
    let mut kokoro = crate::tts::Kokoro::load(cfg)?;
    // The flattening in use while the blends are compared.
    let in_use = effective_f0_flatten(cfg.voice_f0_flatten, reference.flatten);
    set_f0_flatten(in_use);
    let mut candidates = candidate_blends();
    if let Some(kept) = reference.blend.clone().filter(|b| !candidates.contains(b)) {
        candidates.push(kept);
    }
    // (blend, its match on the clip's sentence, its distance over both sentences)
    let mut best: Option<(String, Match, f32)> = None;
    let mut kept: Option<(String, Match, f32)> = None;
    for blend in candidates {
        let (m, distance) = match fit(&mut kokoro, &phonemizer, &reference, &blend, cfg.kokoro_speed) {
            Ok(fit) => fit,
            Err(e) => {
                warn!("[VOICE] Calibration: {blend} failed: {e}");
                continue;
            }
        };
        info!("[VOICE] Calibration {blend:<40} distance {distance:.3}: {}", m.describe());
        if reference.blend.as_deref() == Some(blend.as_str()) {
            kept = Some((blend.clone(), m, distance));
        }
        if best.as_ref().is_none_or(|(_, _, b)| distance < *b) {
            best = Some((blend, m, distance));
        }
    }
    let best = best.ok_or("no blend could be synthesised")?;
    let (blend, mut m, mut distance) = match kept {
        Some(kept) if kept.2 <= best.2 + KEEP_MARGIN => kept,
        _ => best,
    };
    // Layer C: how much flattening brings the intonation closest to the clip's. The amount in
    // use stays unless another is clearly closer.
    let mut flatten = in_use;
    let mut margin = KEEP_MARGIN;
    for amount in FLATTEN_CANDIDATES {
        if (amount - in_use).abs() < 1e-4 {
            continue;
        }
        set_f0_flatten(amount);
        let Ok((c, d)) = fit(&mut kokoro, &phonemizer, &reference, &blend, cfg.kokoro_speed) else { continue };
        info!("[VOICE] Calibration F0 flattening {:>3.0} %: distance {d:.3}: {}", amount * 100.0, c.describe());
        if d + margin < distance {
            // Once the amount in use is beaten, the closest of the others wins outright.
            (flatten, m, distance, margin) = (amount, c, d, 0.0);
        }
    }
    set_f0_flatten(flatten);
    reference.blend = Some(blend.clone());
    reference.flatten = Some(flatten);
    std::fs::write(PROFILE_PATH, reference.to_bytes()).map_err(|e| format!("{PROFILE_PATH}: {e}"))?;
    info!("[VOICE] Calibrated: {blend}, F0 flattening {:.0} % (distance {distance:.3}; {})", flatten * 100.0, m.describe());
    Ok((blend, m))
}

/// The F0 flattening amounts the calibration compares.
pub const FLATTEN_CANDIDATES: [f32; 5] = [0.0, 0.1, 0.2, 0.3, 0.35];

/// Layer D's report.
#[derive(Debug, Clone)]
pub struct Verification {
    pub voice: String,
    pub dry: Match,
    pub transferred: Match,
    pub held_out: Option<Match>,
    pub reference: ReferenceProfile,
}

impl Verification {
    pub fn passed(&self) -> bool {
        self.transferred.similarity >= SIMILARITY_TARGET
    }

    pub fn describe(&self) -> String {
        format!(
            "voice {}\n  clip:        {}\n  dry chain:   {}\n  transferred: {}\n  held-out:    {}\n  target spectral similarity ≥ {SIMILARITY_TARGET}: {}",
            self.voice,
            self.reference.describe(),
            self.dry.describe(),
            self.transferred.describe(),
            self.held_out.map(|m| m.describe()).unwrap_or_else(|| "n/a".into()),
            if self.passed() { "PASSED" } else { "NOT REACHED" }
        )
    }
}

/// Layer D: the clip's own sentence through the whole chain, measured against the clip; then a
/// held-out sentence through the same (now warmed-up) transfer.
pub fn verify_with(cfg: &crate::config::JaiConfig, clip: &Path, cache: &Path) -> Result<Verification, String> {
    let reference = load_profile(clip, cache)?;
    let phonemizer = crate::phonemizer::Phonemizer::new(cfg.espeak_path.as_deref());
    let mut kokoro = crate::tts::Kokoro::load(cfg)?;
    let voice = if cfg.kokoro_voice == crate::tts::JAI_VOICE { reference.blend.clone().unwrap_or_else(|| crate::tts::JAI_VOICE_BLEND.to_string()) } else { cfg.kokoro_voice.clone() };
    set_f0_flatten(effective_f0_flatten(cfg.voice_f0_flatten, reference.flatten));
    let rate = crate::jai_voice_dsp::MASTER_RATE;
    let lock = Some(reference.f0.median_hz);
    let mut audio = synth_dry(&mut kokoro, &phonemizer, &reference_transcript(), &voice, cfg.kokoro_speed, lock)?;
    let dry = Match::measure(&audio, rate, &reference).ok_or("the synthesised sentence holds no speech")?;
    let mut transfer = SpectralTransfer::new(&reference);
    transfer.process(&mut audio, rate);
    let transferred = Match::measure(&audio, rate, &reference).ok_or("the transferred sentence holds no speech")?;
    let held_out = synth_dry(&mut kokoro, &phonemizer, HELD_OUT_SENTENCE, &voice, cfg.kokoro_speed, lock).ok().and_then(|mut a| {
        transfer.process(&mut a, rate);
        Match::measure(&a, rate, &reference)
    });
    Ok(Verification { voice, dry, transferred, held_out, reference })
}

/// The five kinds of sentence a voice is measured on: the clip's own statement, a status
/// report, a question, a technical line with figures (and the homograph "live", which must be
/// read "lyve"), and a short alert.
pub const VOICE_SENTENCES: [(&str, &str); 5] = [
    ("statement", "The workshop is secure, the reactor is stable, and every system reports ready for the morning."),
    ("report", HELD_OUT_SENTENCE),
    ("question", "Shall I put the report on the board, Sir, or would you rather hear the short version first?"),
    ("technical", "Live telemetry shows the reactor at 3.2 gigawatts and 61 degrees, and the interface is live on all three monitors."),
    ("alert", "Sir, the graphics card has reached its limit. I recommend a pause."),
];

/// Layer D across sentence types: `VOICE_SENTENCES` in JAI's finished voice, as the speaker
/// gets them (synthesis, F0 lock, mastering, the clip's envelope), each with its kind and the
/// text as spoken after the pronunciation rules. The transfer is warmed on the clip's own
/// sentence first, as in a running session. (The exhaustive verifier measures them against
/// the clip and plays them to the speech recogniser.)
pub fn render_sentences(cfg: &crate::config::JaiConfig) -> Result<Vec<(&'static str, String, Vec<f32>)>, String> {
    let clip = crate::voice_reference::reference_path().ok_or("no reference clip (assets/jai_voice_reference.mp3)")?;
    let reference = load_profile(&clip, Path::new(PROFILE_PATH))?;
    let phonemizer = crate::phonemizer::Phonemizer::new(cfg.espeak_path.as_deref());
    let mut kokoro = crate::tts::Kokoro::load(cfg)?;
    let voice = if cfg.kokoro_voice == crate::tts::JAI_VOICE { reference.blend.clone().unwrap_or_else(|| crate::tts::JAI_VOICE_BLEND.to_string()) } else { cfg.kokoro_voice.clone() };
    set_f0_flatten(effective_f0_flatten(cfg.voice_f0_flatten, reference.flatten));
    let rate = crate::jai_voice_dsp::MASTER_RATE;
    let mut transfer = SpectralTransfer::new(&reference);
    let mut out = Vec::with_capacity(VOICE_SENTENCES.len());
    for (kind, text) in VOICE_SENTENCES {
        let mut audio = synth_dry(&mut kokoro, &phonemizer, text, &voice, cfg.kokoro_speed, Some(reference.f0.median_hz))?;
        transfer.process(&mut audio, rate);
        out.push((kind, crate::tts::normalize_for_speech(text), audio));
    }
    Ok(out)
}

pub fn verify(cfg: &crate::config::JaiConfig) -> Result<Verification, String> {
    let clip = crate::voice_reference::reference_path().ok_or("no reference clip (assets/jai_voice_reference.mp3)")?;
    verify_with(cfg, &clip, Path::new(PROFILE_PATH))
}

/// The clip, if the project has one (tests that need the real recording).
#[cfg(test)]
pub fn project_clip() -> Option<std::path::PathBuf> {
    crate::voice_reference::reference_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const R: u32 = 24_000;

    /// A voiced "vowel": a band-limited pulse train following `f0(t)` with a formant-like tilt.
    fn vowel(secs: f32, rate: u32, f0: impl Fn(f32) -> f32) -> Vec<f32> {
        let n = (secs * rate as f32) as usize;
        let mut phase = 0.0f32;
        (0..n)
            .map(|i| {
                let t = i as f32 / rate as f32;
                let f = f0(t);
                phase = (phase + TAU * f / rate as f32) % TAU;
                let mut s = 0.0;
                let mut k = 1.0;
                while f * k < rate as f32 * 0.45 && k < 40.0 {
                    s += (phase * k).sin() / k.powf(1.1);
                    k += 1.0;
                }
                s * 0.08
            })
            .collect()
    }

    #[test]
    fn fft_matches_a_direct_dft_and_inverts() {
        let n = 64;
        let x: Vec<f32> = (0..n).map(|i| ((i * 7 % 11) as f32 - 5.0) * 0.1).collect();
        let (mut re, mut im) = (x.clone(), vec![0.0; n]);
        fft(&mut re, &mut im, false);
        for k in [0, 1, 5, 31, 32] {
            let (mut dr, mut di) = (0.0f64, 0.0f64);
            for (i, v) in x.iter().enumerate() {
                let a = -2.0 * std::f64::consts::PI * (k * i) as f64 / n as f64;
                dr += *v as f64 * a.cos();
                di += *v as f64 * a.sin();
            }
            assert!((re[k] as f64 - dr).abs() < 1e-3 && (im[k] as f64 - di).abs() < 1e-3, "bin {k}");
        }
        fft(&mut re, &mut im, true);
        for (a, b) in re.iter().zip(&x) {
            assert!((a / n as f32 - b).abs() < 1e-4);
        }
    }

    #[test]
    fn mel_bands_cover_60_hz_to_12_khz_and_are_never_empty() {
        let c = band_centres();
        assert!(c[0] > 60.0 && c[0] < 120.0 && c[MEL_BANDS - 1] < 12_000.0 && c[MEL_BANDS - 1] > 10_000.0);
        assert!(c.windows(2).all(|w| w[1] > w[0]));
        assert!(MEL_48K.filters.iter().all(|f| !f.is_empty()));
    }

    #[test]
    fn band_gains_are_transparent_at_0_db_and_shape_the_spectrum() {
        let x = vowel(1.0, 48_000, |_| 130.0);
        let same = apply_band_gains(&x, 48_000, &[0.0; MEL_BANDS]);
        let err = x.iter().zip(&same).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(err < 1e-3, "perfect reconstruction: {err}");
        // −6 dB everywhere: half the amplitude.
        let quiet = apply_band_gains(&x, 48_000, &[-6.0206; MEL_BANDS]);
        let ratio = crate::dsp::rms(&quiet[4800..43_200]) / crate::dsp::rms(&x[4800..43_200]);
        assert!((ratio - 0.5).abs() < 0.01, "{ratio}");
        // A tilt shows up in the envelope.
        let tilt: [f32; MEL_BANDS] = std::array::from_fn(|b| -6.0 + 12.0 * b as f32 / (MEL_BANDS - 1) as f32);
        let bright = apply_band_gains(&x, 48_000, &tilt);
        let (before, _) = mel_envelope(&x, 48_000).unwrap();
        let (after, _) = mel_envelope(&bright, 48_000).unwrap();
        let low = (after[3] - before[3], after[MEL_BANDS - 4] - before[MEL_BANDS - 4]);
        assert!(low.0 < -3.0 && low.1 > 3.0, "{low:?}");
    }

    #[test]
    fn yin_tracks_a_known_pitch() {
        let x = vowel(1.0, R, |_| 121.4);
        let stats = f0_stats(&pitch_track(&x, R)).unwrap();
        assert!((stats.median_hz - 121.4).abs() < 1.5, "{stats:?}");
        assert!(stats.std_st < 0.2 && stats.voiced_ratio > 0.9);
        assert!(f0_stats(&pitch_track(&vec![0.0; 24_000], R)).is_none(), "silence is unvoiced");
    }

    #[test]
    fn psola_flattens_intonation_by_35_percent_and_keeps_duration() {
        // A sing-song contour: 125 Hz ± 20 % at 1.5 Hz.
        let contour = |t: f32| 125.0 * 2f32.powf(0.27 * (TAU * 1.5 * t).sin());
        let x = vowel(2.0, R, contour);
        let before = f0_stats(&pitch_track(&x, R)).unwrap();
        let y = flatten_f0(&x, R, 0.35, 0.0);
        assert_eq!(y.len(), x.len(), "same duration");
        assert!(y.iter().all(|s| s.is_finite()));
        let after = f0_stats(&pitch_track(&y, R)).unwrap();
        let ratio = after.std_st / before.std_st;
        assert!((ratio - 0.65).abs() < 0.12, "width {:.2} → {:.2} st (×{ratio:.2})", before.std_st, after.std_st);
        assert!(semitones(after.median_hz, before.median_hz).abs() < 0.4, "the level stays: {before:?} → {after:?}");
        let level = crate::dsp::rms(&y) / crate::dsp::rms(&x);
        assert!((0.85..1.15).contains(&level), "loudness kept: {level}");
        // Off: bit-exact.
        assert_eq!(flatten_f0(&x, R, 0.0, 0.0), x);
    }

    #[test]
    fn the_f0_lock_puts_a_clause_on_the_clips_median_and_keeps_its_melody() {
        // A clause a semitone and a half under the clip's 115 Hz, with a gentle melody.
        let contour = |t: f32| 105.5 * 2f32.powf(0.08 * (TAU * 1.2 * t).sin());
        let x = vowel(2.0, R, contour);
        let before = f0_stats(&pitch_track(&x, R)).unwrap();
        let y = lock_f0(&x, R, 0.0, Some(115.0), 0.0);
        assert_eq!(y.len(), x.len(), "same duration");
        let after = f0_stats(&pitch_track(&y, R)).unwrap();
        // Nine tenths of the way to the clip: within a fifth of a semitone of it.
        assert!(semitones(after.median_hz, 115.0).abs() < 0.3, "{before:?} → {after:?}");
        assert!(semitones(before.median_hz, 115.0) < -1.2, "it started well below: {before:?}");
        assert!((after.std_st / before.std_st - 1.0).abs() < 0.2, "the melody keeps its width: {:.2} → {:.2} st", before.std_st, after.std_st);
        // A clause already on the clip's median is left alone, bit for bit.
        let on_it = vowel(1.0, R, |_| 115.0);
        let median = f0_stats(&pitch_track(&on_it, R)).unwrap().median_hz;
        assert_eq!(lock_f0(&on_it, R, 0.0, Some(median), 0.0), on_it);
        // No clip: the flattening alone. A wild measurement is never followed further than 2.5 st.
        assert_eq!(lock_f0(&x, R, 0.0, None, 0.0), x);
        assert!((semitones(lock_step(60.0, 115.0), 1.0) - F0_LOCK_MAX_ST).abs() < 1e-3);
        assert!((semitones(lock_step(100.0, 115.0), 1.0) - semitones(115.0, 100.0) * F0_LOCK).abs() < 1e-3);
        assert_eq!((lock_step(0.0, 115.0), lock_step(115.0, 0.0)), (1.0, 1.0));
    }

    /// A question keeps its rise: the flattening lets go at its end and the voice glides up;
    /// a statement's end stays level.
    #[test]
    fn a_question_rises_at_its_end() {
        let x = vowel(1.5, R, |_| 115.0);
        let f0_in = |y: &[f32], from: f32, to: f32| {
            let t = pitch_track(y, R);
            let frame = |s: f32| ((s * R as f32) as usize / t.hop.max(1)).min(t.f0.len());
            let voiced: Vec<f32> = t.f0[frame(from)..frame(to)].iter().copied().filter(|f| *f > 0.0).collect();
            voiced.iter().sum::<f32>() / voiced.len().max(1) as f32
        };
        let asked = lock_f0(&x, R, 0.35, Some(115.0), QUESTION_RISE_ST);
        let told = lock_f0(&x, R, 0.35, Some(115.0), 0.0);
        assert_eq!(asked.len(), x.len(), "same duration");
        let rise = semitones(f0_in(&asked, 1.38, 1.46), f0_in(&asked, 0.2, 0.8));
        assert!(rise > 0.6 && rise < QUESTION_RISE_ST + 0.3, "the end rises: {rise:.2} st");
        assert!(semitones(f0_in(&told, 1.38, 1.46), f0_in(&told, 0.2, 0.8)).abs() < 0.3, "a statement stays level");
        assert!(semitones(f0_in(&asked, 0.2, 0.8), 115.0).abs() < 0.3, "only the end moves");
    }

    #[test]
    fn profile_round_trips_through_its_cache_format() {
        let p = ReferenceProfile {
            envelope_db: std::array::from_fn(|b| -40.0 - b as f32 * 0.5),
            rms_db: -33.2,
            crest_db: 17.1,
            speech_secs: 16.2,
            f0: F0Stats { mean_hz: 121.4, median_hz: 119.0, std_st: 2.1, voiced_ratio: 0.7 },
            blend: Some("bm_fable:0.75+bm_george:0.25".into()),
            flatten: Some(0.25),
            source: (358_609, 1_762_098_000),
        };
        assert_eq!(ReferenceProfile::from_bytes(&p.to_bytes()), Some(p.clone()));
        let mut bad = p.to_bytes();
        bad[0] = b'X';
        assert!(ReferenceProfile::from_bytes(&bad).is_none());
        assert!(ReferenceProfile::from_bytes(&p.to_bytes()[..20]).is_none());
    }

    #[test]
    fn transfer_moves_the_envelope_onto_the_reference_and_locks_the_level() {
        let target = vowel(3.0, 48_000, |_| 118.0);
        let darker: [f32; MEL_BANDS] = std::array::from_fn(|b| 4.0 - 8.0 * b as f32 / (MEL_BANDS - 1) as f32);
        let reference_audio = apply_band_gains(&target, 48_000, &darker);
        let reference = ReferenceProfile::analyze(&reference_audio, 48_000, (0, 0)).unwrap();
        let mut t = SpectralTransfer::new(&reference);
        let mut last = 0.0;
        let first = spectral_similarity(&mel_envelope(&target, 48_000).unwrap().0, &reference.envelope_db);
        for _ in 0..4 {
            let mut clause = vowel(1.5, 48_000, |_| 118.0);
            t.process(&mut clause, 48_000);
            last = spectral_similarity(&mel_envelope(&clause, 48_000).unwrap().0, &reference.envelope_db);
            let level = crate::voice_reference::analyze(&clause, 48_000).unwrap().rms_db;
            assert!(level <= t.target_rms_db() + 0.3 && level >= t.target_rms_db() - 6.5, "{level}");
            assert!(clause.iter().all(|s| s.is_finite() && s.abs() <= 0.9));
        }
        assert!(last > first && last > 0.97, "similarity {first:.3} → {last:.3}");
        assert!(SpectralTransfer::new(&reference).target_rms_db() >= MIN_TARGET_RMS_DB);
    }

    #[test]
    fn a_clause_too_short_to_measure_is_levelled_like_the_rest() {
        let reference = ReferenceProfile::analyze(&vowel(3.0, 48_000, |_| 118.0), 48_000, (0, 0)).unwrap();
        let mut t = SpectralTransfer::new(&reference);
        t.process(&mut vowel(1.5, 48_000, |_| 118.0), 48_000);
        // "Sir,": a fifth of a second, 4.4 dB under the clip.
        let mut sir: Vec<f32> = vowel(0.2, 48_000, |_| 118.0).iter().map(|s| s * 0.6).collect();
        t.process(&mut sir, 48_000);
        let level = crate::voice_reference::speech_rms_db(&sir, 48_000).unwrap();
        assert!((level - t.target_rms_db()).abs() < 1.0, "{level:.1} dB, the others {:.1} dB", t.target_rms_db());
    }

    #[test]
    fn gains_respect_alpha_and_the_9_db_limit() {
        let mut reference = ReferenceProfile {
            envelope_db: [0.0; MEL_BANDS],
            rms_db: -20.0,
            crest_db: 12.0,
            speech_secs: 10.0,
            f0: F0Stats { mean_hz: 120.0, median_hz: 120.0, std_st: 2.0, voiced_ratio: 0.7 },
            blend: None,
            flatten: None,
            source: (0, 0),
        };
        // A peak three bands wide (one band alone is smoothed down to a third).
        for band in 9..=11 {
            reference.envelope_db[band] = 40.0;
        }
        let mut t = SpectralTransfer::new(&reference);
        let g = t.gains_for(&[0.0; MEL_BANDS], 1.0);
        assert!(g.iter().all(|v| v.abs() <= MAX_TRANSFER_DB + 1e-4));
        assert!(g[10] > 4.0, "the boosted band is lifted (clamped, smoothed): {}", g[10]);
        assert!(candidate_blends().len() == 15 && candidate_blends().contains(&"bm_fable:0.75+bm_george:0.25".to_string()));
        // Sir's calibrated blend is one of them (it is kept unless another is clearly closer).
        assert!(candidate_blends().contains(&"bm_fable:0.50+bm_lewis:0.50".to_string()) && KEEP_MARGIN > 0.0);
    }

    #[test]
    fn sirs_own_flattening_wins_over_the_calibrated_one() {
        assert_eq!(effective_f0_flatten(DEFAULT_F0_FLATTEN, Some(0.1)), 0.1);
        assert_eq!(effective_f0_flatten(0.0, Some(0.1)), 0.0);
        assert_eq!(effective_f0_flatten(DEFAULT_F0_FLATTEN, None), DEFAULT_F0_FLATTEN);
    }

    /// Layer A on the real clip: `cargo test calibrate_the_project_voice -- --ignored --nocapture`
    /// (writes `assets/jai_reference_profile.bin`; the app reads it at its next start).
    #[test]
    #[ignore]
    fn calibrate_the_project_voice() {
        let cfg = crate::config::JaiConfig {
            kokoro_model_path: crate::models::KOKORO_V1_MODEL.into(),
            kokoro_voices_archive_path: crate::models::KOKORO_V1_VOICES.into(),
            tts_gpu: false,
            ..Default::default()
        };
        let (blend, m) = calibrate(&cfg).expect("calibration runs");
        eprintln!("calibrated {blend}: {} (distance {:.3})", m.describe(), m.distance());
    }

    /// Layer D against the real clip and the real Kokoro voice (skipped when the project has
    /// neither). Run on its own with `cargo test closed_loop -- --nocapture` to see the numbers.
    #[test]
    fn closed_loop_voice_verification_against_the_reference_clip() {
        let model = Path::new(crate::models::KOKORO_V1_MODEL);
        let (Some(clip), true) = (project_clip(), model.is_file()) else {
            eprintln!("skipped: no reference clip or Kokoro model in this checkout");
            return;
        };
        let cfg = crate::config::JaiConfig {
            kokoro_model_path: crate::models::KOKORO_V1_MODEL.into(),
            kokoro_voices_archive_path: crate::models::KOKORO_V1_VOICES.into(),
            kokoro_voice: crate::tts::JAI_VOICE.into(),
            tts_gpu: false,
            ..Default::default()
        };
        let cache = std::env::temp_dir().join(format!("jai_profile_{}.bin", uuid::Uuid::new_v4()));
        // The project's calibrated blend (if any) is used, as in the app.
        if let Ok(bytes) = std::fs::read(PROFILE_PATH) {
            let _ = std::fs::write(&cache, bytes);
        }
        let v = verify_with(&cfg, &clip, &cache).expect("verification runs");
        let _ = std::fs::remove_file(&cache);
        eprintln!("{}", v.describe());
        assert!(v.transferred.similarity > v.dry.similarity - 1e-3, "the transfer never moves away from the clip");
        assert!(v.passed(), "spectral similarity {:.3} < {SIMILARITY_TARGET}", v.transferred.similarity);
        let f0 = v.transferred.f0.expect("voiced");
        assert!((60.0..200.0).contains(&f0.median_hz), "a baritone: {f0:?}");
    }

    /// What the reference clip really holds: the pitch histogram (how sure the F0 is) and the
    /// envelope's shape. `cargo test reference_clips_measured -- --ignored --nocapture`.
    #[test]
    #[ignore = "prints measurements of the reference clip"]
    fn reference_clips_measured() {
        for (name, path) in [("JAI", crate::voice_reference::reference_path())] {
            let Some(path) = path else { continue };
            let (pcm, rate) = crate::voice_reference::decode(&path).expect("decodes");
            let track = pitch_track(&pcm, rate);
            let voiced: Vec<f32> = track.f0.iter().copied().filter(|f| *f > 0.0).collect();
            eprintln!("{name}: {} — {:.1} s at {rate} Hz, {} speech hops, {} voiced ({:.0} %)", path.display(), pcm.len() as f32 / rate as f32, track.speech_hops, voiced.len(), 100.0 * voiced.len() as f32 / track.speech_hops.max(1) as f32);
            let mut bins = [0usize; 13];
            for f in &voiced {
                bins[(((f - 60.0) / 20.0) as usize).min(12)] += 1;
            }
            let row: Vec<String> = bins.iter().enumerate().map(|(i, n)| format!("{}:{n}", 60 + 20 * i)).collect();
            eprintln!("  F0 histogram (Hz:hops) {}", row.join(" "));
            if let Some(stats) = f0_stats(&track) {
                eprintln!("  F0 mean {:.1}, median {:.1} Hz, width {:.2} st", stats.mean_hz, stats.median_hz, stats.std_st);
            }
            if let Some((env, secs)) = mel_envelope(&pcm, rate) {
                let shape = shape(&env);
                let centres = band_centres();
                let row: Vec<String> = (0..MEL_BANDS).step_by(4).map(|b| format!("{:.0}Hz:{:+.0}", centres[b], shape[b])).collect();
                eprintln!("  envelope over {secs:.1} s (dB about its mean) {}", row.join(" "));
            }
        }
    }
}
