//! Acoustic environment radar: sharp transients and lasting sounds in the microphone signal,
//! grouped into gestures while nobody is talking. What a sound really was is `sound_sense`'s to
//! say (the radar hears that something happened; the sound model names it).
//!
//! * **Claps** — two or three broadband impulses (energy above 2.5 kHz, gone within ~40 ms)
//!   150-900 ms apart at an even pace, with nothing right before or after (applause and music
//!   don't count). Two: JAI wakes up like for "JAI", or toggles the music.
//! * **Door knock** — two to six low, percussive impulses (most energy in 100-900 Hz, little
//!   above 2.5 kHz) 100-550 ms apart, then quiet: while Sir has music on (headphones), the music is
//!   ducked, the HUD flashes amber and JAI says someone is at the door.
//! * **A single impulse** on its own (a snap, breaking glass) and **a sound that lasts** (a bell,
//!   an alarm, a bark): only the sound model can tell what they were.
//!
//! Analysis runs in 8 ms blocks (128 samples at 16 kHz) on the VAD thread: an onset is a block
//! far above the slowly tracked background (inside a sound that lasts, a sharp jump over the
//! block before); it is kept when its energy falls away fast (an impulse, not speech or a
//! sustained note). The radar is lenient on purpose: the sound model judges what it heard.

use crate::dsp::Biquad;

const RATE: f32 = 16_000.0;
const BLOCK: usize = 128;
const BLOCK_MS: f64 = 8.0;
/// Blocks after an onset that decide whether it was an impulse.
const DECAY_BLOCKS: usize = 6;
/// How far apart the first two claps of a run may be (a deliberate, loud double clap is slow:
/// Sir's of 2026-11-21 were over 500 ms apart and came out as two runs).
const CLAP_GAP_MS: f64 = 900.0;
/// Inside a sound that lasts, an impulse is a block this many times the one before it.
const JUMP: f32 = 16.0;
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

/// What Sir taught about his own claps and snaps (`sound_sense::personal`): how far over the
/// room an impulse must stand (his softest, with room to spare) and how much echo may follow it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Personal {
    pub gate: f32,
    pub tail: f32,
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
    personal: Option<Personal>,
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
    /// Whether the sound that lasts began with an impulse: a clap's own echo, told only if it
    /// lasts `SOUND_TOLD_MS`.
    loud_impulse: bool,
    sound_rest_until: f64,
    /// The block before's energy.
    prev: f32,
    /// Where what was told last began (`span_ms`).
    told_from: f64,
    carry: Vec<f32>,
}

impl AcousticRadar {
    pub fn new(sensitivity: f32) -> Self {
        Self {
            sensitivity: sensitivity.clamp(0.5, 2.0),
            personal: None,
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
            loud_impulse: false,
            sound_rest_until: 0.0,
            prev: 0.0,
            told_from: 0.0,
            carry: Vec::with_capacity(BLOCK),
        }
    }

    /// How long ago what was told last began: the sound model hears it from just before
    /// (three seconds of what came before made two claps "Computer keyboard", 2026-11-21).
    pub fn span_ms(&self) -> f64 {
        self.now_ms - self.told_from
    }

    /// How long after the latest clap of a run the next may come, and the run is over without
    /// one: up to `CLAP_GAP_MS` for the second, then about the run's own pace.
    fn clap_window(&self) -> f64 {
        match self.claps.as_slice() {
            [first, second, ..] => ((second - first) * 1.6).clamp(350.0, CLAP_GAP_MS),
            _ => CLAP_GAP_MS,
        }
    }

    pub fn set_sensitivity(&mut self, s: f32) {
        self.sensitivity = s.clamp(0.5, 2.0);
    }

