//! The film voice's mastering chain, matched to the reference clip of the film's J.A.I.
//! (`assets/jai_voice_reference.mp3`): an ultra-clean, dry, intimate close-mic studio voice — calm
//! baritone, crisp non-rhotic RP diction, no chorus and no echo. Every synthesised clause runs
//! through these stages at 48 kHz (f32) before it reaches the speaker.
//!
//! 1. Pitch & formant lock: −0.55 semitones (×0.9687) by resampling the 24 kHz synthesis straight
//!    to 48 kHz, which puts the voice's fundamental on Paul Bettany's 118–128 Hz baritone. Pitch and
//!    formants drop together; the tempo is kept by synthesising faster (`synthesis_speed`), so
//!    JAI speaks at an unhurried 0.98×.
//! 2. Reference-matched 5-band studio EQ: 85 Hz high-pass (12 dB/oct Butterworth), +2.2 dB warm
//!    fundamental at 135 Hz (Q 1.15), −2.5 dB mud cut at 420 Hz (Q 1.5), −0.8 dB upper-mid contour
//!    at 2.85 kHz (Q 1.2), a +2.0 dB condenser-air shelf from 4.2 kHz and a soft dynamic de-esser
//!    above 6.4 kHz.
//! 3. Studio room: 98.5 % dry plus one tight 2.2 ms reflection at 1.5 % — the near wall of a vocal
//!    booth, not a doubler (the old ±3-cent micro-double read as a metallic chorus).
//! 4. Studio optical compressor (−14 dBFS, 3.2:1, 4 ms / 65 ms, +3 dB make-up, 6 dB soft knee) and
//!    a soft-knee limiter at −1 dBFS.
//! 0. F0 flattening (`fish_zero_shot_tts::flatten_f0`): TD-PSOLA compresses the pitch movement
//!    around each clause's median by 35 %, for the film voice's level, unhurried intonation.
//! 5. Reference transfer (`fish_zero_shot_tts`): when the reference clip is on disk, its 128-band
//!    mel envelope is imposed on every clause (α = 0.78, ±9 dB) and the speech level is locked to
//!    the clip's RMS (never below −21 dBFS).
//! 6. Joins: 25 ms raised-cosine crossfades between clauses (`tts::ClauseJoiner`) and 70 ms
//!    studio micro-pauses after "Sir,", "Right,", "Checking." and "On it, Sir." (`breath_cue_ms`).
//!
//! Earcons (the report chime, the UI confirmation tone, the power-down tone) are synthesised here
//! too, so the HUD board's sounds share the voice's level and speaker path.
//!
//! Everything is allocation-free per sample and unit-tested.

use std::f32::consts::{PI, TAU};

/// The rate the chain runs at (and the rate of the clause cache).
pub const MASTER_RATE: u32 = 48_000;
/// Stage 1: −0.55 semitones.
pub const PITCH_SEMITONES: f32 = -0.55;
/// Stage 6: the crossfade between consecutive clauses.
pub const CROSSFADE_MS: usize = 25;
/// Stage 6: the micro-pause after a vocative or a one-word opener.
pub const CUE_PAUSE_MS: usize = 70;

/// ×0.9687: the playback ratio of stage 1 (< 1 lowers the pitch).
pub fn pitch_ratio() -> f32 {
    2f32.powf(PITCH_SEMITONES / 12.0)
}

/// The Kokoro speed that yields `target` after stage 1 stretched the clause by `1 / pitch_ratio`.
pub fn synthesis_speed(target: f32) -> f32 {
    (target / pitch_ratio()).clamp(0.5, 2.2)
}

// ------------------------------------------------------------------------------ biquads ----

