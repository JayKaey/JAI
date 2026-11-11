//! Acoustic environment radar: sharp transients in the microphone signal, classified and
//! grouped into gestures while nobody is talking.
//!
//! * **Double clap** — two broadband impulses (strong energy above 2.5 kHz, gone within ~40 ms)
//!   150-450 ms apart, with nothing right before or after (applause and music don't count): JAI
//!   wakes up like for "JAI", or toggles the music.
//! * **Door knock** — two to six low, percussive impulses (most energy in 100-900 Hz, little
//!   above 2.5 kHz) 100-550 ms apart, then quiet: while Sir has music on (headphones), the music is
//!   ducked, the HUD flashes amber and JAI says someone is at the door.
//!
//! Analysis runs in 8 ms blocks (128 samples at 16 kHz) on the VAD thread: an onset is a block
//! far above the slowly tracked background; it is kept when its energy falls away fast (an
//! impulse, not speech or a sustained note).

use crate::dsp::Biquad;

const RATE: f32 = 16_000.0;
const BLOCK: usize = 128;
const BLOCK_MS: f64 = 8.0;
/// Blocks after an onset that decide whether it was an impulse.
const DECAY_BLOCKS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcousticEvent {
    DoubleClap,
    Knock { count: usize },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Clap,
    Knock,
}

#[derive(Debug, Clone)]
struct Onset {
    at: f64,
    peak: f32,
    peak_low: f32,
    peak_high: f32,
    after: Vec<f32>,
}

pub struct AcousticRadar {
    sensitivity: f32,
    low_hp: Biquad,
    low_lp: Biquad,
    high_hp: Biquad,
    background: f32,
    primed: bool,
    now_ms: f64,
    refractory_until: f64,
    onset: Option<Onset>,
    claps: Vec<f64>,
    /// The second clap of a pair: confirmed when no third one follows.
    double_until: Option<f64>,
    knocks: Vec<f64>,
    carry: Vec<f32>,
}

impl AcousticRadar {
    pub fn new(sensitivity: f32) -> Self {
        Self {
            sensitivity: sensitivity.clamp(0.5, 2.0),
            low_hp: Biquad::high_pass(100.0, RATE),
            low_lp: Biquad::low_pass(900.0, RATE),
            high_hp: Biquad::high_pass(2500.0, RATE),
            background: 0.0,
            primed: false,
            now_ms: 0.0,
            refractory_until: 0.0,
            onset: None,
            claps: Vec::new(),
            double_until: None,
            knocks: Vec::new(),
            carry: Vec::with_capacity(BLOCK),
        }
    }

    pub fn set_sensitivity(&mut self, s: f32) {
        self.sensitivity = s.clamp(0.5, 2.0);
    }

    /// Forgets half-seen gestures (Sir started talking, JAI is speaking).
    pub fn reset_patterns(&mut self) {
        self.onset = None;
        self.claps.clear();
        self.knocks.clear();
        self.double_until = None;
    }

    /// One 16 kHz frame. `listening`: false while someone talks (the frame still feeds the
    /// background level, but no gesture can start).
    pub fn process(&mut self, frame: &[f32], listening: bool) -> Option<AcousticEvent> {
        let mut event = None;
        self.carry.extend_from_slice(frame);
        let mut offset = 0;
        while self.carry.len() - offset >= BLOCK {
            let block: Vec<f32> = self.carry[offset..offset + BLOCK].to_vec();
            offset += BLOCK;
            if let Some(e) = self.block(&block, listening) {
                event = Some(e);
            }
        }
        self.carry.drain(..offset);
        event
    }

    fn block(&mut self, block: &[f32], listening: bool) -> Option<AcousticEvent> {
        self.now_ms += BLOCK_MS;
        let (mut e, mut el, mut eh) = (0.0f32, 0.0f32, 0.0f32);
        for &x in block {
            let low = self.low_lp.process(self.low_hp.process(x));
            let high = self.high_hp.process(x);
            e += x * x;
            el += low * low;
            eh += high * high;
        }
        let n = block.len() as f32;
        let (e, el, eh) = (e / n, el / n, eh / n);
        if !self.primed {
            self.background = e.max(1e-7);
            self.primed = true;
            return None;
        }
        let mut out = None;
        if let Some(on) = self.onset.as_mut() {
            if e > on.peak {
                on.peak = e;
                on.peak_low = el;
                on.peak_high = eh;
            }
            on.after.push(e);
            if on.after.len() >= DECAY_BLOCKS {
                let on = self.onset.take().expect("onset present");
                if let Some(kind) = classify(&on) {
                    out = self.pattern(kind, on.at);
                }
            }
        } else if listening && self.now_ms >= self.refractory_until {
            let k = 12.0 / self.sensitivity;
            let floor = 2e-5 / self.sensitivity;
            if e > self.background * k && e > floor {
                self.onset = Some(Onset { at: self.now_ms, peak: e, peak_low: el, peak_high: eh, after: Vec::with_capacity(DECAY_BLOCKS) });
                self.refractory_until = self.now_ms + 60.0;
            }
        }
        // The background follows slowly and ignores the impulses themselves.
        if self.onset.is_none() {
            let rate = if e > self.background { 0.004 } else { 0.02 };
            self.background += (e - self.background) * rate;
            self.background = self.background.max(1e-7);
        }
        if !listening {
            self.claps.clear();
            self.knocks.clear();
            self.double_until = None;
        }
        out.or_else(|| self.tick())
    }