    pub fn set_personal(&mut self, personal: Option<Personal>) {
        self.personal = personal;
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
                if let Some(kind) = classify(&on, self.personal) {
                    // (The loud stretch it began is its own echo.)
                    self.loud_impulse |= self.loud_since.is_some_and(|since| since >= on.at);
                    out = self.pattern(kind, on.at);
                }
            }
        } else if listening && self.now_ms >= self.refractory_until {
            // Inside a loud stretch (a clap's echo, a bell) only a sharp jump over the block before
            // is an impulse: the sound's own ups and downs and its end are none. (From its first
            // block on: with his gate open, a clap's echo 64 ms on was a "clap" of its own, and
            // the pair it began was no longer alone.)
            let inside = self.loud_since.is_some();
            let (mut k, mut floor) = (12.0 / self.sensitivity, 6e-6 / self.sensitivity);
            if let Some(his) = self.personal {
                // (His softest taught one gets through; his prints say whether it was his.)
                k = k.min(his.gate);
                floor = floor.min(1e-6);
            }
            if e > self.background * k && e > floor && (!inside || e > self.prev * JUMP) {
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
                self.loud_impulse = false;
            }
            self.loud_last = self.now_ms;
        }
        if let Some(since) = self.loud_since {
            let over = !listening || self.now_ms - self.loud_last > 120.0;
            let lasted = self.loud_last - since;
            // (Claps in an echoing room keep it loud from one to the next: the echo of impulses is
            // told as a sound only once it lasts far longer than an echo can.)
            let told_after = if self.loud_impulse { 3.0 * SOUND_TOLD_MS } else { SOUND_TOLD_MS };
            if listening && !self.loud_told && self.now_ms >= self.sound_rest_until && (lasted >= told_after || (over && lasted >= SOUND_SHORTEST_MS && !self.loud_impulse)) {
                self.loud_told = true;
                self.sound_rest_until = self.now_ms + SOUND_REST_MS;
                self.told_from = since;
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
        self.prev = e;
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
            let claps = std::mem::take(&mut self.claps);
            self.told_from = claps.first().copied().unwrap_or(self.now_ms);
            // (Four and more: applause or a beat, not the gesture.)
            let count = claps.len();
            return match count {
                1 if self.alone => Some(AcousticEvent::Single),
                2 | 3 if self.alone => Some(AcousticEvent::Claps { count }),
                _ => None,
            };
        }
        if let Some(last) = self.knocks.last().copied() {
            if self.now_ms - last > 700.0 {
                let count = self.knocks.len();
                self.told_from = self.knocks[0];
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
                let window = self.clap_window();
                if self.claps.last().is_some_and(|prev| (150.0..=window).contains(&(at - prev))) {
                    self.claps.push(at);
                } else {
                    // A new run: a gesture only after quiet.
                    self.claps = vec![at];
                    self.alone = at - before > ALONE_MS;
                }
                self.claps_until = Some(at + self.clap_window());
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
/// Taught (`personal`), more echo may follow and an impulse between bright and dull counts too:
/// his snap through a headset's microphone.
fn classify(on: &Onset, personal: Option<Personal>) -> Option<Kind> {
    let tail: f32 = on.after[3..].iter().sum::<f32>() / (on.after.len() - 3).max(1) as f32;
    if tail > on.peak * personal.map_or(0.25, |his| his.tail) {
        return None;
    }
    let high = on.peak_high / on.peak.max(1e-9);
    let low = on.peak_low / on.peak.max(1e-9);
    if low > 0.4 && high < 0.15 {
        Some(Kind::Knock)
    } else if high > 0.12 || personal.is_some() {
        // (Bright, or a clap as a headset's microphone hears it, its treble cut.)
        Some(Kind::Clap)
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

    /// Noise as from a quiet room (rms ≈ 0.0005, what his microphone hears).
    fn room(ms: usize) -> Vec<f32> {
        let mut seed = 99u32;
        (0..ms * 16)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (((seed >> 16) as f32 / 32768.0) - 1.0) * 0.0009
            })
            .collect()
    }

    /// A clap in a room as a headset's microphone hears it: a burst of `peak`, its treble cut
    /// above `bright` Hz, and its echo dying away over about a second.
    fn room_clap(peak: f32, bright: f32) -> Vec<f32> {
        let mut seed = 777u32;
        let mut treble = crate::dsp::Biquad::low_pass(bright, RATE);
        (0..16 * 500)
            .map(|i| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let t = i as f32;
                treble.process((((seed >> 16) as f32 / 32768.0) - 1.0) * peak * ((-t / 40.0).exp() + 0.15 * (-t / 2500.0).exp()))
            })
            .collect()
    }

    /// A finger snap: a crack of under a millisecond, bright and dry.
    fn snap(peak: f32) -> Vec<f32> {
        let mut seed = 4242u32;
        let mut bright = crate::dsp::Biquad::high_pass(2000.0, RATE);
        (0..16 * 60)
            .map(|i| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                bright.process((((seed >> 16) as f32 / 32768.0) - 1.0) * peak * (-(i as f32) / 10.0).exp())
            })
            .collect()
    }

    /// A room's noise with these sounds laid in at these times (ms).
    fn scene(ms: usize, sounds: &[(usize, Vec<f32>)]) -> Vec<f32> {
        let mut s = room(ms);
        for (at, sound) in sounds {
            for (y, x) in s.iter_mut().skip(at * 16).zip(sound) {
                *y += x;
            }
        }
        s
    }

    /// Sir's claps of 2026-11-21: the first clap's echo counted as a sound that lasts, which
    /// let no second clap start inside it, and claps over 500 ms apart were two runs. Each came
    /// out as a "Sound" and, 128 ms later, a "Single".
    #[test]
    fn claps_in_a_room_count_however_they_are_spaced() {
        for gap in [220, 450, 700] {
            let clap = room_clap(0.3, 6000.0);
            let s = scene(1500 + gap + 2500, &[(1500, clap.clone()), (1500 + gap, clap)]);
            assert_eq!(run(&mut AcousticRadar::new(1.0), &s), vec![AcousticEvent::Claps { count: 2 }], "claps {gap} ms apart");
        }
        // (One clap's own echo is no sound that lasts.)
        let s = scene(1500 + 2500, &[(1500, room_clap(0.3, 6000.0))]);
        assert_eq!(run(&mut AcousticRadar::new(1.0), &s), vec![AcousticEvent::Single]);
    }

    /// Softer claps, claps whose treble the headset's microphone cuts, and a finger snap.
    #[test]
    fn soft_and_dull_claps_and_a_snap_are_heard() {
        for (what, clap) in [("soft", room_clap(0.015, 6000.0)), ("dull", room_clap(0.3, 2500.0))] {
            let s = scene(1500 + 300 + 2500, &[(1500, clap.clone()), (1800, clap)]);
            assert_eq!(run(&mut AcousticRadar::new(1.0), &s), vec![AcousticEvent::Claps { count: 2 }], "{what} claps");
        }
        let s = scene(1500 + 2500, &[(1500, snap(0.03))]);
        assert_eq!(run(&mut AcousticRadar::new(1.0), &s), vec![AcousticEvent::Single], "a snap");
    }

    /// Sir, 2026-11-22: "the clap and snap detection is extremely bad and barely works even with
    /// the training". Taught his snaps, 45 s of snapping gave the radar one: a snap his headset's
    /// microphone hears without its treble is neither bright nor dull, a soft one stands under
    /// twelve times the room, and a clap in his room keeps too much echo for the impulse test.
    /// What he taught (`Personal`) opens the radar to his own; the sound model and his prints
    /// (`sound_sense`) judge what came through.
    #[test]
    fn his_own_snaps_and_claps_come_through_once_taught() {
        // (A headset's microphone cuts its treble steeply: four poles at 1.6 kHz.)
        let mid_snap = |peak: f32| -> Vec<f32> {
            let mut seed = 4242u32;
            let mut hp = crate::dsp::Biquad::high_pass(900.0, RATE);
            let (mut lp1, mut lp2) = (crate::dsp::Biquad::low_pass(1600.0, RATE), crate::dsp::Biquad::low_pass(1600.0, RATE));
            (0..16 * 60)
                .map(|i| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    let x = hp.process((((seed >> 16) as f32 / 32768.0) - 1.0) * peak * (-(i as f32) / 12.0).exp());
                    lp2.process(lp1.process(x))
                })
                .collect()
        };
        let echoing_clap = || -> Vec<f32> {
            let mut seed = 777u32;
            (0..16 * 500)
                .map(|i| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    let t = i as f32;
                    (((seed >> 16) as f32 / 32768.0) - 1.0) * 0.3 * ((-t / 40.0).exp() + 0.6 * (-t / 2500.0).exp())
                })
                .collect()
        };
        let his = Personal { gate: 4.0, tail: 0.6 };
        let heard = |signal: &[f32], personal: Option<Personal>| {
            let mut r = AcousticRadar::new(1.0);
            r.set_personal(personal);
            run(&mut r, signal)
        };
        let headset_snap = scene(1500 + 2500, &[(1500, mid_snap(0.08))]);
        let soft_snap = scene(1500 + 2500, &[(1500, snap(0.012))]);
        let claps = scene(1500 + 400 + 2500, &[(1500, echoing_clap()), (1900, echoing_clap())]);
        let found: Vec<(&str, Vec<AcousticEvent>, Vec<AcousticEvent>)> = vec![
            // Untaught: a snap without treble is let be, a soft one is under the gate, claps with
            // a long echo are no impulses but a sound that lasts (the sound model names it).
            ("headset snap, untaught", heard(&headset_snap, None), vec![]),
            ("soft snap, untaught", heard(&soft_snap, None), vec![]),
            ("echoing claps, untaught", heard(&claps, None), vec![AcousticEvent::Sound]),
            ("headset snap, taught", heard(&headset_snap, Some(his)), vec![AcousticEvent::Single]),
            ("soft snap, taught", heard(&soft_snap, Some(his)), vec![AcousticEvent::Single]),
            ("echoing claps, taught", heard(&claps, Some(his)), vec![AcousticEvent::Claps { count: 2 }]),
            ("the room alone, however open", heard(&room(4000), Some(his)), vec![]),
        ];
        let wrong: Vec<String> = found.iter().filter(|(_, got, want)| got != want).map(|(what, got, want)| format!("{what}: {got:?}, not {want:?}")).collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// The sound model hears the gesture itself: from its first clap to the moment it is told
    /// (the gap and the run's own pace after it), not the seconds before.
    #[test]
    fn the_models_clip_starts_at_the_first_clap() {
        let clap = room_clap(0.3, 6000.0);
        let s = scene(1500 + 300 + 2500, &[(1500, clap.clone()), (1800, clap)]);
        let mut r = AcousticRadar::new(1.0);
        let told: Vec<(AcousticEvent, f64)> = s.chunks(512).filter_map(|f| r.process(f, true).map(|ev| (ev, r.span_ms()))).collect();
        assert!(matches!(told.as_slice(), [(AcousticEvent::Claps { count: 2 }, span)] if (760.0..=860.0).contains(span)), "{told:?}");
    }
}
