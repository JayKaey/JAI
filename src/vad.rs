//! Voice activity detection on 512-sample (32 ms @ 16 kHz) frames.
//!
//! * [`SileroVad`]: Silero VAD v5 ONNX (64-sample context + 512-sample frame, recurrent state).
//! * [`VoiceBand`]: the energy of the 100 Hz – 3.5 kHz band, where a voice carries its level; the
//!   energy gate and its noise floor follow it, so key clicks (above the band) and fan rumble or
//!   desk bumps (below it) weigh far less than they do in the full band.
//! * [`VadMachine`]: pure, unit-tested state machine
//!   `Calibrating → Idle → PreSpeech → Speaking ⇄ Hangover → (finalised) → Idle`
//!   with a dual probability threshold, an adaptive noise floor, a pre-roll buffer, a hard length
//!   cap, raised-threshold barge-in while JAI is talking, and push-to-talk.
//!
//!   The end of speech is decided in two steps. After a first, short silence (160 ms behind a
//!   one-word reflex, 220 ms otherwise) the words so far are handed out (`SpeechPaused`) so that
//!   Whisper can start on them; the utterance only ends after the full silence (280 ms behind a
//!   short command of up to 1.1 s of voice, 450 ms behind a sentence). When Sir carries on in
//!   between (`SpeechResumed`) the head start is dropped and nothing was cut; when he does not,
//!   the transcript is ready that much earlier.

use crate::dsp::Biquad;
use ort::session::Session;
use ort::value::Tensor;
use std::collections::VecDeque;
use std::path::Path;
use tracing::{info, warn};

pub const SAMPLE_RATE: u32 = 16_000;
pub const FRAME: usize = 512;
pub const FRAME_MS: u32 = 32;
const CONTEXT: usize = 64;

const MIN_FLOOR: f32 = 0.0004;
const MAX_FLOOR: f32 = 0.08;
/// Frames of trailing silence kept on the utterance (the rest of the hangover is trimmed).
const KEEP_TAIL_FRAMES: usize = 3;

pub struct SileroVad {
    session: Session,
    state: Vec<f32>,
    context: [f32; CONTEXT],
    failures: u32,
}

impl SileroVad {
    pub fn load(path: &Path) -> Result<Self, String> {
        let env = crate::config::ort_env().map_err(|e| e.to_string())?;
        let session = Session::builder(env)
            .map_err(|e| e.to_string())?
            .with_intra_threads(1)
            .unwrap_or_else(|e| e.recover())
            .with_inter_threads(1)
            .unwrap_or_else(|e| e.recover())
            .commit_from_file(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        info!("[VAD] Silero VAD v5 loaded from {}", path.display());
        Ok(Self {
            session,
            state: vec![0.0; 2 * 128],
            context: [0.0; CONTEXT],
            failures: 0,
        })
    }

    pub fn reset(&mut self) {
        self.state.iter_mut().for_each(|v| *v = 0.0);
        self.context = [0.0; CONTEXT];
    }

    /// Speech probability for one 512-sample frame.
    pub fn predict(&mut self, frame: &[f32]) -> f32 {
        match self.run(frame) {
            Ok(p) => {
                self.failures = 0;
                p
            }
            Err(e) => {
                self.failures += 1;
                if self.failures == 1 || self.failures.is_multiple_of(500) {
                    warn!("[VAD] Silero inference failed ({} times): {e}", self.failures);
                }
                0.0
            }
        }
    }

    fn run(&mut self, frame: &[f32]) -> ort::Result<f32> {
        let mut input = Vec::with_capacity(CONTEXT + FRAME);
        input.extend_from_slice(&self.context);
        input.extend_from_slice(frame);
        self.context.copy_from_slice(&frame[FRAME - CONTEXT..]);

        let input = Tensor::from_array(([1usize, CONTEXT + FRAME], input))?;
        let state = Tensor::from_array(([2usize, 1, 128], self.state.clone()))?;
        let sr = Tensor::from_array(([0usize; 0], vec![SAMPLE_RATE as i64]))?;
        let outputs = self.session.run(ort::inputs!["input" => input, "state" => state, "sr" => sr])?;
        let prob = outputs[0].try_extract_tensor::<f32>()?.1.first().copied().unwrap_or(0.0);
        let (_, new_state) = outputs[1].try_extract_tensor::<f32>()?;
        if new_state.len() == self.state.len() {
            self.state.copy_from_slice(new_state);
        }
        Ok(prob)
    }
}

/// The voice band's energy, frame by frame (the filters keep their state across frames).
pub struct VoiceBand {
    high_pass: [Biquad; 2],
    low_pass: [Biquad; 2],
}

/// Below this is rumble: fans, desk bumps, the hum of a PC.
pub const VOICE_BAND_LOW_HZ: f32 = 100.0;
/// Above this a voice has little level left; key clicks and hiss have most of theirs.
pub const VOICE_BAND_HIGH_HZ: f32 = 3500.0;

impl VoiceBand {
    pub fn new() -> Self {
        let rate = SAMPLE_RATE as f32;
        Self {
            high_pass: [Biquad::high_pass(VOICE_BAND_LOW_HZ, rate), Biquad::high_pass(VOICE_BAND_LOW_HZ, rate)],
            low_pass: [Biquad::low_pass(VOICE_BAND_HIGH_HZ, rate), Biquad::low_pass(VOICE_BAND_HIGH_HZ, rate)],
        }
    }