    /// Time-based confirmations (a pair of claps with no third one; the end of a knock series).
    fn tick(&mut self) -> Option<AcousticEvent> {
        if let Some(until) = self.double_until {
            if self.now_ms > until {
                self.double_until = None;
                self.claps.clear();
                return Some(AcousticEvent::DoubleClap);
            }
        }
        if let Some(last) = self.knocks.last().copied() {
            if self.now_ms - last > 700.0 {
                let count = self.knocks.len();
                self.knocks.clear();
                if (2..=6).contains(&count) {
                    return Some(AcousticEvent::Knock { count });
                }
            }
        }
        None
    }

    fn pattern(&mut self, kind: Kind, at: f64) -> Option<AcousticEvent> {
        match kind {
            Kind::Clap => {
                self.knocks.clear();
                if self.double_until.is_some() {
                    // A third clap: applause or a beat, not the gesture.
                    self.double_until = None;
                    self.claps.clear();
                    self.claps.push(at);
                    return None;
                }
                self.claps.retain(|t| at - t < 1500.0);
                if let Some(prev) = self.claps.last().copied() {
                    let gap = at - prev;
                    let isolated = self.claps.len() == 1 || at - self.claps[self.claps.len() - 2] > 1000.0;
                    if (150.0..=450.0).contains(&gap) && isolated {
                        self.double_until = Some(at + 450.0);
                    }
                }
                self.claps.push(at);
            }
            Kind::Knock => {
                self.claps.clear();
                self.double_until = None;
                if let Some(prev) = self.knocks.last().copied() {
                    let gap = at - prev;
                    if !(100.0..=550.0).contains(&gap) {
                        self.knocks.clear();
                    }
                }
                self.knocks.push(at);
                if self.knocks.len() > 6 {
                    // A steady beat (music), not someone at the door.
                    self.knocks.drain(..self.knocks.len() - 1);
                }
            }
        }
        None
    }
}

/// An impulse that fell away fast: a clap (broadband, bright) or a knock (low, dull).
fn classify(on: &Onset) -> Option<Kind> {
    let tail: f32 = on.after[3..].iter().sum::<f32>() / (on.after.len() - 3).max(1) as f32;
    if tail > on.peak * 0.25 {
        return None;
    }
    let high = on.peak_high / on.peak.max(1e-9);
    let low = on.peak_low / on.peak.max(1e-9);
    if high > 0.22 {
        Some(Kind::Clap)
    } else if low > 0.4 && high < 0.15 {
        Some(Kind::Knock)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn silence(ms: usize) -> Vec<f32> {
        (0..ms * 16).map(|i| ((i * 7919) % 13) as f32 * 1e-5 - 6e-5).collect()
    }

    /// A clap: a 4 ms burst of white-ish noise with a fast exponential decay.
    fn clap() -> Vec<f32> {
        let mut seed = 12345u32;
        (0..640)
            .map(|i| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let noise = ((seed >> 16) as f32 / 32768.0) - 1.0;
                noise * 0.6 * (-(i as f32) / 40.0).exp()
            })
            .collect()
    }

    /// A knock: a low 180 Hz thump decaying within ~20 ms.
    fn knock() -> Vec<f32> {
        (0..640).map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 180.0 / RATE).sin() * 0.5 * (-(i as f32) / 120.0).exp()).collect()
    }

    fn run(radar: &mut AcousticRadar, signal: &[f32]) -> Vec<AcousticEvent> {
        signal.chunks(512).filter_map(|f| radar.process(f, true)).collect()
    }

    #[test]
    fn double_clap_is_detected_once() {
        let mut r = AcousticRadar::new(1.0);
        let mut s = silence(800);
        s.extend(clap());
        s.extend(silence(260));
        s.extend(clap());
        s.extend(silence(1200));
        assert_eq!(run(&mut r, &s), vec![AcousticEvent::DoubleClap]);
    }

    #[test]
    fn triple_clap_and_single_clap_are_ignored() {
        let mut r = AcousticRadar::new(1.0);
        let mut s = silence(800);
        for _ in 0..3 {
            s.extend(clap());
            s.extend(silence(250));
        }
        s.extend(silence(1500));
        s.extend(clap());
        s.extend(silence(1500));
        assert!(run(&mut r, &s).is_empty());
    }

    #[test]
    fn knock_series_is_detected() {
        let mut r = AcousticRadar::new(1.0);
        let mut s = silence(800);
        for _ in 0..3 {
            s.extend(knock());
            s.extend(silence(230));
        }
        s.extend(silence(1200));
        assert_eq!(run(&mut r, &s), vec![AcousticEvent::Knock { count: 3 }]);
    }
}
