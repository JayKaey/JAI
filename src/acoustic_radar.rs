//! Acoustic environment radar: sharp transients and lasting sounds in the microphone signal,
//! grouped into gestures while nobody is talking. What a sound really was is `sound_sense`'s to
//! say (the radar hears that something happened; the sound model names it).
//!
//! * **Claps** — two or three broadband impulses (strong energy above 2.5 kHz, gone within
//!   ~40 ms) 150-500 ms apart, with nothing right before or after (applause and music don't
//!   count). Two: JAI wakes up like for "JAI", or toggles the music.
//! * **Door knock** — two to six low, percussive impulses (most energy in 100-900 Hz, little
//!   above 2.5 kHz) 100-550 ms apart, then quiet: while Sir has music on (headphones), the music is
//!   ducked, the HUD flashes amber and JAI says someone is at the door.
//! * **A single impulse** on its own (a snap, breaking glass) and **a sound that lasts** (a bell,
//!   an alarm, a bark): only the sound model can tell what they were.
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
/// How long after the last clap of a run it is over (and how far apart two claps of it may be).
const CLAP_GAP_MS: f64 = 500.0;
/// The quiet before a run of claps or knocks that makes it a gesture, not part of a noise.
const ALONE_MS: f64 = 1000.0;
/// A lasting sound: loud this long (or this long and then over) before it is told.
const SOUND_TOLD_MS: f64 = 600.0;
const SOUND_SHORTEST_MS: f64 = 200.0;
/// The rest after a lasting sound was told (music would else tell one every moment).
const SOUND_REST_MS: f64 = 3000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcousticEvent {
    /// Two or three claps in an even run, nothing just before or after.
    Claps { count: usize },
    Knock { count: usize },
    /// One impulse on its own: a snap, a lone clap, breaking glass.
    Single,
    /// A sound that lasts: a bell, an alarm, a ringing phone, a bark.
    Sound,
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
    /// The run of claps going on, told once no clap follows for `CLAP_GAP_MS`.
    claps: Vec<f64>,
    claps_until: Option<f64>,
    knocks: Vec<f64>,
    /// When the last impulse of any kind was, and whether the run going on came after quiet.
    last_impulse: f64,
    alone: bool,
    /// The lasting sound going on: since when, its last loud block, whether it was told.
    loud_since: Option<f64>,
    loud_last: f64,
    loud_told: bool,
    sound_rest_until: f64,
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
            claps_until: None,
            knocks: Vec::new(),
            last_impulse: f64::MIN,
            alone: false,
            loud_since: None,
            loud_last: 0.0,
            loud_told: false,
            sound_rest_until: 0.0,
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
        self.claps_until = None;
        self.loud_since = None;
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
        } else if listening && self.now_ms >= self.refractory_until && self.loud_since.is_none_or(|since| self.loud_last - since < 100.0) {
            // (Not inside a sound that lasts: its end would else pass for an impulse.)
            let k = 12.0 / self.sensitivity;
            let floor = 2e-5 / self.sensitivity;
            if e > self.background * k && e > floor {
                self.onset = Some(Onset { at: self.now_ms, peak: e, peak_low: el, peak_high: eh, after: Vec::with_capacity(DECAY_BLOCKS) });
                self.refractory_until = self.now_ms + 60.0;
            }
        }
        // A sound that lasts: loud for a while, short breaks inside it (the two tones of a bell)
        // part of it.
        let loud = e > self.background * (6.0 / self.sensitivity) && e > 2e-5 / self.sensitivity;
        if listening && loud {
            if self.loud_since.is_none() {
                self.loud_since = Some(self.now_ms);
                self.loud_told = false;
            }
            self.loud_last = self.now_ms;
        }
        if let Some(since) = self.loud_since {
            let over = !listening || self.now_ms - self.loud_last > 120.0;
            let lasted = self.loud_last - since;
            if listening && !self.loud_told && self.now_ms >= self.sound_rest_until && (lasted >= SOUND_TOLD_MS || (over && lasted >= SOUND_SHORTEST_MS)) {
                self.loud_told = true;
                self.sound_rest_until = self.now_ms + SOUND_REST_MS;
                out = out.or(Some(AcousticEvent::Sound));
            }
            if over {
                self.loud_since = None;
            }
        }
        // The background follows slowly and ignores the impulses themselves (and, all but, a
        // sound that lasts: it would else stop being loud before it is told).
        if self.onset.is_none() {
            let rate = if e <= self.background {
                0.02
            } else if self.loud_since.is_some() {
                0.0004
            } else {
                0.004
            };
            self.background += (e - self.background) * rate;
            self.background = self.background.max(1e-7);
        }
        if !listening {
            self.claps.clear();
            self.knocks.clear();
            self.claps_until = None;
        }
        out.or_else(|| self.tick())
    }

    /// Time-based confirmations (the end of a run of claps or knocks).
    fn tick(&mut self) -> Option<AcousticEvent> {
        if self.claps_until.is_some_and(|until| self.now_ms > until) {
            self.claps_until = None;
            let count = std::mem::take(&mut self.claps).len();
            // (Four and more: applause or a beat, not the gesture.)
            return match count {
                1 if self.alone => Some(AcousticEvent::Single),
                2 | 3 if self.alone => Some(AcousticEvent::Claps { count }),
                _ => None,
            };
        }
        if let Some(last) = self.knocks.last().copied() {
            if self.now_ms - last > 700.0 {
                let count = self.knocks.len();
                self.knocks.clear();
                return match count {
                    1 if self.alone => Some(AcousticEvent::Single),
                    2..=6 => Some(AcousticEvent::Knock { count }),
                    _ => None,
                };
            }
        }
        None
    }

    fn pattern(&mut self, kind: Kind, at: f64) -> Option<AcousticEvent> {
        let before = std::mem::replace(&mut self.last_impulse, at);
        match kind {
            Kind::Clap => {
                self.knocks.clear();
                if self.claps.last().is_some_and(|prev| (150.0..=CLAP_GAP_MS).contains(&(at - prev))) {
                    self.claps.push(at);
                } else {
                    // A new run: a gesture only after quiet.
                    self.claps = vec![at];
                    self.alone = at - before > ALONE_MS;
                }
                self.claps_until = Some(at + CLAP_GAP_MS);
            }
            Kind::Knock => {
                self.claps.clear();
                self.claps_until = None;
                if !self.knocks.last().is_some_and(|prev| (100.0..=550.0).contains(&(at - prev))) {
                    self.knocks.clear();
                    self.alone = at - before > ALONE_MS;
                }
                self.knocks.push(at);
                if self.knocks.len() > 6 {
                    // A steady beat (music), not someone at the door.
                    self.knocks.drain(..self.knocks.len() - 1);
                    self.alone = false;
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
        let mut s = silence(1200);
        s.extend(clap());
        s.extend(silence(260));
        s.extend(clap());
        s.extend(silence(1200));
        assert_eq!(run(&mut r, &s), vec![AcousticEvent::Claps { count: 2 }]);
    }

    /// Three claps are their own gesture, a lone clap is a single impulse (the sound model says
    /// whether it was a snap), and applause is nothing.
    #[test]
    fn claps_are_counted_and_applause_is_nothing() {
        let mut r = AcousticRadar::new(1.0);
        let mut s = silence(1200);
        for _ in 0..3 {
            s.extend(clap());
            s.extend(silence(250));
        }
        s.extend(silence(1500));
        s.extend(clap());
        s.extend(silence(1500));
        for _ in 0..6 {
            s.extend(clap());
            s.extend(silence(220));
        }
        s.extend(silence(1500));
        assert_eq!(run(&mut r, &s), vec![AcousticEvent::Claps { count: 3 }, AcousticEvent::Single]);
    }

    /// A tone that lasts is told once; a short one and claps are not lasting sounds.
    #[test]
    fn a_lasting_sound_is_told_once() {
        let tone = |ms: usize| -> Vec<f32> { (0..ms * 16).map(|i| (i as f32 * std::f32::consts::TAU * 660.0 / RATE).sin() * 0.3).collect() };
        let mut r = AcousticRadar::new(1.0);
        let mut s = silence(1200);
        s.extend(tone(1500));
        s.extend(silence(4000));
        s.extend(tone(60));
        s.extend(silence(1500));
        assert_eq!(run(&mut r, &s), vec![AcousticEvent::Sound]);
    }

    #[test]
    fn knock_series_is_detected() {
        let mut r = AcousticRadar::new(1.0);
        let mut s = silence(1200);
        for _ in 0..3 {
            s.extend(knock());
            s.extend(silence(230));
        }
        s.extend(silence(1200));
        assert_eq!(run(&mut r, &s), vec![AcousticEvent::Knock { count: 3 }]);
    }
}