    /// The RMS of `frame` inside the band (24 dB/octave on both sides).
    pub fn rms(&mut self, frame: &[f32]) -> f32 {
        if frame.is_empty() {
            return 0.0;
        }
        let mut sum = 0.0f64;
        for &x in frame {
            let mut y = x;
            for stage in &mut self.high_pass {
                y = stage.process(y);
            }
            for stage in &mut self.low_pass {
                y = stage.process(y);
            }
            sum += (y as f64) * (y as f64);
        }
        (sum / frame.len() as f64).sqrt() as f32
    }
}

impl Default for VoiceBand {
    fn default() -> Self {
        Self::new()
    }
}

/// Energy-only probability used when the Silero model cannot be loaded.
pub fn energy_probability(rms: f32, noise_floor: f32) -> f32 {
    let snr = rms / noise_floor.max(MIN_FLOOR);
    ((snr - 1.5) / 4.0).clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VadParams {
    pub start_threshold: f32,
    pub release_threshold: f32,
    pub noise_multiplier: f32,
    pub start_frames: u32,
    /// Trailing silent frames that close a sentence.
    pub hangover_frames: u32,
    /// Trailing silent frames after which the words so far are handed out for a head start.
    pub short_hangover_frames: u32,
    /// Trailing silent frames that close a short command burst.
    pub short_commit_frames: u32,
    /// Up to this many voiced frames an utterance is a short command burst.
    pub short_burst_frames: u32,
    /// The head start behind a one-word reflex burst ("mute", "stop").
    pub reflex_hangover_frames: u32,
    /// A reflex burst has at least / at most this many voiced frames.
    pub reflex_min_frames: u32,
    pub reflex_max_frames: u32,
    pub pre_roll_frames: usize,
    pub min_speech_frames: u32,
    pub max_frames: u32,
    pub calibration_frames: usize,
    pub barge_in_enabled: bool,
    pub barge_in_frames: u32,
    pub barge_in_boost: f32,
}

fn ms_to_frames(ms: u32) -> u32 {
    ms.div_ceil(FRAME_MS)
}

impl VadParams {
    pub fn from_config(cfg: &crate::config::JaiConfig) -> Self {
        let mut p = Self {
            start_threshold: 0.5,
            release_threshold: 0.35,
            noise_multiplier: 2.2,
            start_frames: cfg.vad_start_frames.max(1),
            hangover_frames: 11,
            short_hangover_frames: ms_to_frames(cfg.short_command_hangover_ms).max(1),
            short_commit_frames: ms_to_frames(cfg.short_command_commit_ms).max(1),
            short_burst_frames: cfg.short_command_max_ms / FRAME_MS,
            reflex_hangover_frames: ms_to_frames(cfg.reflex_hangover_ms).max(1),
            reflex_min_frames: ms_to_frames(cfg.reflex_min_ms),
            reflex_max_frames: cfg.reflex_max_ms / FRAME_MS,
            pre_roll_frames: ms_to_frames(cfg.pre_roll_ms) as usize,
            min_speech_frames: ms_to_frames(cfg.min_speech_ms).max(1),
            max_frames: ms_to_frames(cfg.max_utterance_secs * 1000),
            calibration_frames: ms_to_frames(cfg.noise_calibration_ms).max(4) as usize,
            barge_in_enabled: cfg.barge_in_enabled,
            barge_in_frames: ms_to_frames(cfg.barge_in_min_ms).max(2),
            barge_in_boost: cfg.barge_in_threshold_boost,
        };
        p.apply_live(
            cfg.vad_start_threshold,
            cfg.vad_release_threshold,
            cfg.vad_noise_multiplier,
            cfg.silence_hangover_ms,
        );
        p
    }

    /// Values tunable at runtime from the UI sliders.
    pub fn apply_live(&mut self, start: f32, release: f32, noise_multiplier: f32, hangover_ms: u32) {
        self.start_threshold = start.clamp(0.05, 0.98);
        self.release_threshold = release.clamp(0.02, self.start_threshold);
        self.noise_multiplier = noise_multiplier.clamp(1.0, 10.0);
        self.hangover_frames = ms_to_frames(hangover_ms).max(1);
    }

    /// The trailing silence that ends an utterance with `voiced_frames` of speech so far: a
    /// short burst of up to 1.1 s of voice ("stop", "JAI, next page") after 280 ms, a
    /// sentence after the normal hangover.
    pub fn end_of_speech_frames(&self, voiced_frames: u32) -> u32 {
        if voiced_frames <= self.short_burst_frames {
            self.short_commit_frames.min(self.hangover_frames)
        } else {
            self.hangover_frames
        }
    }

    /// The trailing silence after which the words so far are handed out for a head start
    /// (`SpeechPaused`): 160 ms behind a one-word reflex (150-850 ms of voice), 220 ms
    /// otherwise. Always before the utterance could end.
    pub fn early_frames(&self, voiced_frames: u32) -> u32 {
        let early = if (self.reflex_min_frames..=self.reflex_max_frames).contains(&voiced_frames) {
            self.reflex_hangover_frames.min(self.short_hangover_frames)
        } else {
            self.short_hangover_frames
        };
        early.min(self.end_of_speech_frames(voiced_frames).saturating_sub(1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadPhase {
    Calibrating,
    Idle,
    PreSpeech,
    Speaking,
    Hangover,
}

impl VadPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Calibrating => "calibrating",
            Self::Idle => "idle",
            Self::PreSpeech => "pre_speech",
            Self::Speaking => "speaking",
            Self::Hangover => "hangover",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Silence,
    MaxLength,
    PttReleased,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameContext {
    /// JAI is speaking (or its echo tail is still in the room): use barge-in thresholds.
    pub assistant_speaking: bool,
    pub ptt: bool,
    /// e.g. while the microphone test records: track noise, never open utterances.
    pub suspended: bool,
}

#[derive(Debug)]
pub enum VadEvent {
    Calibrated { noise_floor: f32 },
    SpeechStarted { barge_in: bool, ptt: bool },
    /// A pause that may be the end: the utterance so far, exactly as `SpeechEnded` will hand it
    /// out if nothing more is said.
    SpeechPaused { pcm: Vec<f32>, speech_ms: u32 },
    /// Sir carried on after a `SpeechPaused`: the head start is void.
    SpeechResumed,
    SpeechEnded {
        pcm: Vec<f32>,
        speech_ms: u32,
        silence_ms: u32,
        reason: EndReason,
        barge_in: bool,
        ptt: bool,
    },
    Discarded { speech_ms: u32 },
}

pub struct VadMachine {
    pub params: VadParams,
    phase: VadPhase,
    noise_floor: f32,
    calibration: Vec<f32>,
    pre_roll: VecDeque<Vec<f32>>,
    utterance: Vec<f32>,
    run: u32,
    voiced_frames: u32,
    frames: u32,
    barge_in: bool,
    ptt_utterance: bool,
    prev_ptt: bool,
    /// The current pause was handed out as `SpeechPaused`.
    paused: bool,
}

impl VadMachine {
    pub fn new(params: VadParams) -> Self {
        Self {
            params,
            phase: VadPhase::Calibrating,
            noise_floor: 0.003,
            calibration: Vec::with_capacity(params.calibration_frames),
            pre_roll: VecDeque::with_capacity(params.pre_roll_frames + 1),
            utterance: Vec::new(),
            run: 0,
            voiced_frames: 0,
            frames: 0,
            barge_in: false,
            ptt_utterance: false,
            prev_ptt: false,
            paused: false,
        }
    }

    pub fn phase(&self) -> VadPhase {
        self.phase
    }

    pub fn noise_floor(&self) -> f32 {
        self.noise_floor
    }

    pub fn start_gate(&self) -> f32 {
        self.noise_floor * self.params.noise_multiplier
    }

    fn release_gate(&self) -> f32 {
        self.noise_floor * (self.params.noise_multiplier * 0.6).max(1.25)
    }

    pub fn in_utterance(&self) -> bool {
        matches!(self.phase, VadPhase::Speaking | VadPhase::Hangover) || self.ptt_utterance
    }

    /// Restart noise-floor calibration (device change or user request).
    pub fn recalibrate(&mut self) {
        self.reset();
        self.calibration.clear();
        self.phase = VadPhase::Calibrating;
    }

    /// Drops any partial utterance (e.g. microphone muted).
    pub fn reset(&mut self) {
        self.phase = if self.phase == VadPhase::Calibrating { VadPhase::Calibrating } else { VadPhase::Idle };
        self.pre_roll.clear();
        self.utterance.clear();
        self.run = 0;
        self.voiced_frames = 0;
        self.frames = 0;
        self.barge_in = false;
        self.ptt_utterance = false;
        self.paused = false;
    }

    /// The utterance so far without the silence of the current pause beyond the kept tail: what
    /// `finalize` hands out if nothing more is said.
    fn heard_so_far(&self) -> Vec<f32> {
        let trim = (self.run as usize).saturating_sub(KEEP_TAIL_FRAMES) * FRAME;
        self.utterance[..self.utterance.len().saturating_sub(trim)].to_vec()
    }

    fn push_pre_roll(&mut self, frame: &[f32]) {
        if self.params.pre_roll_frames == 0 {
            return;
        }
        if self.pre_roll.len() >= self.params.pre_roll_frames {
            self.pre_roll.pop_front();
        }
        self.pre_roll.push_back(frame.to_vec());
    }

    fn begin_utterance(&mut self, barge_in: bool, ptt: bool) {
        self.utterance.clear();
        for f in self.pre_roll.drain(..) {
            self.utterance.extend_from_slice(&f);
        }
        self.frames = (self.utterance.len() / FRAME) as u32;
        self.voiced_frames = if ptt { 0 } else { self.run };
        self.run = 0;
        self.barge_in = barge_in;
        self.ptt_utterance = ptt;
        self.phase = VadPhase::Speaking;
    }

    fn adapt_floor(&mut self, rms: f32) {
        let rate = if rms < self.noise_floor { 0.05 } else { 0.004 };
        self.noise_floor = (self.noise_floor + (rms - self.noise_floor) * rate).clamp(MIN_FLOOR, MAX_FLOOR);
    }

    fn finalize(&mut self, reason: EndReason, silent_frames: u32) -> VadEvent {
        if reason == EndReason::Silence {
            let trim = (silent_frames as usize).saturating_sub(KEEP_TAIL_FRAMES) * FRAME;
            let keep = self.utterance.len().saturating_sub(trim);
            self.utterance.truncate(keep);
        }
        let speech_ms = self.voiced_frames * FRAME_MS;
        let duration_ms = (self.utterance.len() / FRAME) as u32 * FRAME_MS;
        let ptt = self.ptt_utterance;
        let barge_in = self.barge_in;
        let pcm = std::mem::take(&mut self.utterance);
        self.phase = VadPhase::Idle;
        self.run = 0;
        self.voiced_frames = 0;
        self.frames = 0;
        self.barge_in = false;
        self.ptt_utterance = false;
        self.paused = false;
        self.pre_roll.clear();

        let too_short = if ptt {
            duration_ms < 300
        } else {
            speech_ms < self.params.min_speech_frames * FRAME_MS
        };
        if too_short {
            return VadEvent::Discarded { speech_ms };
        }
        VadEvent::SpeechEnded {
            pcm,
            speech_ms,
            silence_ms: silent_frames * FRAME_MS,
            reason,
            barge_in,
            ptt,
        }
    }

    /// Feeds one frame (+ its Silero probability and RMS). Returns at most one event.
    pub fn process(&mut self, frame: &[f32], prob: f32, rms: f32, ctx: FrameContext) -> Option<VadEvent> {
        // ---- push-to-talk overrides voice detection entirely ----------------------------
        if ctx.ptt && !self.prev_ptt {
            self.prev_ptt = true;
            if self.phase == VadPhase::Calibrating {
                self.finish_calibration();
            }
            self.push_pre_roll(frame);
            self.begin_utterance(ctx.assistant_speaking, true);
            return Some(VadEvent::SpeechStarted { barge_in: ctx.assistant_speaking, ptt: true });
        }
        if !ctx.ptt && self.prev_ptt {
            self.prev_ptt = false;
            if self.ptt_utterance {
                self.utterance.extend_from_slice(frame);
                return Some(self.finalize(EndReason::PttReleased, 0));
            }
        }
        if self.ptt_utterance {
            self.utterance.extend_from_slice(frame);
            self.frames += 1;
            if prob >= self.params.release_threshold {
                self.voiced_frames += 1;
            }
            if self.frames >= self.params.max_frames {
                return Some(self.finalize(EndReason::MaxLength, 0));
            }
            return None;
        }

        let barge_mode = ctx.assistant_speaking && self.params.barge_in_enabled;
        let voiced = if barge_mode {
            prob >= (self.params.start_threshold + self.params.barge_in_boost).min(0.95)
                && rms >= self.start_gate() * 1.5
        } else {
            prob >= self.params.start_threshold && rms >= self.start_gate()
        };
        let silent = prob < self.params.release_threshold || rms < self.release_gate();

        match self.phase {
            VadPhase::Calibrating => {
                self.push_pre_roll(frame);
                self.calibration.push(rms);
                if self.calibration.len() >= self.params.calibration_frames {
                    let floor = self.finish_calibration();
                    return Some(VadEvent::Calibrated { noise_floor: floor });
                }
                None
            }
            VadPhase::Idle | VadPhase::PreSpeech => {
                self.push_pre_roll(frame);
                if ctx.suspended {
                    self.phase = VadPhase::Idle;
                    self.run = 0;
                    if !ctx.assistant_speaking && prob < 0.25 {
                        self.adapt_floor(rms);
                    }
                    return None;
                }
                if ctx.assistant_speaking && !self.params.barge_in_enabled {
                    self.phase = VadPhase::Idle;
                    self.run = 0;
                    return None;
                }
                if voiced {
                    self.run += 1;
                    self.phase = VadPhase::PreSpeech;
                    let needed = if barge_mode { self.params.barge_in_frames } else { self.params.start_frames };
                    if self.run >= needed {
                        self.begin_utterance(barge_mode, false);
                        return Some(VadEvent::SpeechStarted { barge_in: barge_mode, ptt: false });
                    }
                } else {
                    self.phase = VadPhase::Idle;
                    self.run = 0;
                    if !ctx.assistant_speaking && prob < 0.25 {
                        self.adapt_floor(rms);
                    }
                }
                None
            }
            VadPhase::Speaking | VadPhase::Hangover => {
                self.utterance.extend_from_slice(frame);
                self.frames += 1;
                let mut event = None;
                if silent {
                    self.run = if self.phase == VadPhase::Hangover { self.run + 1 } else { 1 };
                    self.phase = VadPhase::Hangover;
                    if self.run >= self.params.end_of_speech_frames(self.voiced_frames) {
                        let silent_frames = self.run;
                        return Some(self.finalize(EndReason::Silence, silent_frames));
                    }
                    // A pause that may be the end: the words so far go out for a head start.
                    let early = self.params.early_frames(self.voiced_frames);
                    if !self.paused && early > 0 && self.run == early && self.voiced_frames >= self.params.min_speech_frames {
                        self.paused = true;
                        event = Some(VadEvent::SpeechPaused { pcm: self.heard_so_far(), speech_ms: self.voiced_frames * FRAME_MS });
                    }
                } else {
                    // Brief pause (< hangover) followed by voice: keep recording.
                    if std::mem::take(&mut self.paused) {
                        event = Some(VadEvent::SpeechResumed);
                    }
                    self.phase = VadPhase::Speaking;
                    self.run = 0;
                    self.voiced_frames += 1;
                }
                if self.frames >= self.params.max_frames {
                    let silent_frames = if self.phase == VadPhase::Hangover { self.run } else { 0 };
                    return Some(self.finalize(EndReason::MaxLength, silent_frames));
                }
                event
            }
        }
    }

    fn finish_calibration(&mut self) -> f32 {
        if !self.calibration.is_empty() {
            let mut sorted = self.calibration.clone();
            sorted.sort_by(|a, b| a.total_cmp(b));
            // 30th percentile: robust against a word spoken during calibration.
            let p30 = sorted[(sorted.len() as f32 * 0.3) as usize];
            self.noise_floor = p30.clamp(MIN_FLOOR, MAX_FLOOR);
        }
        self.calibration.clear();
        self.phase = VadPhase::Idle;
        self.run = 0;
        self.noise_floor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The configuration defaults are exactly the spec values (0.50/0.35, ×2.2, 2 frames,
    /// 450 ms sentence hangover, 280 ms after a ≤ 1.1 s command with a head start after 160 /
    /// 220 ms, 500 ms pre-roll, 20 s cap, 160 ms barge-in).
    fn params() -> VadParams {
        VadParams::from_config(&crate::config::JaiConfig::default())
    }

    fn calibrated() -> VadMachine {
        let mut m = VadMachine::new(params());
        let frame = vec![0.0f32; FRAME];
        let mut calibrated = false;
        for _ in 0..m.params.calibration_frames {
            if let Some(VadEvent::Calibrated { noise_floor }) = m.process(&frame, 0.02, 0.002, FrameContext::default()) {
                assert!((noise_floor - 0.002).abs() < 1e-6);
                calibrated = true;
            }
        }
        assert!(calibrated);
        m
    }

    const SPEECH: (f32, f32) = (0.92, 0.05);
    const SILENCE: (f32, f32) = (0.03, 0.002);

    fn feed(m: &mut VadMachine, n: usize, (p, r): (f32, f32), ctx: FrameContext) -> Vec<VadEvent> {
        let frame = vec![0.01f32; FRAME];
        (0..n).filter_map(|_| m.process(&frame, p, r, ctx)).collect()
    }

    #[test]
    fn frame_math_matches_spec() {
        let p = params();
        assert_eq!(p.hangover_frames, 15, "450 ms of silence = 15 frames");
        assert_eq!(p.short_hangover_frames, 7, "220 ms = 7 frames");
        assert_eq!(p.short_commit_frames, 9, "280 ms = 9 frames");
        assert_eq!(p.short_burst_frames, 34, "1.1 s of voice");
        assert_eq!((p.reflex_hangover_frames, p.reflex_min_frames, p.reflex_max_frames), (5, 5, 26), "160 ms after 150-850 ms of voice");
        // The end: 288 ms behind a short burst, 480 ms behind a sentence.
        assert_eq!(
            (p.end_of_speech_frames(4), p.end_of_speech_frames(10), p.end_of_speech_frames(26), p.end_of_speech_frames(27), p.end_of_speech_frames(34), p.end_of_speech_frames(35)),
            (9, 9, 9, 9, 9, 15)
        );
        // The head start: 160 ms behind a reflex word, 224 ms otherwise, always before the end.
        assert_eq!((p.early_frames(4), p.early_frames(10), p.early_frames(26), p.early_frames(27), p.early_frames(34), p.early_frames(35)), (7, 5, 5, 7, 7, 7));
        let mut hasty = p;
        hasty.apply_live(0.5, 0.35, 2.2, 150);
        assert_eq!((hasty.end_of_speech_frames(40), hasty.early_frames(40)), (5, 4), "a short hangover from the slider still ends after its head start");
        assert_eq!(p.pre_roll_frames, 16, "~500 ms pre-roll: the first syllable is never cut");
        assert_eq!(p.barge_in_frames, 5, "160 ms barge-in");
        assert_eq!(p.max_frames, 625, "20 s cap");
        assert_eq!(p.calibration_frames, 32);
    }

    #[test]
    fn speech_starts_after_two_frames_and_a_sentence_ends_after_15_silent_frames() {
        let mut m = calibrated();
        feed(&mut m, 5, SILENCE, FrameContext::default());
        let ev = feed(&mut m, 1, SPEECH, FrameContext::default());
        assert!(ev.is_empty(), "one voiced frame is not enough");
        let ev = feed(&mut m, 1, SPEECH, FrameContext::default());
        assert!(matches!(ev[..], [VadEvent::SpeechStarted { barge_in: false, ptt: false }]));
        // A sentence: more than 1.1 s of voice.
        feed(&mut m, 40, SPEECH, FrameContext::default());

        let ev = feed(&mut m, 14, SILENCE, FrameContext::default());
        // A 448 ms pause inside a sentence must not close it; after 224 ms the words so far go
        // out for a head start, and that is all.
        let ahead = match &ev[..] {
            [VadEvent::SpeechPaused { pcm, speech_ms }] => {
                assert_eq!(*speech_ms, 42 * 32);
                pcm.clone()
            }
            other => panic!("expected one SpeechPaused, got {other:?}"),
        };
        let ev = feed(&mut m, 1, SILENCE, FrameContext::default());
        match &ev[..] {
            [VadEvent::SpeechEnded { pcm, speech_ms, silence_ms, reason, .. }] => {
                assert_eq!(*reason, EndReason::Silence);
                assert_eq!(*silence_ms, 480);
                assert_eq!(*speech_ms, 42 * 32);
                // 16 pre-roll frames (incl. the 2 onset frames) + 40 speech + 3 kept tail frames
                assert_eq!(pcm.len() / FRAME, 16 + 40 + 3);
                assert_eq!(*pcm, ahead, "the head start was the very audio the utterance ends with");
            }
            other => panic!("expected SpeechEnded, got {other:?}"),
        }
        assert_eq!(m.phase(), VadPhase::Idle);
    }

    #[test]
    fn short_pauses_keep_recording() {
        let mut m = calibrated();
        feed(&mut m, 40, SPEECH, FrameContext::default());
        for _ in 0..3 {
            // ~400 ms pauses between the words of a sentence: each one hands the words so far out
            // once, and is called off the moment Sir carries on.
            assert!(matches!(feed(&mut m, 12, SILENCE, FrameContext::default())[..], [VadEvent::SpeechPaused { .. }]));
            assert!(matches!(feed(&mut m, 4, SPEECH, FrameContext::default())[..], [VadEvent::SpeechResumed]));
        }
        // A breath shorter than the head start is nothing at all.
        assert!(feed(&mut m, 5, SILENCE, FrameContext::default()).is_empty());
        assert!(feed(&mut m, 4, SPEECH, FrameContext::default()).is_empty());
        assert!(m.in_utterance());
    }

    #[test]
    fn a_reflex_word_goes_to_whisper_after_160_ms_and_ends_after_280() {
        // "Mute": ~0.5 s of voice. After 5 silent frames (160 ms) Whisper gets it; after 9
        // (288 ms) with nothing more said, the utterance ends with that same audio.
        let mut m = calibrated();
        feed(&mut m, 16, SPEECH, FrameContext::default());
        assert!(feed(&mut m, 4, SILENCE, FrameContext::default()).is_empty());
        let ev = feed(&mut m, 1, SILENCE, FrameContext::default());
        let ahead = match &ev[..] {
            [VadEvent::SpeechPaused { pcm, .. }] => pcm.clone(),
            other => panic!("expected SpeechPaused after 160 ms, got {other:?}"),
        };
        assert!(feed(&mut m, 3, SILENCE, FrameContext::default()).is_empty());
        let ev = feed(&mut m, 1, SILENCE, FrameContext::default());
        match &ev[..] {
            [VadEvent::SpeechEnded { pcm, silence_ms: 288, reason: EndReason::Silence, .. }] => assert_eq!(*pcm, ahead),
            other => panic!("expected SpeechEnded after 288 ms, got {other:?}"),
        }
    }

    #[test]
    fn a_short_command_is_not_cut_at_a_breath_between_its_words() {
        // "JAI … next page": a 256 ms breath after the name used to end the utterance (the
        // old 160 / 220 ms cut) and cost a second transcription. Now it only hands the name out
        // early, and the command stays one utterance.
        let mut m = calibrated();
        feed(&mut m, 14, SPEECH, FrameContext::default());
        let ev = feed(&mut m, 8, SILENCE, FrameContext::default());
        assert!(matches!(ev[..], [VadEvent::SpeechPaused { .. }]), "{ev:?}");
        let ev = feed(&mut m, 14, SPEECH, FrameContext::default());
        assert!(matches!(ev[..], [VadEvent::SpeechResumed]), "{ev:?}");
        assert!(m.in_utterance());
        // ~1 s of voice in all (past the reflex tier): the head start after 224 ms, the end
        // after 288 ms.
        assert!(feed(&mut m, 6, SILENCE, FrameContext::default()).is_empty());
        assert!(matches!(feed(&mut m, 1, SILENCE, FrameContext::default())[..], [VadEvent::SpeechPaused { .. }]));
        assert!(feed(&mut m, 1, SILENCE, FrameContext::default()).is_empty());
        let ev = feed(&mut m, 1, SILENCE, FrameContext::default());
        assert!(matches!(ev[..], [VadEvent::SpeechEnded { silence_ms: 288, reason: EndReason::Silence, .. }]), "{ev:?}");
        // Past 1.1 s of voice the sentence hangover applies.
        let mut m = calibrated();
        feed(&mut m, 36, SPEECH, FrameContext::default());
        feed(&mut m, 9, SILENCE, FrameContext::default());
        assert!(m.in_utterance());
    }

    #[test]
    fn the_voice_band_keeps_a_voice_and_drops_clicks_and_rumble() {
        let tone = |hz: f32| -> Vec<f32> { (0..FRAME * 8).map(|i| 0.1 * (std::f32::consts::TAU * hz * i as f32 / SAMPLE_RATE as f32).sin()).collect() };
        // The level a band lets through, measured once the filters have settled.
        let level = |hz: f32| {
            let mut band = VoiceBand::new();
            let x = tone(hz);
            x.chunks(FRAME).map(|f| band.rms(f)).last().unwrap() / crate::dsp::rms(&x[FRAME * 7..])
        };
        // A voice's fundamental and formants pass almost untouched …
        for hz in [220.0, 500.0, 1000.0, 1500.0] {
            assert!(level(hz) > 0.9, "{hz} Hz kept: {:.2}", level(hz));
        }
        // … rumble below and the hiss / click band above are down by 20 dB and more.
        for hz in [30.0, 40.0, 7000.0] {
            assert!(level(hz) < 0.1, "{hz} Hz dropped: {:.2}", level(hz));
        }
        assert_eq!(VoiceBand::new().rms(&[]), 0.0);
    }

    #[test]
    fn quiet_noise_with_high_probability_does_not_trigger() {
        let mut m = calibrated();
        // Silero says "speech" but energy is at the noise floor (e.g. distant TV).
        assert!(feed(&mut m, 20, (0.9, 0.003), FrameContext::default()).is_empty());
    }

    #[test]
    fn blips_are_discarded() {
        let mut m = calibrated();
        let mut events = feed(&mut m, 3, SPEECH, FrameContext::default());
        events.extend(feed(&mut m, 15, SILENCE, FrameContext::default()));
        assert!(matches!(events.last(), Some(VadEvent::Discarded { speech_ms: 96 })));
    }

    #[test]
    fn utterances_are_capped_at_max_length() {
        let mut m = calibrated();
        let events = feed(&mut m, 700, SPEECH, FrameContext::default());
        assert!(events.iter().any(|e| matches!(e, VadEvent::SpeechEnded { reason: EndReason::MaxLength, .. })));
    }

    #[test]
    fn barge_in_needs_160ms_of_louder_voice_while_speaking() {
        let mut m = calibrated();
        let speaking = FrameContext { assistant_speaking: true, ..Default::default() };
        // Normal-strength speech (prob 0.6) is treated as echo while JAI talks.
        assert!(feed(&mut m, 10, (0.6, 0.05), speaking).is_empty());
        let ev = feed(&mut m, 4, SPEECH, speaking);
        assert!(ev.is_empty(), "4 frames = 128 ms is not enough");
        let ev = feed(&mut m, 1, SPEECH, speaking);
        assert!(matches!(ev[..], [VadEvent::SpeechStarted { barge_in: true, .. }]));
    }

    #[test]
    fn push_to_talk_records_until_release() {
        let mut m = calibrated();
        let held = FrameContext { ptt: true, ..Default::default() };
        let ev = feed(&mut m, 1, SILENCE, held);
        assert!(matches!(ev[..], [VadEvent::SpeechStarted { ptt: true, .. }]));
        // Silence while holding must not end the utterance.
        assert!(feed(&mut m, 40, SILENCE, held).is_empty());
        let ev = feed(&mut m, 1, SILENCE, FrameContext::default());
        assert!(matches!(ev[..], [VadEvent::SpeechEnded { reason: EndReason::PttReleased, ptt: true, .. }]));
    }

    #[test]
    fn suspended_machine_never_opens_utterances() {
        let mut m = calibrated();
        let ctx = FrameContext { suspended: true, ..Default::default() };
        assert!(feed(&mut m, 30, SPEECH, ctx).is_empty());
    }

    #[test]
    fn calibration_is_robust_to_speech_burst() {
        let mut m = VadMachine::new(params());
        let frame = vec![0.0f32; FRAME];
        for i in 0..m.params.calibration_frames {
            let rms = if i < 8 { 0.2 } else { 0.004 };
            m.process(&frame, 0.1, rms, FrameContext::default());
        }
        assert!((m.noise_floor() - 0.004).abs() < 1e-6);
    }
}