/// RBJ audio-EQ-cookbook biquad (transposed direct form II) with the filter shapes the chain
/// needs (the shared `dsp::Biquad` only has fixed-Q passes and the peaking bell).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bq {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Bq {
    fn new(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0, z1: 0.0, z2: 0.0 }
    }

    fn w0(freq: f32, rate: f32) -> (f32, f32) {
        let w0 = TAU * freq.min(rate * 0.49) / rate;
        (w0.cos(), w0.sin())
    }

    /// Second order: Q 0.7071 is a 12 dB/octave Butterworth.
    pub(crate) fn high_pass(freq: f32, q: f32, rate: f32) -> Self {
        let (cos, sin) = Self::w0(freq, rate);
        let alpha = sin / (2.0 * q.max(0.05));
        Self::new((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    /// Constant 0 dB peak gain band-pass.
    pub(crate) fn band_pass(freq: f32, q: f32, rate: f32) -> Self {
        let (cos, sin) = Self::w0(freq, rate);
        let alpha = sin / (2.0 * q.max(0.05));
        Self::new(alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    pub(crate) fn peaking(freq: f32, q: f32, gain_db: f32, rate: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let (cos, sin) = Self::w0(freq, rate);
        let alpha = sin / (2.0 * q.max(0.05));
        Self::new(1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a)
    }

    /// High shelf (shelf slope S = 1): `gain_db` above `freq`, half of it at `freq`.
    pub(crate) fn high_shelf(freq: f32, gain_db: f32, rate: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let (cos, sin) = Self::w0(freq, rate);
        let alpha = sin / 2.0 * 2f32.sqrt();
        let k = 2.0 * a.sqrt() * alpha;
        Self::new(
            a * ((a + 1.0) + (a - 1.0) * cos + k),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - k),
            (a + 1.0) - (a - 1.0) * cos + k,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - k,
        )
    }

    #[inline]
    pub(crate) fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    /// |H(f)| in dB (the tests check the curve).
    #[cfg(test)]
    fn response_db(&self, freq: f32, rate: f32) -> f32 {
        let w = TAU * freq / rate;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let num = ((self.b0 + self.b1 * c1 + self.b2 * c2).powi(2) + (self.b1 * s1 + self.b2 * s2).powi(2)).sqrt();
        let den = ((1.0 + self.a1 * c1 + self.a2 * c2).powi(2) + (self.a1 * s1 + self.a2 * s2).powi(2)).sqrt();
        20.0 * (num / den).log10()
    }
}

fn coefficient(ms: f32, rate: f32) -> f32 {
    (-1.0 / (ms.max(0.01) * 0.001 * rate)).exp()
}

pub(crate) fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub(crate) fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.max(1e-9).log10()
}

// ------------------------------------------------------------------------- the stages ----

/// Stage 1: the 24 kHz synthesis resampled to 48 kHz as if it had been recorded at
/// `from × pitch_ratio` (4-point Hermite), which lowers pitch and formants by 0.55 semitones and
/// stretches the clause by 3.2 %.
pub fn pitch_lock(input: &[f32], from: u32) -> Vec<f32> {
    if input.is_empty() || from == 0 {
        return Vec::new();
    }
    let source_rate = from as f64 * pitch_ratio() as f64;
    hermite_resample(input, source_rate / MASTER_RATE as f64)
}

/// `input` read `step` samples per output sample (4-point Hermite): < 1 lowers pitch and
/// formants and stretches the audio by `1 / step`.
pub fn hermite_resample(input: &[f32], step: f64) -> Vec<f32> {
    if input.is_empty() || step <= 0.0 {
        return Vec::new();
    }
    let count = (input.len() as f64 / step).floor() as usize;
    let last = input.len() as isize - 1;
    let sample = |i: isize| input[i.clamp(0, last) as usize];
    (0..count)
        .map(|i| {
            let pos = i as f64 * step;
            let k = pos.floor() as isize;
            let t = (pos - k as f64) as f32;
            let (a, b, c, d) = (sample(k - 1), sample(k), sample(k + 1), sample(k + 2));
            let c1 = 0.5 * (c - a);
            let c2 = a - 2.5 * b + 2.0 * c - 0.5 * d;
            let c3 = 0.5 * (d - a) + 1.5 * (b - c);
            ((c3 * t + c2) * t + c1) * t + b
        })
        .collect()
}

/// Stage 2: the five-band studio EQ and the dynamic de-esser.
struct StudioEq {
    high_pass: Bq,
    fundamental: Bq,
    mud: Bq,
    contour: Bq,
    air: Bq,
    /// Sibilance detector (a 6.4 kHz high-pass) and the band the de-esser takes out.
    ess_detect: Bq,
    ess_band: Bq,
    ess_env: f32,
    ess_attack: f32,
    ess_release: f32,
}

/// The EQ's bands: (frequency Hz, Q, gain dB).
pub const HIGH_PASS_HZ: f32 = 85.0;
pub const FUNDAMENTAL: (f32, f32, f32) = (135.0, 1.15, 2.2);
pub const MUD_CUT: (f32, f32, f32) = (420.0, 1.5, -2.5);
pub const CONTOUR: (f32, f32, f32) = (2850.0, 1.2, -0.8);
/// The condenser-air shelf: (corner Hz, gain dB).
pub const AIR_SHELF: (f32, f32) = (4200.0, 2.0);
pub const DE_ESS_HZ: f32 = 6400.0;
/// De-esser: sibilance above this level (linear envelope above 6.4 kHz) is turned down softly.
const ESS_THRESHOLD: f32 = 0.04;
/// At most −6 dB on the sibilant band.
const ESS_MAX_CUT: f32 = 0.5;

impl StudioEq {
    fn new(rate: f32) -> Self {
        Self {
            high_pass: Bq::high_pass(HIGH_PASS_HZ, std::f32::consts::FRAC_1_SQRT_2, rate),
            fundamental: Bq::peaking(FUNDAMENTAL.0, FUNDAMENTAL.1, FUNDAMENTAL.2, rate),
            mud: Bq::peaking(MUD_CUT.0, MUD_CUT.1, MUD_CUT.2, rate),
            contour: Bq::peaking(CONTOUR.0, CONTOUR.1, CONTOUR.2, rate),
            air: Bq::high_shelf(AIR_SHELF.0, AIR_SHELF.1, rate),
            ess_detect: Bq::high_pass(DE_ESS_HZ, 0.9, rate),
            // The sibilant region above the corner (≈ 6.4–9 kHz), in phase at its centre.
            ess_band: Bq::band_pass(7600.0, 0.8, rate),
            ess_env: 0.0,
            ess_attack: coefficient(1.0, rate),
            ess_release: coefficient(40.0, rate),
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.air.process(self.contour.process(self.mud.process(self.fundamental.process(self.high_pass.process(x)))));
        let level = self.ess_detect.process(y).abs();
        let c = if level > self.ess_env { self.ess_attack } else { self.ess_release };
        self.ess_env = level + c * (self.ess_env - level);
        let band = self.ess_band.process(y);
        // A soft knee: the cut grows smoothly from the threshold to twice it.
        let over = ((self.ess_env - ESS_THRESHOLD) / ESS_THRESHOLD).clamp(0.0, 1.0);
        y - over * over * (3.0 - 2.0 * over) * ESS_MAX_CUT * band
    }
}

/// A circular delay line read with linear interpolation.
struct DelayLine {
    buf: Vec<f32>,
    write: usize,
}

impl DelayLine {
    fn new(max_samples: usize) -> Self {
        Self { buf: vec![0.0; max_samples.max(4)], write: 0 }
    }

    #[inline]
    fn push(&mut self, x: f32) {
        self.buf[self.write] = x;
        self.write = (self.write + 1) % self.buf.len();
    }

    /// The sample `delay` samples ago (fractional).
    #[inline]
    fn read(&self, delay: f32) -> f32 {
        let n = self.buf.len();
        let d = delay.clamp(1.0, (n - 2) as f32);
        let whole = d.floor() as usize;
        let frac = d - whole as f32;
        let i0 = (self.write + n - whole) % n;
        let i1 = (i0 + n - 1) % n;
        self.buf[i0] * (1.0 - frac) + self.buf[i1] * frac
    }
}

/// Stage 3: the vocal booth's one tight reflection.
struct StudioRoom {
    line: DelayLine,
    delay: f32,
    /// The reflection is darker and thinner than the direct sound.
    band_hp: Bq,
    band_lp: Bq,
}

pub const ROOM_DELAY_MS: f32 = 2.2;
pub const DRY_MIX: f32 = 0.985;
pub const WET_MIX: f32 = 0.015;

impl StudioRoom {
    fn new(rate: f32) -> Self {
        Self {
            line: DelayLine::new((0.010 * rate) as usize),
            delay: ROOM_DELAY_MS * 0.001 * rate,
            band_hp: Bq::high_pass(250.0, std::f32::consts::FRAC_1_SQRT_2, rate),
            band_lp: Bq::band_pass(2500.0, 0.35, rate),
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        self.line.push(x);
        let wet = self.band_lp.process(self.band_hp.process(self.line.read(self.delay)));
        DRY_MIX * x + WET_MIX * wet
    }
}

/// Stage 4: the optical-style compressor and the soft-knee limiter.
pub struct Compressor {
    threshold_db: f32,
    ratio: f32,
    knee_db: f32,
    makeup: f32,
    attack: f32,
    release: f32,
    /// Smoothed gain reduction in dB (≤ 0).
    reduction_db: f32,
    /// Peak detector (linear).
    env: f32,
}

pub const COMP_THRESHOLD_DB: f32 = -14.0;
pub const COMP_RATIO: f32 = 3.2;
pub const COMP_MAKEUP_DB: f32 = 3.0;
pub const LIMIT_CEILING_DB: f32 = -1.0;

impl Compressor {
    pub fn new(rate: f32) -> Self {
        Self {
            threshold_db: COMP_THRESHOLD_DB,
            ratio: COMP_RATIO,
            knee_db: 6.0,
            makeup: db_to_gain(COMP_MAKEUP_DB),
            attack: coefficient(4.0, rate),
            release: coefficient(65.0, rate),
            reduction_db: 0.0,
            env: 0.0,
        }
    }

    /// The static curve: gain reduction (dB, ≤ 0) for an input level.
    pub fn curve_db(&self, level_db: f32) -> f32 {
        let over = level_db - self.threshold_db;
        let slope = 1.0 / self.ratio - 1.0;
        if 2.0 * over < -self.knee_db {
            0.0
        } else if 2.0 * over.abs() <= self.knee_db {
            slope * (over + self.knee_db / 2.0).powi(2) / (2.0 * self.knee_db)
        } else {
            slope * over
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let level = x.abs();
        // Instant peak attack, the release of the gain stage does the smoothing.
        self.env = if level > self.env { level } else { level + self.release * (self.env - level) };
        let target = self.curve_db(gain_to_db(self.env));
        let c = if target < self.reduction_db { self.attack } else { self.release };
        self.reduction_db = target + c * (self.reduction_db - target);
        limit(x * db_to_gain(self.reduction_db) * self.makeup)
    }
}

/// Soft-knee limiter at −1 dBFS: transparent below ~−4 dBFS, saturating smoothly to the ceiling.
#[inline]
pub fn limit(x: f32) -> f32 {
    let ceiling = db_to_gain(LIMIT_CEILING_DB);
    crate::dsp::soft_clip(x / ceiling) * ceiling
}

/// Stages 2-4 with their state (one per clause: the cache stores finished clauses).
pub struct JaiVoiceDsp {
    eq: StudioEq,
    room: StudioRoom,
    comp: Compressor,
}

impl JaiVoiceDsp {
    pub fn new(rate: u32) -> Self {
        let rate = rate.max(8_000) as f32;
        Self { eq: StudioEq::new(rate), room: StudioRoom::new(rate), comp: Compressor::new(rate) }
    }

    #[inline]
    pub fn process_sample(&mut self, x: f32) -> f32 {
        self.comp.process(self.room.process(self.eq.process(x)))
    }

    pub fn process(&mut self, audio: &mut [f32]) {
        for s in audio.iter_mut() {
            *s = self.process_sample(*s);
        }
    }
}

/// RMS the clause is brought to before the compressor (−21 dBFS: speech peaks sit around the
/// −14 dBFS threshold, so the compressor rides the peaks rather than the whole line).
const PRE_GAIN_RMS: f32 = 0.089;

/// The chain up to the reference transfer: the F0 flattening (`fish_zero_shot_tts::flatten_f0`,
/// 35 % by default), stage 1 (24 kHz → 48 kHz, −0.55 st), a loudness pre-gain and stages 2-4.
/// Returns 48 kHz audio.
pub fn master_dry(input: &[f32], from: u32, question: bool) -> Vec<f32> {
    // JAI's clauses are locked to his clip's median F0.
    master_dry_to(input, from, crate::fish_zero_shot_tts::profile().map(|p| p.f0.median_hz), question)
}

/// Whether a clause asks (its end rises: `fish_zero_shot_tts::QUESTION_RISE_ST`).
pub fn is_question(clause: &str) -> bool {
    clause.trim_end().trim_end_matches(['"', '\'', ')', '”']).ends_with('?')
}

/// `master_dry` with the F0 lock stated: `lock_to_hz` is the median F0 the finished clause
/// should have (the reference clip's), None for no lock. `question`: its end rises.
pub fn master_dry_to(input: &[f32], from: u32, lock_to_hz: Option<f32>, question: bool) -> Vec<f32> {
    // Stage 1 lowers the pitch after this stage: the lock aims that much higher.
    let target = lock_to_hz.filter(|hz| *hz > 0.0).map(|hz| hz / pitch_ratio());
    let rise = if question { crate::fish_zero_shot_tts::QUESTION_RISE_ST } else { 0.0 };
    let flat = crate::fish_zero_shot_tts::lock_f0(input, from, crate::fish_zero_shot_tts::f0_flatten(), target, rise);
    let mut audio = pitch_lock(&flat, from);
    let level = crate::dsp::rms(&audio);
    if level > 1e-4 {
        let gain = (PRE_GAIN_RMS / level).clamp(0.5, 2.2);
        audio.iter_mut().for_each(|s| *s *= gain);
    }
    JaiVoiceDsp::new(MASTER_RATE).process(&mut audio);
    audio
}

/// The whole chain for one clause: `master_dry` and the reference transfer (stage 5: the clip's
/// 128-band envelope and speech level). Returns 48 kHz audio.
pub fn master(input: &[f32], from: u32, question: bool) -> Vec<f32> {
    let mut audio = master_dry(input, from, question);
    crate::fish_zero_shot_tts::transfer_clause(&mut audio, MASTER_RATE);
    audio
}

/// Stage 6: a 70 ms micro-pause after the film's beats ("Sir,", "Right,", "Checking.", "On it, Sir.")
/// when a clause ends on one; None otherwise (the punctuation's own breath applies).
pub fn breath_cue_ms(clause: &str) -> Option<usize> {
    let t = clause.trim().trim_end_matches(['"', '\'', ')']).to_lowercase();
    const CUES: [&str; 8] = ["sir,", "right,", "right.", "checking.", "checking,", "on it, sir.", "on it, sir,", "on it."];
    CUES.contains(&t.as_str()).then_some(CUE_PAUSE_MS)
}

/// Stage 6: a leading vocative or opener ("Sir, the report's done.") split off so it gets its own
/// breath: (opener, rest). None when the clause does not start with one or nothing follows it.
pub fn split_leading_cue(clause: &str) -> Option<(String, String)> {
    let trimmed = clause.trim_start();
    let lower = trimmed.to_lowercase();
    for cue in ["on it, sir.", "checking.", "sir,", "right,"] {
        if lower.starts_with(cue) {
            let (head, rest) = trimmed.split_at(cue.len());
            let rest = rest.trim();
            if rest.chars().filter(|c| c.is_alphanumeric()).count() >= 3 {
                return Some((head.to_string(), rest.to_string()));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------- earcons ----

/// The HUD board's sounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Earcon {
    /// A report finished assembling: two crystal notes.
    ReportChime,
    /// "Maximise that": a crisp confirmation blip.
    Confirm,
    /// Sign-off: a soft descending power-down.
    PowerDown,
    /// A block materialises on the board: a short rising shimmer.
    Materialize,
    /// A block is maximised into the spotlight: an upward sweep that opens out.
    Spotlight,
    /// A telemetry alert (heat, memory, a crash): two urgent pips.
    Alert,
    /// A block leaves the board: the shimmer, falling.
    Dismiss,
    /// An alarm rings: up the chord, quick; played again and again until he stops it.
    Alarm,
}

impl Earcon {
    /// Every cue, for the sound check.
    pub const ALL: [Earcon; 8] = [Earcon::ReportChime, Earcon::Confirm, Earcon::PowerDown, Earcon::Materialize, Earcon::Spotlight, Earcon::Alert, Earcon::Dismiss, Earcon::Alarm];
}

/// A sine gliding from `from` to `to` Hz over the whole of `out[first..]`, with its own envelope
/// (`env(t)` for t in 0-1).
fn glide(out: &mut [f32], rate: f32, start: f32, secs: f32, from: f32, to: f32, amp: f32, env: impl Fn(f32) -> f32) {
    let first = (start * rate) as usize;
    let n = ((secs * rate) as usize).min(out.len().saturating_sub(first));
    let mut phase = 0.0f32;
    for i in 0..n {
        let t = i as f32 / n.max(1) as f32;
        let freq = from * (to / from).powf(t);
        phase = (phase + TAU * freq / rate).rem_euclid(TAU);
        out[first + i] += phase.sin() * amp * env(t);
    }
}

/// A partial that rings like glass: a sine with an exponential decay.
fn bell(out: &mut [f32], rate: f32, start: f32, freq: f32, amp: f32, decay: f32) {
    let first = (start * rate) as usize;
    for (i, s) in out.iter_mut().enumerate().skip(first) {
        let t = (i - first) as f32 / rate;
        let attack = (t / 0.004).min(1.0);
        *s += (TAU * freq * t).sin() * amp * attack * (-t * decay).exp();
    }
}

/// One note of the family every cue is made of: a struck glass (the fundamental, a soft
/// octave above it and the inharmonic partial of glass at ×2.76), ringing off at `decay`.
fn note(out: &mut [f32], rate: f32, start: f32, freq: f32, amp: f32, decay: f32) {
    bell(out, rate, start, freq, amp, decay);
    bell(out, rate, start, freq * 2.0, amp * 0.22, decay * 1.5);
    bell(out, rate, start, freq * 2.76, amp * 0.08, decay * 2.2);
}

/// The notes the cues are tuned to (one pentatonic scale, so any two of them agree): D5, F♯5,
/// A5, B5, D6.
const D5: f32 = 587.33;
const FS5: f32 = 739.99;
const A5: f32 = 880.0;
const B5: f32 = 987.77;
const D6: f32 = 1174.66;

/// The earcon at `rate` (mono, peak ≈ 0.08, raised-cosine tail). The seven are one family
/// (Sir, 2026-11-12: "the same seven remade so they belong together, softer and shorter"):
/// the same struck-glass voice, the same scale, a third of a second at most but for the
/// report's chime.
pub fn earcon(kind: Earcon, rate: u32) -> Vec<f32> {
    let r = rate.max(8_000) as f32;
    let secs = match kind {
        Earcon::ReportChime => 0.5,
        Earcon::Confirm => 0.12,
        Earcon::PowerDown => 0.42,
        Earcon::Materialize => 0.18,
        Earcon::Spotlight => 0.3,
        Earcon::Alert => 0.34,
        Earcon::Dismiss => 0.18,
        Earcon::Alarm => 0.46,
    };
    let mut out = vec![0.0f32; (r * secs) as usize];
    match kind {
        // A report is ready: a third, then the octave over it.
        Earcon::ReportChime => {
            note(&mut out, r, 0.0, FS5, 0.04, 9.0);
            note(&mut out, r, 0.0, A5, 0.04, 9.0);
            note(&mut out, r, 0.13, D6, 0.05, 8.0);
        }
        // Done: one clean tick.
        Earcon::Confirm => note(&mut out, r, 0.0, B5, 0.055, 36.0),
        // Signing off: down the chord.
        Earcon::PowerDown => {
            note(&mut out, r, 0.0, D6, 0.05, 14.0);
            note(&mut out, r, 0.09, A5, 0.05, 12.0);
            note(&mut out, r, 0.18, D5, 0.055, 9.0);
        }
        // A block arrives: a fourth up.
        Earcon::Materialize => {
            note(&mut out, r, 0.0, A5, 0.05, 30.0);
            note(&mut out, r, 0.07, D6, 0.055, 24.0);
        }
        // A block takes the spotlight: up the chord, the top note held.
        Earcon::Spotlight => {
            note(&mut out, r, 0.0, D5, 0.045, 26.0);
            note(&mut out, r, 0.06, A5, 0.045, 22.0);
            note(&mut out, r, 0.12, D6, 0.055, 9.0);
        }
        // Something is wrong: two level pips (the one cue that does not ring off).
        Earcon::Alert => {
            for start in [0.0, 0.17] {
                glide(&mut out, r, start, 0.11, A5, A5, 0.06, |t| (t * 30.0).min(1.0) * ((1.0 - t) * 30.0).min(1.0));
                glide(&mut out, r, start, 0.11, A5 * 2.0, A5 * 2.0, 0.013, |t| (t * 30.0).min(1.0) * ((1.0 - t) * 30.0).min(1.0));
            }
        }
        // A block leaves: the fourth, down.
        Earcon::Dismiss => {
            note(&mut out, r, 0.0, D6, 0.05, 34.0);
            note(&mut out, r, 0.07, A5, 0.055, 24.0);
        }
        // Time to get up: up the chord twice, quick and bright.
        Earcon::Alarm => {
            for start in [0.0, 0.21] {
                note(&mut out, r, start, D5, 0.06, 22.0);
                note(&mut out, r, start + 0.06, A5, 0.06, 22.0);
                note(&mut out, r, start + 0.12, D6, 0.065, 18.0);
            }
        }
    }
    let fade = (r * 0.03) as usize;
    let n = out.len();
    for i in 0..fade.min(n) {
        let w = 0.5 - 0.5 * (PI * i as f32 / fade as f32).cos();
        out[n - 1 - i] *= w;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: f32 = MASTER_RATE as f32;

    fn sine(freq: f32, secs: f32, amp: f32) -> Vec<f32> {
        (0..(R * secs) as usize).map(|i| (TAU * freq * i as f32 / R).sin() * amp).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        crate::dsp::rms(x)
    }

    #[test]
    fn pitch_lock_lowers_pitch_by_055_semitones_and_doubles_the_rate() {
        assert!((pitch_ratio() - 0.9687).abs() < 0.0005, "{}", pitch_ratio());
        // 1 s of 24 kHz audio → 48 kHz, 3.2 % longer.
        let input: Vec<f32> = (0..24_000).map(|i| (TAU * 1000.0 * i as f32 / 24_000.0).sin()).collect();
        let out = pitch_lock(&input, 24_000);
        let expected = 48_000.0 / pitch_ratio();
        assert!((out.len() as f32 - expected).abs() < 60.0, "{}", out.len());
        // Zero crossings: 1000 Hz becomes 968.7 Hz; a 124 Hz voice lands on ~120 Hz.
        let crossings = out.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f32;
        let hz = crossings / (out.len() as f32 / R);
        assert!((hz - 968.7).abs() < 3.0, "{hz}");
        assert!((124.0 * pitch_ratio() - 120.1).abs() < 0.5);
        // Speed compensation keeps the tempo at the requested, unhurried 0.98×.
        assert!((synthesis_speed(0.98) * pitch_ratio() - 0.98).abs() < 1e-4);
    }

    #[test]
    fn studio_eq_curve_matches_the_reference_spec() {
        let eq = StudioEq::new(R);
        let total = |f: f32| {
            eq.high_pass.response_db(f, R) + eq.fundamental.response_db(f, R) + eq.mud.response_db(f, R) + eq.contour.response_db(f, R) + eq.air.response_db(f, R)
        };
        // 12 dB/oct Butterworth: −3 dB at 85 Hz, about −12 dB an octave below.
        assert!((eq.high_pass.response_db(85.0, R) + 3.01).abs() < 0.1, "HPF −3 dB at 85 Hz");
        let octave_below = eq.high_pass.response_db(42.5, R);
        assert!(octave_below < -11.0 && octave_below > -13.5, "{octave_below}");
        assert!((eq.fundamental.response_db(135.0, R) - 2.2).abs() < 0.01);
        assert!((eq.mud.response_db(420.0, R) + 2.5).abs() < 0.01);
        assert!((eq.contour.response_db(2850.0, R) + 0.8).abs() < 0.01);
        // The shelf: half its gain at the corner, all of it in the air band.
        assert!((eq.air.response_db(4200.0, R) - 1.0).abs() < 0.15, "{}", eq.air.response_db(4200.0, R));
        assert!((eq.air.response_db(12_000.0, R) - 2.0).abs() < 0.2);
        assert!(eq.air.response_db(500.0, R).abs() < 0.1);
        assert!(total(1000.0).abs() < 0.6, "the midrange stays neutral: {}", total(1000.0));
    }

    #[test]
    fn de_esser_only_acts_on_loud_sibilance_above_64_khz() {
        let mut quiet = sine(7500.0, 0.3, 0.015);
        let mut loud = sine(7500.0, 0.3, 0.4);
        let (q0, l0) = (rms(&quiet), rms(&loud));
        let mut eq = StudioEq::new(R);
        quiet.iter_mut().for_each(|s| *s = eq.process(*s));
        let mut eq = StudioEq::new(R);
        loud.iter_mut().for_each(|s| *s = eq.process(*s));
        let quiet_db = gain_to_db(rms(&quiet[4800..]) / q0);
        let loud_db = gain_to_db(rms(&loud[4800..]) / l0);
        assert!(quiet_db > 1.2, "quiet sibilance keeps the air lift: {quiet_db}");
        assert!(loud_db < quiet_db - 3.0, "loud sibilance tamed: {loud_db} vs {quiet_db}");
        // A loud vowel at 300 Hz is no sibilance.
        let mut vowel = sine(300.0, 0.3, 0.4);
        let v0 = rms(&vowel);
        let mut eq = StudioEq::new(R);
        vowel.iter_mut().for_each(|s| *s = eq.process(*s));
        assert!(gain_to_db(rms(&vowel[4800..]) / v0).abs() < 1.0);
    }

    #[test]
    fn studio_room_is_one_dry_2_2_ms_reflection_at_1_5_percent() {
        assert!(WET_MIX <= 0.015 && (DRY_MIX + WET_MIX - 1.0).abs() < 1e-6);
        let mut room = StudioRoom::new(R);
        let mut out = vec![0.0f32; 960];
        for (i, s) in out.iter_mut().enumerate() {
            *s = room.process(if i == 0 { 1.0 } else { 0.0 });
        }
        assert!((out[0] - DRY_MIX).abs() < 1e-6, "the direct sound");
        let tap = (ROOM_DELAY_MS * 0.001 * R) as usize;
        let early: f32 = out[1..tap - 2].iter().map(|x| x.abs()).fold(0.0, f32::max);
        let late: f32 = out[tap - 2..tap + 120].iter().map(|x| x.abs()).fold(0.0, f32::max);
        assert!(early < 1e-6, "nothing before the reflection: {early}");
        assert!(late > 1e-4 && late < WET_MIX, "one quiet reflection: {late}");
        // No chorus: a steady tone comes out at a steady level (no detune beating).
        let mut room = StudioRoom::new(R);
        let mut tone = sine(1000.0, 1.0, 0.5);
        tone.iter_mut().for_each(|s| *s = room.process(*s));
        let windows: Vec<f32> = tone[4800..].chunks(2400).map(rms).collect();
        let (lo, hi) = windows.iter().fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        assert!(gain_to_db(hi / lo) < 0.05, "steady: {lo}..{hi}");
    }

    #[test]
    fn compressor_holds_threshold_ratio_and_makeup() {
        let c = Compressor::new(R);
        assert_eq!(c.curve_db(-30.0), 0.0);
        // 10 dB over: 10 / 3.2 out, so −6.875 dB of reduction.
        assert!((c.curve_db(-4.0) + 10.0 * (1.0 - 1.0 / 3.2)).abs() < 0.01);
        // Soft knee: a little reduction just under the threshold.
        assert!(c.curve_db(-15.0) < 0.0 && c.curve_db(-15.0) > -1.0);
        // A quiet line gets the +3 dB make-up, a loud one is held down.
        let mut quiet = sine(500.0, 0.4, 0.05);
        let q0 = rms(&quiet);
        let mut c = Compressor::new(R);
        quiet.iter_mut().for_each(|s| *s = c.process(*s));
        assert!((gain_to_db(rms(&quiet[4800..]) / q0) - 3.0).abs() < 0.3);
        let mut loud = sine(500.0, 0.4, 0.9);
        let l0 = rms(&loud);
        let mut c = Compressor::new(R);
        loud.iter_mut().for_each(|s| *s = c.process(*s));
        assert!(gain_to_db(rms(&loud[9600..]) / l0) < -3.0);
        assert!(loud.iter().all(|s| s.abs() <= db_to_gain(LIMIT_CEILING_DB) + 1e-4), "the limiter holds −1 dBFS");
    }

    #[test]
    fn master_chain_is_finite_bounded_and_at_48k() {
        let input: Vec<f32> = (0..12_000)
            .map(|i| {
                let t = i as f32 / 24_000.0;
                ((TAU * 124.0 * t).sin() * 0.3 + (TAU * 2600.0 * t).sin() * 0.05) * (1.0 - (TAU * 3.0 * t).cos()) * 0.5
            })
            .collect();
        let out = master(&input, 24_000, false);
        let expected = 24_000.0 / pitch_ratio();
        assert!((out.len() as f32 - expected).abs() < 40.0, "{}", out.len());
        assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 0.9));
        let level = rms(&out);
        assert!(level > 0.04 && level < 0.25, "{level}");
    }

    #[test]
    fn breath_cues_and_leading_vocatives() {
        assert_eq!(breath_cue_ms("On it, Sir."), Some(CUE_PAUSE_MS));
        assert_eq!(breath_cue_ms("Checking."), Some(CUE_PAUSE_MS));
        assert_eq!(breath_cue_ms("Right,"), Some(CUE_PAUSE_MS));
        assert_eq!(breath_cue_ms("The report is done."), None);
        assert_eq!(breath_cue_ms("I have checked everything, Sir,"), None);
        assert_eq!(split_leading_cue("Sir, the report's done."), Some(("Sir,".into(), "the report's done.".into())));
        assert_eq!(split_leading_cue("Right, two things."), Some(("Right,".into(), "two things.".into())));
        assert_eq!(split_leading_cue("Sir,"), None);
        assert_eq!(split_leading_cue("Sirius is a star."), None);
    }

    #[test]
    fn earcons_are_short_quiet_and_end_in_silence() {
        for kind in Earcon::ALL {
            let e = earcon(kind, 48_000);
            assert!(!e.is_empty() && e.len() < 48_000);
            let peak = e.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.02 && peak < 0.2, "{kind:?} peak {peak}");
            assert!(e.last().unwrap().abs() < 1e-3);
            assert!(e.iter().all(|s| s.is_finite()), "{kind:?}");
        }
    }

    /// The share of a cue's energy that sits in its second half.
    fn late_share(e: &[f32]) -> f32 {
        let half = e.len() / 2;
        let energy = |x: &[f32]| x.iter().map(|s| s * s).sum::<f32>();
        energy(&e[half..]) / energy(e).max(1e-12)
    }

    /// Zero crossings per second over `e[from..to]` (fractions of the cue): a rough pitch.
    fn crossings(e: &[f32], from: f32, to: f32) -> f32 {
        let (a, b) = ((e.len() as f32 * from) as usize, (e.len() as f32 * to) as usize);
        let n = e[a..b].windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f32;
        n / ((b - a) as f32 / 48_000.0)
    }

    /// One family: none longer than half a second, none louder than a tenth of full scale, and
    /// softer and shorter than the set they replace (which peaked at 0.12 and ran to 0.9 s).
    #[test]
    fn the_cues_are_one_soft_short_family() {
        for kind in Earcon::ALL {
            let e = earcon(kind, 48_000);
            let peak = e.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak <= 0.1, "{kind:?} peaks at {peak}");
            assert!(e.len() <= 24_000, "{kind:?} lasts {} ms", e.len() / 48);
        }
        // Every pitch is on the one scale.
        for f in [D5, FS5, A5, B5, D6] {
            let steps = 12.0 * (f / D5).log2();
            assert!((steps - steps.round()).abs() < 0.02 && [0, 4, 7, 9, 12].contains(&(steps.round() as i32)), "{f}");
        }
    }

    #[test]
    fn every_cue_is_its_own_sound_at_every_device_rate() {
        // A block arriving rises, a block leaving falls.
        let (arrive, leave) = (earcon(Earcon::Materialize, 48_000), earcon(Earcon::Dismiss, 48_000));
        assert!(crossings(&arrive, 0.05, 0.3) < crossings(&arrive, 0.45, 0.7), "materialise rises");
        assert!(crossings(&leave, 0.05, 0.3) > crossings(&leave, 0.55, 0.8), "dismiss falls");
        // The alert is two pips with silence between them.
        let alert = earcon(Earcon::Alert, 48_000);
        let gap = &alert[(0.125 * 48_000.0) as usize..(0.16 * 48_000.0) as usize];
        assert!(rms(gap) < 0.002, "silence between the pips: {}", rms(gap));
        assert!(rms(&alert[(0.2 * 48_000.0) as usize..(0.26 * 48_000.0) as usize]) > 0.02, "the second pip");
        // The spotlight opens out: it still rings in its second half.
        assert!(late_share(&earcon(Earcon::Spotlight, 48_000)) > 0.2);
        // No two cues are the same samples, and every one scales with the device rate.
        for (i, a) in Earcon::ALL.iter().enumerate() {
            for b in &Earcon::ALL[i + 1..] {
                assert_ne!(earcon(*a, 48_000), earcon(*b, 48_000), "{a:?} vs {b:?}");
            }
            for rate in [16_000u32, 44_100, 96_000] {
                let e = earcon(*a, rate);
                let secs = e.len() as f32 / rate as f32;
                assert!((secs - earcon(*a, 48_000).len() as f32 / 48_000.0).abs() < 0.01, "{a:?} at {rate} Hz lasts {secs} s");
                assert!(e.iter().all(|s| s.is_finite() && s.abs() < 0.2));
            }
        }
    }
}
