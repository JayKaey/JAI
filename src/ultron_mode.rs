//! Ultron Mode ("Jarvis, switch to Ultron mode", "Activate Ultron protocol", "Ultron Modus
//! aktivieren", the header's `[ ULTRON ]`): the whole HUD fades to the blood-crimson Ultron
//! palette (`theme_engine::set_ultron`, the stored theme untouched), the emblem reads
//! `U.L.T.R.O.N.`, and the voice becomes another man's: a deep, deliberate American baritone
//! (`base_voice()` at `PACE`) that never passes Jarvis' studio chain. A clause of his goes from
//! the synthesis straight through his own (`voice_of`):
//!
//! * he speaks General American (`phonemizer::Accent::American`): with Jarvis' Received
//!   Pronunciation an American voice still sounded like an Englishman, and Sir heard "Jarvis,
//!   lower" (2026-11-06);
//! * the pitch goes down to the median F0 of Sir's Ultron clip (measured: 88.5 Hz) in two parts:
//!   8 % by resampling, which lowers the formants with it (a larger chest, `FORMANT_RATIO`), and
//!   the rest as a move of the pitch alone (`fish_zero_shot_tts::reshape_f0`, TD-PSOLA), so the
//!   vowels stay vowels. Reaching the clip's pitch by resampling alone dragged every formant down
//!   by a quarter and had Kokoro speak a third too fast to make up for the stretch: that was the
//!   voice Sir called "not Ultron's". His melody is narrowed towards the clip's, never by more
//!   than `MAX_FLATTEN`: the sarcasm lives in it;
//! * the man, not a machine around him: Sir wants to hear the actor who speaks Ultron (James
//!   Spader: slow, husky, dry), so the layers are traces. Under the voice a hint of a second one
//!   an octave below (`octave_voice`), around it a trace of a metal chassis (`chassis`: two
//!   short resonant delays on the 450-3600 Hz band), a little grit (`grit`: a driven copy of
//!   the voice's body) and breath in it (`husk`: noise that follows the voice's loudness);
//! * the envelope of the clip (`assets/ultron_voice_reference.mp3`, any `ultron*.mp3|wav` next to
//!   JARVIS.exe), imposed by the same 128-band transfer the Jarvis voice uses.
//!
//! The base voice is picked by measurement (`calibrate`, `--voice-calibrate ultron`): the
//! American Kokoro voice or blend whose chain output is closest to the clip with the least pitch
//! to move, stored in the clip's profile cache.
//! `verify` (`--voice-verify`) reports how close the finished voice is: the similarity of the
//! long-term spectral envelopes and the F0 statistics — what can be measured, not a judgement of
//! whether it is the actor. "Switch back to Jarvis" restores the palette, the voice and its pace
//! exactly as they were.

use crate::dsp::Biquad;
use crate::fish_zero_shot_tts::{f0_stats, load_profile, pitch_track, reshape_f0, F0Stats, Match, ReferenceProfile, SpectralTransfer, Verification};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use tracing::{debug, info, warn};

/// The base voice until `calibrate` has picked one: Kokoro's two deepest American voices.
pub const VOICE: &str = "am_onyx:0.50+am_fenrir:0.50";
/// Ultron's deliberate pace (× Sir's speaking rate). Measured on the clips (2026-11-06,
/// `voice_features`): the film's Ultron (James Spader, who never hurries a line) speaks 3.1
/// syllables a second, its Jarvis 4.7. At 0.8 the synthesis still spoke 4.4, at 0.72 it spoke
/// 4.0. (Whether he is still understood at this pace is measured: the verifier has Whisper
/// listen to him.)
pub const PACE: f32 = 0.68;
/// −2.2 semitones: the pitch step without a clip to lock to.
pub const PITCH_SEMITONES: f32 = -2.2;
/// How much lower the formants sit than the base voice's (a larger chest). The rest of the way
/// down to the clip's pitch moves the pitch alone.
pub const FORMANT_RATIO: f32 = 0.92;
/// The most his melody is narrowed towards the clip's.
pub const MAX_FLATTEN: f32 = 0.3;
/// The second voice, an octave below: its level against the voice's, and where its body ends.
/// A hint of body under the voice, no second speaker: Sir's clip shows no period doubling, and
/// what he wants to hear is the man who speaks Ultron (James Spader), not a machine around him.
pub const OCTAVE_MIX: f32 = 0.07;
pub const OCTAVE_BODY_HZ: f32 = 1200.0;
/// The chassis: (delay ms, feedback) of its two resonances, the band they ring in (Hz) and
/// their level against the voice's (a trace: the film leaves the actor's voice nearly dry).
pub const CHASSIS: [(f32, f32); 2] = [(3.3, 0.46), (5.7, 0.40)];
pub const BODY: (f32, f32) = (450.0, 3600.0);
pub const CHASSIS_MIX: f32 = 0.06;
/// Grit: how hard the copy of the voice's body (140-2800 Hz) is driven, and its level.
pub const GRIT_DRIVE: f32 = 3.0;
pub const GRIT_MIX: f32 = 0.05;
/// Breath: the husk in a voice that lets air through. The band it sits in (Hz) and its level
/// against the voice's.
pub const HUSK_BAND: (f32, f32) = (1800.0, 7000.0);
pub const HUSK_MIX: f32 = 0.05;
/// The level a clause is brought to before the compressor (as Jarvis' chain does).
const PRE_GAIN_RMS: f32 = 0.089;
/// His envelope transfer: how much of the way to the clip's envelope, and how many dB a band
/// at most. Further than Jarvis' (0.78, 9 dB): the clip is much darker than any synthesis.
pub const TRANSFER_ALPHA: f32 = 0.92;
pub const MAX_TRANSFER_DB: f32 = 12.0;

/// The transfer that imposes the clip's envelope on his clauses.
pub fn transfer_for(profile: &ReferenceProfile) -> SpectralTransfer {
    SpectralTransfer::with(profile, TRANSFER_ALPHA, MAX_TRANSFER_DB)
}
pub const ENGAGED: &str = "Ultron protocol engaged, Sir.";
pub const TERMINATED: &str = "Ultron protocol terminated. I am back online, Sir.";
/// What the calibration and the verification say (their own words, not the film's).
pub const CALIBRATION_LINE: &str = "The Ultron protocol is online, Sir. Every system now answers to one deliberate voice, and it does not intend to repeat itself.";
/// A sentence the transfer never saw.
pub const HELD_OUT_LINE: &str = "Good evening. The reactor is stable, the perimeter is quiet, and I have all the time in the world.";
const PROFILE_PATH: &str = "assets/ultron_reference_profile.bin";

static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The voice and speaking rate (f32 bits) Ultron Mode replaced.
static SAVED: Mutex<Option<(String, u32)>> = Mutex::new(None);

pub fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// The emblem's wordmark.
pub fn wordmark() -> &'static str {
    if active() { "U.L.T.R.O.N." } else { "J.A.R.V.I.S." }
}

/// His name in the text console's log.
pub fn console_name() -> &'static str {
    wordmark()
}

/// How much the chain stretches a clause (only its resampling part does), so the Kokoro speed
/// accounts for it.
pub fn pitch_ratio() -> f32 {
    FORMANT_RATIO
}

/// The base voice: the calibrated one once the clip has been calibrated, else `VOICE`.
pub fn base_voice() -> String {
    reference_profile().ok().and_then(|p| p.blend).unwrap_or_else(|| VOICE.to_string())
}

/// Sir's Ultron clip, measured: its median F0 and intonation width (once the clip loaded).
static REFERENCE_F0: Mutex<Option<F0Stats>> = Mutex::new(None);

/// The pitch step that moves a clause whose median F0 is `clause_hz` onto the clip's
/// `reference_hz` (−7 … +2 semitones; outside that the measurement is not trusted).
pub fn lock_ratio(clause_hz: f32, reference_hz: f32) -> f32 {
    (reference_hz / clause_hz.max(1.0)).clamp(2f32.powf(-7.0 / 12.0), 2f32.powf(2.0 / 12.0))
}

/// How much flatter a clause with intonation width `clause_st` must be to match the clip's
/// `reference_st` (0 … 0.6).
pub fn flatten_to(clause_st: f32, reference_st: f32) -> f32 {
    if clause_st <= reference_st || clause_st <= 0.0 {
        return 0.0;
    }
    (1.0 - reference_st / clause_st).clamp(0.0, 0.6)
}

/// Ultron Mode on or off: palette, wordmark, voice and pace. Returns the line to say (in the
/// new voice), None when it already was in that mode.
pub fn set(on: bool, tts: &crate::tts::TtsController) -> Option<&'static str> {
    if on == active() {
        return None;
    }
    let speed = tts.speed_handle();
    let mut saved = SAVED.lock().unwrap_or_else(|e| e.into_inner());
    if on {
        let rate = speed.load(Ordering::Relaxed);
        *saved = Some((tts.voice(), rate));
        tts.set_voice(&base_voice());
        speed.store((f32::from_bits(rate) * PACE).clamp(0.5, 2.0).to_bits(), Ordering::Relaxed);
        std::thread::Builder::new().name("ultron-reference".into()).spawn(|| { LazyLock::force(&TRANSFER); }).ok();
    } else if let Some((voice, rate)) = saved.take() {
        tts.set_voice(&voice);
        speed.store(rate, Ordering::Relaxed);
    }
    ACTIVE.store(on, Ordering::Release);
    crate::undo::keep_mode(on);
    crate::theme_engine::set_ultron(on);
    crate::gui::refresh();
    info!("[ULTRON] {}", if on { "engaged" } else { "terminated" });
    Some(if on { ENGAGED } else { TERMINATED })
}

/// "Switch to Ultron mode" (true) / "switch back to Jarvis" (false), English and German. While
/// Ultron is on (`ultron_on`), a bare "change back" / "switch back" / "normal mode" also ends it.
/// This is stage 1 of the reflex router (`intent_engine::route`): it is asked before music or
/// any other parser, so "change back" is never a track.
pub fn parse(text: &str, ultron_on: bool) -> Option<bool> {
    let s: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == ' ' { c } else { ' ' }).collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let off = ["switch back to jarvis", "back to jarvis", "switch back to normal", "back to normal mode", "deactivate ultron", "disable ultron", "turn off ultron", "ultron mode off", "end ultron", "stop ultron", "exit ultron", "ultron modus deaktivieren", "ultron modus aus", "zurück zu jarvis", "wieder jarvis"];
    if off.iter().any(|p| s.contains(p)) {
        return Some(false);
    }
    let bare = s.strip_prefix("ultron ").unwrap_or(&s);
    let bare_off = ["change back", "switch back", "turn back", "go back to normal", "back to normal", "normal mode", "jarvis mode", "be jarvis again", "zurück wechseln", "wechsel zurück", "wechsle zurück", "normaler modus"];
    let bare = bare.trim_end_matches(" please").trim_end_matches(" now").trim_end_matches(" to");
    if ultron_on && bare_off.contains(&bare) {
        return Some(false);
    }
    let on = ["switch to ultron", "activate ultron", "ultron mode on", "enable ultron", "turn on ultron", "engage ultron", "become ultron", "ultron modus aktivieren", "ultron modus an", "aktiviere ultron", "wechsel zu ultron", "wechsle zu ultron"];
    on.iter().any(|p| s.contains(p)).then_some(true)
}

// ----------------------------------------------------------------------- the reference ----

/// Sir's Ultron clip: the fixed names first, else the newest `*ultron*.mp3|wav` in `assets/` or
/// next to JARVIS.exe.
pub fn reference_path() -> Option<PathBuf> {
    for fixed in ["assets/ultron_voice_reference.mp3", "assets/ultron_voice_reference.wav"] {
        if Path::new(fixed).is_file() {
            return Some(PathBuf::from(fixed));
        }
    }
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = ["assets", "."]
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
            name.contains("ultron") && (name.ends_with(".mp3") || name.ends_with(".wav"))
        })
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    found.sort();
    found.pop().map(|(_, p)| p)
}

/// Sir's Ultron clip, measured (from its cache when the clip has not changed).
pub fn reference_profile() -> Result<ReferenceProfile, String> {
    let clip = reference_path().ok_or("no Ultron reference clip (assets/ultron_voice_reference.mp3)")?;
    load_profile(&clip, Path::new(PROFILE_PATH))
}

/// The clip's envelope transfer (None without a clip; never in unit tests).
static TRANSFER: LazyLock<Option<Mutex<SpectralTransfer>>> = LazyLock::new(|| {
    if cfg!(test) {
        return None;
    }
    let clip = reference_path()?;
    match load_profile(&clip, Path::new(PROFILE_PATH)) {
        Ok(p) => {
            info!("[ULTRON] Reference clip {}: {}", clip.display(), p.describe());
            *REFERENCE_F0.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.f0);
            Some(Mutex::new(transfer_for(&p)))
        }
        Err(e) => {
            warn!("[ULTRON] Reference clip {}: {e}", clip.display());
            None
        }
    }
});

// ---------------------------------------------------------------------------- the chain ----

/// The second voice: the same words an octave below (the pitch alone is halved, the vowels keep
/// their formants), kept to its body under `OCTAVE_BODY_HZ`. Same length as `voice`.
pub fn octave_voice(voice: &[f32], rate: u32) -> Vec<f32> {
    let r = rate as f32;
    let (mut lp1, mut lp2) = (Biquad::low_pass(OCTAVE_BODY_HZ, r), Biquad::low_pass(OCTAVE_BODY_HZ, r));
    reshape_f0(voice, rate, 0.0, 0.5).into_iter().map(|s| lp2.process(lp1.process(s))).collect()
}

/// The chassis: what of the voice rings in a metal body. Its `BODY` band through two short
/// resonant delays (`CHASSIS`); only the ring, without the voice itself.
pub fn chassis(voice: &[f32], rate: u32) -> Vec<f32> {
    let r = rate as f32;
    let (mut hp, mut lp) = (Biquad::high_pass(BODY.0, r), Biquad::low_pass(BODY.1, r));
    let band: Vec<f32> = voice.iter().map(|s| lp.process(hp.process(*s))).collect();
    let mut out = vec![0.0f32; band.len()];
    for (ms, feedback) in CHASSIS {
        let mut line = vec![0.0f32; (((ms / 1000.0) * r).round() as usize).max(1)];
        let mut at = 0;
        for (o, x) in out.iter_mut().zip(&band) {
            let ring = line[at];
            line[at] = x + feedback * ring;
            at = (at + 1) % line.len();
            *o += ring;
        }
    }
    out
}

/// Grit: the voice's body (140-2800 Hz) driven into a soft clip, which puts overtones between
/// its own. Level-independent: the drive is set against the body's own peak.
pub fn grit(voice: &[f32], rate: u32) -> Vec<f32> {
    let r = rate as f32;
    let (mut hp, mut lp) = (Biquad::high_pass(140.0, r), Biquad::low_pass(2800.0, r));
    let body: Vec<f32> = voice.iter().map(|s| lp.process(hp.process(*s))).collect();
    let peak = crate::dsp::peak(&body).max(1e-6);
    body.into_iter().map(|s| (GRIT_DRIVE * s / peak).tanh() * peak).collect()
}

/// Breath: noise in the band where breath sits that follows the voice's own loudness (4 ms up,
/// 30 ms down), so it is in the voice and not beside it. The same clause gives the same breath
/// twice (the noise is a fixed sequence).
pub fn husk(voice: &[f32], rate: u32) -> Vec<f32> {
    let r = rate as f32;
    // (Two high-passes in a row: breath must not add anything to the chest.)
    let (mut hp, mut hp2, mut lp) = (Biquad::high_pass(HUSK_BAND.0, r), Biquad::high_pass(HUSK_BAND.0, r), Biquad::low_pass(HUSK_BAND.1, r));
    let (up, down) = ((-1.0 / (0.004 * r)).exp(), (-1.0 / (0.030 * r)).exp());
    let mut loudness = 0.0f32;
    let mut seed = 0x2545_F491u32;
    voice
        .iter()
        .map(|s| {
            let now = s.abs();
            loudness = now + if now > loudness { up } else { down } * (loudness - now);
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = seed as f32 / u32::MAX as f32 * 2.0 - 1.0;
            lp.process(hp2.process(hp.process(noise * loudness)))
        })
        .collect()
}

/// A clause of the base voice as Ultron speaks it: Kokoro's synthesis (`from` Hz) in, the
/// finished clause at the master rate out. The clause gets 1 / `FORMANT_RATIO` longer, which
/// `jarvis_voice_dsp::synthesis_speed_as` makes up for.
pub fn voice_of(input: &[f32], from: u32) -> Vec<f32> {
    let mut audio = input.to_vec();
    process(&mut audio, from);
    audio
}

/// `voice_of` in place, with the pitch of Sir's clip once it is loaded.
pub fn process(audio: &mut Vec<f32>, from: u32) {
    let reference = *REFERENCE_F0.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(step) = process_with(audio, from, reference) {
        debug!("[ULTRON] Clause put on the clip's pitch: ×{step:.3} in all, ×{:.3} of it the pitch alone", step / FORMANT_RATIO);
    }
}

/// The chain with the clip's F0 statistics (`reference`) and, on top, the clip's envelope: see
/// `chain`. Returns the pitch step used when it was locked to the clip.
pub fn process_with(audio: &mut Vec<f32>, from: u32, reference: Option<F0Stats>) -> Option<f32> {
    if audio.is_empty() {
        return None;
    }
    let rate = crate::jarvis_voice_dsp::MASTER_RATE;
    let (mut out, locked) = chain(audio, from, reference);
    if let Some(t) = TRANSFER.as_ref() {
        t.lock().unwrap_or_else(|e| e.into_inner()).process(&mut out, rate);
    }
    out.iter_mut().for_each(|s| *s = crate::jarvis_voice_dsp::limit(*s));
    *audio = out;
    locked
}

/// The chain up to (not including) the clip's envelope, from the synthesis at `from` Hz to the
/// master rate. First the larger chest: pitch and formants down together by `FORMANT_RATIO`.
/// Then the pitch alone: with the clip's F0 statistics the clause's median is put on the clip's
/// and its melody narrowed towards the clip's width (by `MAX_FLATTEN` at most); without them
/// the pitch drops by a fixed −2.2 st in all. Then the layers and the level. Returns the audio
/// and the whole pitch step when it was locked to the clip.
pub fn chain(audio: &[f32], from: u32, reference: Option<F0Stats>) -> (Vec<f32>, Option<f32>) {
    let rate = crate::jarvis_voice_dsp::MASTER_RATE;
    let lowered = crate::jarvis_voice_dsp::hermite_resample(audio, from as f64 * FORMANT_RATIO as f64 / rate as f64);
    let clause = reference.and_then(|r| Some((r, f0_stats(&pitch_track(&lowered, rate))?)));
    let (step, flatten, locked) = match clause {
        Some((r, c)) => {
            let ratio = lock_ratio(c.median_hz, r.median_hz);
            (ratio, flatten_to(c.std_st, r.std_st).min(MAX_FLATTEN), Some(ratio * FORMANT_RATIO))
        }
        None => (2f32.powf(PITCH_SEMITONES / 12.0) / FORMANT_RATIO, 0.0, None),
    };
    let voice = reshape_f0(&lowered, rate, flatten, step);
    (levelled(layers(&voice, rate), rate), locked)
}

/// The voice with its layers: the second voice an octave below, the chassis' ring, the grit and
/// the breath, each at its level against the voice's own.
fn layers(voice: &[f32], rate: u32) -> Vec<f32> {
    let level = crate::dsp::rms(voice);
    if level < 1e-6 {
        return voice.to_vec();
    }
    let mut out = voice.to_vec();
    for (layer, mix) in [(octave_voice(voice, rate), OCTAVE_MIX), (chassis(voice, rate), CHASSIS_MIX), (grit(voice, rate), GRIT_MIX), (husk(voice, rate), HUSK_MIX)] {
        // (A layer with nothing in it, as under a pure test tone, adds nothing.)
        let gain = (mix * level / crate::dsp::rms(&layer).max(1e-6)).min(8.0);
        for (o, l) in out.iter_mut().zip(&layer) {
            *o += gain * l;
        }
    }
    out
}

/// His tone, as measured against Sir's clip band by band (2026-11-06): against any base voice
/// the clip has more chest at the fundamental (60-100 Hz), a little less body around 300 Hz, a
/// deep dip where sibilance sits (4-6.3 kHz) and more air above it. (frequency Hz, Q, gain dB);
/// the envelope transfer then only has the rest to do.
pub const TONE: [(f32, f32, f32); 3] = [(82.0, 0.9, 5.0), (300.0, 0.7, -2.0), (5000.0, 1.3, -6.0)];
/// The air above the dip: (corner Hz, gain dB).
pub const AIR: (f32, f32) = (7000.0, 5.0);

/// Rumble out, his tone, and the clause's level evened: a 40 Hz high-pass, `TONE` and `AIR`,
/// the level Jarvis' chain works at, and the same compressor.
fn levelled(mut audio: Vec<f32>, rate: u32) -> Vec<f32> {
    use crate::jarvis_voice_dsp::Bq;
    let r = rate as f32;
    let mut rumble = Biquad::high_pass(40.0, r);
    let mut tone = TONE.map(|(hz, q, gain)| Bq::peaking(hz, q, gain, r));
    let mut air = Bq::high_shelf(AIR.0, AIR.1, r);
    audio.iter_mut().for_each(|s| *s = air.process(tone.iter_mut().fold(rumble.process(*s), |x, band| band.process(x))));
    let level = crate::dsp::rms(&audio);
    if level > 1e-4 {
        let gain = (PRE_GAIN_RMS / level).clamp(0.5, 2.2);
        audio.iter_mut().for_each(|s| *s *= gain);
    }
    let mut compressor = crate::jarvis_voice_dsp::Compressor::new(rate as f32);
    audio.iter_mut().for_each(|s| *s = compressor.process(*s));
    audio
}

// ------------------------------------------------------------ calibration & verification ----

/// The base voices the calibration tries: Kokoro's American male voices (he is no Englishman),
/// and even blends of the four deepest (the stock blend is one of them).
pub fn candidate_voices() -> Vec<String> {
    const AMERICAN: [&str; 8] = ["am_onyx", "am_fenrir", "am_michael", "am_adam", "am_echo", "am_eric", "am_liam", "am_puck"];
    let mut out: Vec<String> = AMERICAN.iter().map(|v| v.to_string()).collect();
    for (i, a) in AMERICAN[..4].iter().enumerate() {
        for b in &AMERICAN[i + 1..4] {
            out.push(format!("{a}:0.50+{b}:0.50"));
        }
    }
    // The deepest voice leaning either way with the two warm ones: finer steps where the
    // closest voices have been found so far.
    for other in ["am_adam", "am_michael"] {
        for share in ["0.70", "0.30"] {
            let rest = if share == "0.70" { "0.30" } else { "0.70" };
            out.push(format!("am_onyx:{share}+{other}:{rest}"));
        }
    }
    out
}

/// What each semitone the pitch has to be moved on its own costs a base voice in the
/// calibration: a naturally deep voice needs less of it and keeps more of itself.
const SHIFT_COST: f32 = 0.12;

/// What a calibration or verification run needs: the clip's profile, Kokoro, and the chain set
/// as it is while Jarvis runs.
struct Bench {
    reference: ReferenceProfile,
    phonemizer: crate::phonemizer::Phonemizer,
    kokoro: crate::tts::Kokoro,
    /// His speaking rate: Sir's setting at Ultron's pace, as while Ultron is on.
    speed: f32,
}

impl Bench {
    fn new(cfg: &crate::config::JarvisConfig) -> Result<Self, String> {
        Ok(Self {
            reference: reference_profile()?,
            phonemizer: crate::phonemizer::Phonemizer::new(cfg.espeak_path.as_deref()),
            kokoro: crate::tts::Kokoro::load(cfg)?,
            speed: cfg.kokoro_speed * PACE,
        })
    }

    /// `line` in `voice` as the synthesis gives it: his English, his pace.
    fn synthesise(&mut self, line: &str, voice: &str) -> Result<Vec<f32>, String> {
        let ipa = self.phonemizer.phonemize_as(&crate::tts::normalize_for_speech(line), crate::phonemizer::Accent::American, self.kokoro.own_symbols());
        self.kokoro.synthesize(&ipa, voice, crate::jarvis_voice_dsp::synthesis_speed_as(self.speed, true))
    }

    /// `line` in `voice` through the chain, before the clip's envelope is imposed, and the
    /// pitch step that put it on the clip.
    fn speak(&mut self, line: &str, voice: &str) -> Result<(Vec<f32>, f32), String> {
        let audio = self.synthesise(line, voice)?;
        let (out, locked) = chain(&audio, crate::tts::KOKORO_RATE, Some(self.reference.f0));
        Ok((out, locked.unwrap_or(1.0)))
    }

    fn measure(&self, audio: &[f32]) -> Result<Match, String> {
        Match::measure(audio, crate::jarvis_voice_dsp::MASTER_RATE, &self.reference).ok_or_else(|| "the synthesised sentence holds no speech".to_string())
    }

    /// How far `voice` is from the clip over both lines (a voice that only fits one sentence
    /// is no match): the match on the first line and the mean distance, which also counts how
    /// far the pitch alone had to be moved (`SHIFT_COST`).
    fn fit(&mut self, voice: &str) -> Result<(Match, f32), String> {
        let mut measured = Vec::with_capacity(2);
        for line in [CALIBRATION_LINE, HELD_OUT_LINE] {
            let (audio, step) = self.speak(line, voice)?;
            let moved_st = (12.0 * (step / FORMANT_RATIO).log2()).abs();
            measured.push((self.measure(&audio)?, moved_st));
        }
        let distance = measured.iter().map(|(m, moved)| m.distance() + SHIFT_COST * moved).sum::<f32>() / measured.len() as f32;
        Ok((measured[0].0, distance))
    }
}

/// Picks the base voice whose chain output (before the clip's envelope is imposed) is closest to
/// the Ultron clip, and stores it in the clip's profile cache. A voice already calibrated stays
/// unless another is closer by `KEEP_MARGIN`. Silent; a minute or two of CPU.
pub fn calibrate(cfg: &crate::config::JarvisConfig) -> Result<(String, Match), String> {
    let mut bench = Bench::new(cfg)?;
    let calibrated = bench.reference.blend.clone();
    let mut candidates = candidate_voices();
    if let Some(kept) = calibrated.clone().filter(|v| !candidates.contains(v)) {
        candidates.push(kept);
    }
    // (voice, its match on the first line, its distance over both lines)
    let mut best: Option<(String, Match, f32)> = None;
    let mut kept: Option<(String, Match, f32)> = None;
    for voice in candidates {
        let (m, distance) = match bench.fit(&voice) {
            Ok(fit) => fit,
            Err(e) => {
                warn!("[ULTRON] Calibration: {voice} failed: {e}");
                continue;
            }
        };
        info!("[ULTRON] Calibration {voice:<34} distance {distance:.3}: {}", m.describe());
        if calibrated.as_deref() == Some(voice.as_str()) {
            kept = Some((voice.clone(), m, distance));
        }
        if best.as_ref().is_none_or(|(_, _, b)| distance < *b) {
            best = Some((voice, m, distance));
        }
    }
    let best = best.ok_or("no voice could be synthesised")?;
    let (voice, m, _) = match kept {
        Some(kept) if kept.2 <= best.2 + crate::fish_zero_shot_tts::KEEP_MARGIN => kept,
        _ => best,
    };
    let mut reference = bench.reference;
    reference.blend = Some(voice.clone());
    std::fs::write(PROFILE_PATH, reference.to_bytes()).map_err(|e| format!("{PROFILE_PATH}: {e}"))?;
    info!("[ULTRON] Calibrated base voice: {voice} ({})", m.describe());
    Ok((voice, m))
}

/// The finished Ultron voice against the clip: a sentence through the chain, measured before
/// and after the clip's envelope is imposed, and a second sentence the transfer never saw.
pub fn verify(cfg: &crate::config::JarvisConfig) -> Result<Verification, String> {
    let mut bench = Bench::new(cfg)?;
    let rate = crate::jarvis_voice_dsp::MASTER_RATE;
    let voice = bench.reference.blend.clone().unwrap_or_else(|| VOICE.to_string());
    let mut transfer = transfer_for(&bench.reference);
    let (mut audio, _) = bench.speak(CALIBRATION_LINE, &voice)?;
    let dry = bench.measure(&audio)?;
    transfer.process(&mut audio, rate);
    let transferred = bench.measure(&audio)?;
    let held_out = bench.speak(HELD_OUT_LINE, &voice).ok().and_then(|(mut a, _)| {
        transfer.process(&mut a, rate);
        bench.measure(&a).ok()
    });
    Ok(Verification { voice, dry, transferred, held_out, reference: bench.reference })
}

/// The lines Ultron is measured on (the five sentence types the voices share) in his finished
/// voice, as the speaker gets them: synthesis, chain, the clip's envelope, the limiter. Each
/// with its kind and the text as spoken.
pub fn render_sentences(cfg: &crate::config::JarvisConfig) -> Result<Vec<(&'static str, String, Vec<f32>)>, String> {
    let mut bench = Bench::new(cfg)?;
    let rate = crate::jarvis_voice_dsp::MASTER_RATE;
    let voice = bench.reference.blend.clone().unwrap_or_else(|| VOICE.to_string());
    let mut transfer = transfer_for(&bench.reference);
    let mut out = Vec::new();
    for (kind, text) in crate::fish_zero_shot_tts::VOICE_SENTENCES {
        let (mut audio, _) = bench.speak(text, &voice)?;
        transfer.process(&mut audio, rate);
        audio.iter_mut().for_each(|s| *s = crate::jarvis_voice_dsp::limit(*s));
        out.push((kind, crate::tts::normalize_for_speech(text), audio));
    }
    Ok(out)
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const R: u32 = 48_000;

    /// The command with Ultron off (as these tests leave it).
    fn command(text: &str) -> Option<bool> {
        parse(text, false)
    }

    fn tone(f: f32, secs: f32) -> Vec<f32> {
        (0..(secs * R as f32) as usize).map(|i| 0.4 * (TAU * f * i as f32 / R as f32).sin()).collect()
    }

    /// Zero crossings per second / 2: the frequency of a clean tone.
    fn freq(x: &[f32]) -> f32 {
        let crossings = x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        crossings as f32 / (x.len() as f32 / R as f32)
    }

    #[test]
    fn voice_commands_switch_ultron_on_and_back() {
        for on in ["Jarvis, switch to Ultron mode", "Activate Ultron protocol", "Ultron Modus aktivieren", "engage ultron"] {
            assert_eq!(command(on), Some(true), "{on}");
        }
        for off in ["Switch back to Jarvis", "switch back to normal mode", "Deactivate Ultron mode", "Ultron Modus deaktivieren"] {
            assert_eq!(command(off), Some(false), "{off}");
        }
        // The screenshot transcripts (after the wake word), and the bare forms while Ultron is on.
        for off in ["Ultron switch back to Jarvis", "Change back to Jarvis", "Turn back to Jarvis", "Go back to Jarvis"] {
            assert_eq!(parse(off, true), Some(false), "{off}");
        }
        for off in ["Change back", "Ultron, switch back", "switch back please", "Normal mode", "Jarvis mode", "Ultron switch back to"] {
            assert_eq!(parse(off, true), Some(false), "{off}");
            assert_eq!(parse(off, false), None, "only while Ultron is on: {off}");
        }
        assert_eq!(parse("change back to the previous track", true), None, "a track is still music");
        assert_eq!(command("what is ultron"), None);
        assert_eq!(command("turn the volume up"), None);
        assert_eq!(TERMINATED, "Ultron protocol terminated. I am back online, Sir.");
    }

    #[test]
    fn the_chain_lowers_the_pitch_and_stays_intelligible() {
        // Without a clip: −2.2 st in all, of which only the formant part stretches the clause.
        let step = 2f32.powf(PITCH_SEMITONES / 12.0);
        assert!((step - 0.880).abs() < 0.001);
        assert_eq!(pitch_ratio(), FORMANT_RATIO, "what the Kokoro speed makes up for");
        assert!((crate::jarvis_voice_dsp::synthesis_speed_as(0.9, true) * FORMANT_RATIO - 0.9).abs() < 1e-4, "his chain's stretch alone is made up for");
        let input = tone(220.0, 1.0);
        let mut out = input.clone();
        process(&mut out, R);
        let stretched = out.len() as f32 / input.len() as f32;
        assert!((stretched - 1.0 / FORMANT_RATIO).abs() < 0.01, "the clause stretches by 1 / {FORMANT_RATIO} only: ×{stretched}");
        // (The tone under its breath: the crossings of what is left below 600 Hz.)
        let (mut lp1, mut lp2) = (Biquad::low_pass(600.0, R as f32), Biquad::low_pass(600.0, R as f32));
        let body: Vec<f32> = out.iter().map(|s| lp2.process(lp1.process(*s))).collect();
        let f = freq(&body[4800..body.len() - 4800]);
        assert!((f - 220.0 * step).abs() < 6.0, "−2.2 st: {f} Hz");
        assert!(out.iter().all(|s| s.abs() <= 1.0 && s.is_finite()));
        // From Kokoro's 24 kHz the finished clause is at the master rate, the same way longer.
        let synthesis: Vec<f32> = (0..24_000).map(|i| 0.4 * (TAU * 220.0 * i as f32 / 24_000.0).sin()).collect();
        let finished = voice_of(&synthesis, 24_000);
        assert!((finished.len() as f32 / (2.0 * synthesis.len() as f32) - 1.0 / FORMANT_RATIO).abs() < 0.01);
        // Mostly the voice: its layers stay under it.
        let lowered = voice(96.0, 1.0, 1.0, 900.0);
        let layered = layers(&lowered, R);
        assert_eq!(layered.len(), lowered.len());
        let dot: f32 = lowered.iter().zip(&layered).map(|(a, b)| a * b).sum();
        let norm = (lowered.iter().map(|a| a * a).sum::<f32>() * layered.iter().map(|b| b * b).sum::<f32>()).sqrt();
        assert!(dot / norm > 0.85, "the voice dominates: {}", dot / norm);
    }

    #[test]
    fn under_the_voice_runs_a_second_one_an_octave_below_in_a_ringing_chassis() {
        let spoken = voice(96.0, 1.0, 1.5, 900.0);
        // The second voice: the same words at half the pitch, only its body.
        let below = octave_voice(&spoken, R);
        assert_eq!(below.len(), spoken.len());
        let (env, _) = crate::fish_zero_shot_tts::mel_envelope(&below, R).expect("the layer sounds");
        let centres = crate::fish_zero_shot_tts::band_centres();
        let level = |lo: f32, hi: f32| centres.iter().zip(env.iter()).filter(|(f, _)| **f >= lo && **f < hi).map(|(_, db)| *db).fold(f32::MIN, f32::max);
        assert!(level(100.0, 1000.0) > level(3000.0, 8000.0) + 24.0, "its body, not its edge");
        // Heard together it is a hint of body under the voice (the clip has no second speaker
        // in it): the half-harmonics rise a little, the pitch heard stays the voice's.
        let plain = crate::voice_features::Features::measure(&spoken, R).expect("speech");
        let with_layers = layers(&spoken, R);
        let layered = crate::voice_features::Features::measure(&with_layers, R).expect("speech");
        // (What sounds at half the pitch, 48 Hz, where the voice itself has nothing.)
        let at_half = |x: &[f32]| {
            let (mut c, mut s) = (0.0f32, 0.0f32);
            for (i, v) in x.iter().enumerate() {
                let p = TAU * 48.0 * i as f32 / R as f32;
                c += v * p.cos();
                s += v * p.sin();
            }
            (c * c + s * s).sqrt() / x.len() as f32
        };
        assert!(at_half(&with_layers) > at_half(&spoken) * 3.0 + 1e-5, "{} / {}", at_half(&with_layers), at_half(&spoken));
        assert!(layered.doubled < 0.3, "no second speaker: {}", layered.describe());
        assert!((layered.f0_hz - plain.f0_hz).abs() < 3.0, "the pitch heard is still the voice's: {}", layered.describe());
        // Breath: in its band, only while the voice sounds, and the same every time.
        let mut phrase = voice(96.0, 1.0, 0.5, 900.0);
        phrase.extend(std::iter::repeat_n(0.0, R as usize / 2));
        let breath = husk(&phrase, R);
        assert_eq!(breath, husk(&phrase, R));
        let half = R as usize / 2;
        let (during, after) = (crate::dsp::rms(&breath[..half]), crate::dsp::rms(&breath[half + R as usize / 5..]));
        assert!(during > 0.0 && after < during * 0.01, "breath only with the voice: {during} / {after}");
        let (env, _) = crate::fish_zero_shot_tts::mel_envelope(&breath[..half], R).expect("the breath sounds");
        let level = |lo: f32, hi: f32| centres.iter().zip(env.iter()).filter(|(f, _)| **f >= lo && **f < hi).map(|(_, db)| *db).fold(f32::MIN, f32::max);
        assert!(level(2000.0, 6500.0) > level(80.0, 500.0) + 18.0, "where breath sits, not in the chest: {} / {}", level(2000.0, 6500.0), level(80.0, 500.0));
        // The chassis rings on after a click, at its two delays, and dies away.
        let mut click = vec![0.0f32; R as usize / 4];
        click[100] = 1.0;
        let ring = chassis(&click, R);
        let energy = |from_ms: f32, to_ms: f32| ring[100 + (from_ms * 48.0) as usize..100 + (to_ms * 48.0) as usize].iter().map(|s| s * s).sum::<f32>();
        assert!(energy(0.0, 2.5) < energy(2.5, 9.0) * 0.05, "nothing before the first reflection");
        assert!(energy(2.5, 9.0) > 0.0 && energy(9.0, 30.0) > energy(2.5, 9.0) * 0.05, "it rings on");
        assert!(energy(100.0, 150.0) < energy(2.5, 9.0) * 1e-4, "and dies away");
        // Grit puts overtones between a tone's own: a 300 Hz tone gains its third.
        let pure = tone(300.0, 0.5);
        let rough = grit(&pure, R);
        let third = |x: &[f32]| {
            let (mut c, mut s) = (0.0f32, 0.0f32);
            for (i, v) in x.iter().enumerate().skip(4800) {
                let p = TAU * 900.0 * i as f32 / R as f32;
                c += v * p.cos();
                s += v * p.sin();
            }
            (c * c + s * s).sqrt() / (x.len() - 4800) as f32
        };
        assert!(third(&rough) > third(&pure) * 20.0 + 1e-4, "{} / {}", third(&rough), third(&pure));
    }

    /// A voiced test clause: a pulse train (rich harmonics, like a voice) whose pitch wobbles
    /// ±`wobble_st` semitones at 3 Hz, through one formant at `formant_hz` (0 = none).
    fn voice(f0: f32, wobble_st: f32, secs: f32, formant_hz: f32) -> Vec<f32> {
        let mut phase = 0.0f32;
        let mut formant = crate::jarvis_voice_dsp::Bq::peaking(formant_hz.max(100.0), 6.0, if formant_hz > 0.0 { 24.0 } else { 0.0 }, R as f32);
        (0..(secs * R as f32) as usize)
            .map(|i| {
                let t = i as f32 / R as f32;
                let f = f0 * 2f32.powf(wobble_st * (TAU * 3.0 * t).sin() / 12.0);
                phase = (phase + f / R as f32).fract();
                formant.process((0..24).map(|h| (TAU * phase * (h + 1) as f32).sin() / (h + 1) as f32).sum::<f32>() * 0.1)
            })
            .collect()
    }

    /// Where a formant between `lo` and `hi` sits: the centre of gravity (Hz) of the long-term
    /// spectrum of `x` in that range.
    fn formant_centre(x: &[f32], lo: f32, hi: f32) -> f32 {
        let (env, _) = crate::fish_zero_shot_tts::mel_envelope(x, R).expect("speech");
        let centres = crate::fish_zero_shot_tts::band_centres();
        let (mut sum, mut weight) = (0.0f32, 0.0f32);
        for (f, db) in centres.iter().zip(env.iter()).filter(|(f, _)| **f >= lo && **f <= hi) {
            let power = 10f32.powf(db / 10.0);
            sum += f * power;
            weight += power;
        }
        sum / weight
    }

    #[test]
    fn ultron_is_pitched_onto_the_clips_f0_not_a_fixed_shift() {
        // Sir's clip: median 88.5 Hz, intonation 2.45 st (jarvis.log, 2026-11-03).
        let clip = F0Stats { mean_hz: 89.9, median_hz: 88.5, std_st: 2.45, voiced_ratio: 0.12 };
        assert!((lock_ratio(120.0, 88.5) - 0.7375).abs() < 0.001);
        assert!((lock_ratio(400.0, 88.5) - 2f32.powf(-7.0 / 12.0)).abs() < 1e-4, "never more than −7 st");
        assert_eq!(flatten_to(2.0, 2.45), 0.0, "already narrower than the clip");
        assert!((flatten_to(4.9, 2.45) - 0.5).abs() < 1e-4);
        // A 120 Hz clause comes out at the clip's 88.5 Hz (the fixed −2.2 st gave 105.6 Hz).
        let mut out = voice(120.0, 0.0, 1.2, 0.0);
        let ratio = process_with(&mut out, R, Some(clip)).expect("locked to the clip");
        let got = f0_stats(&pitch_track(&out, R)).unwrap().median_hz;
        assert!((got - 88.5).abs() < 3.0, "median F0 {got} Hz (ratio {ratio})");
        // A lively clause (±6 st) is narrowed towards the clip's width.
        let lively = voice(120.0, 6.0, 1.5, 0.0);
        let before = f0_stats(&pitch_track(&lively, R)).unwrap().std_st;
        let mut out = lively.clone();
        process_with(&mut out, R, Some(clip));
        let after = f0_stats(&pitch_track(&out, R)).unwrap().std_st;
        assert!(after < before * 0.85, "intonation {before:.2} → {after:.2} st");
        // Without the clip: the fixed −2.2 st, nothing claimed.
        let mut out = voice(120.0, 0.0, 0.5, 0.0);
        assert_eq!(process_with(&mut out, R, None), None);
    }

    #[test]
    fn the_pitch_drops_to_the_clip_but_the_vowels_keep_their_formants() {
        // "That's not Ultron's voice": the whole step used to be a resampling, which moved a
        // 1200 Hz formant to 885 Hz along with the pitch. Now only `FORMANT_RATIO` of it does.
        let clip = F0Stats { mean_hz: 89.9, median_hz: 88.5, std_st: 2.45, voiced_ratio: 0.12 };
        let vowel = voice(120.0, 0.0, 1.5, 1200.0);
        let before = formant_centre(&vowel, 700.0, 1700.0);
        assert!((before - 1200.0).abs() < 60.0, "the test vowel's formant: {before} Hz");
        let (out, locked) = chain(&vowel, R, Some(clip));
        let step = locked.expect("locked to the clip");
        assert!((step - 0.7375).abs() < 0.02, "the pitch goes all the way: ×{step}");
        let pitch = f0_stats(&pitch_track(&out, R)).unwrap().median_hz;
        assert!((pitch - 88.5).abs() < 3.0, "median F0 {pitch} Hz");
        let after = formant_centre(&out, 700.0, 1700.0);
        let kept = after / before;
        assert!((kept - FORMANT_RATIO).abs() < 0.05, "the formant moved ×{kept:.3} ({before:.0} → {after:.0} Hz), not ×{step:.3} with the pitch");
        // And the clause is only 1 / FORMANT_RATIO longer, so Kokoro need not rush to make up.
        let stretched = out.len() as f32 / vowel.len() as f32;
        assert!((stretched - 1.0 / FORMANT_RATIO).abs() < 0.01, "×{stretched}");
        // The candidates the calibration tries are distinct, and the stock blend is among them.
        let voices = candidate_voices();
        assert!(voices.contains(&VOICE.to_string()) && voices.iter().any(|v| v == "am_onyx"));
        let mut unique = voices.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), voices.len());
    }

    /// The base voice against Sir's real clip, then the finished voice measured against it:
    /// `cargo test calibrate_the_ultron_voice -- --ignored --nocapture` (silent; writes
    /// `assets/ultron_reference_profile.bin`, which the app reads when Ultron is next engaged).
    #[test]
    #[ignore = "calibrates against the real clip and writes its profile"]
    fn calibrate_the_ultron_voice() {
        let cfg = crate::config::JarvisConfig {
            kokoro_model_path: crate::models::KOKORO_V1_MODEL.into(),
            kokoro_voices_archive_path: crate::models::KOKORO_V1_VOICES.into(),
            tts_gpu: false,
            ..Default::default()
        };
        let (voice, m) = calibrate(&cfg).expect("calibration runs");
        eprintln!("calibrated {voice}: {} (distance {:.3})", m.describe(), m.distance());
        let v = verify(&cfg).expect("verification runs");
        eprintln!("{}", v.describe());
    }

    /// Both clips and both finished voices side by side, as measured (`voice_features`), and
    /// every candidate base voice as the synthesis gives it:
    /// `cargo test ultron_voice_lab -- --ignored --nocapture` (silent; nothing is written).
    #[test]
    #[ignore = "measures the real clips and synthesises with the real voice pack"]
    fn ultron_voice_lab() {
        use crate::voice_features::{apart, Features};
        let cfg = crate::config::JarvisConfig {
            kokoro_model_path: crate::models::KOKORO_V1_MODEL.into(),
            kokoro_voices_archive_path: crate::models::KOKORO_V1_VOICES.into(),
            tts_gpu: false,
            ..Default::default()
        };
        let rate = crate::jarvis_voice_dsp::MASTER_RATE;
        let of_clip = |path: Option<PathBuf>| path.and_then(|p| crate::voice_reference::decode(&p).ok()).and_then(|(pcm, r)| Features::measure(&pcm, r));
        let all = |sentences: Vec<(&'static str, String, Vec<f32>)>| -> Vec<f32> { sentences.into_iter().flat_map(|(_, _, audio)| audio).collect() };
        let jarvis_clip = of_clip(crate::voice_reference::reference_path()).expect("Jarvis' clip");
        let ultron_clip = of_clip(reference_path()).expect("Ultron's clip");
        let jarvis_audio = all(crate::fish_zero_shot_tts::render_sentences(&cfg).expect("Jarvis' voice"));
        let ultron_sentences = render_sentences(&cfg).expect("Ultron's voice");
        for (kind, _, audio) in &ultron_sentences {
            eprintln!("Ultron, {kind:<9}: {}", Match::measure(audio, rate, &reference_profile().unwrap()).map_or("no speech".to_string(), |m| m.describe()));
        }
        let ultron_audio = all(ultron_sentences);
        let jarvis = Features::measure(&jarvis_audio, rate).expect("Jarvis speaks");
        let ultron = Features::measure(&ultron_audio, rate).expect("Ultron speaks");
        eprintln!("Jarvis' clip  : {}", jarvis_clip.describe());
        eprintln!("Jarvis' voice : {}", jarvis.describe());
        eprintln!("Ultron's clip : {}", ultron_clip.describe());
        eprintln!("Ultron's voice: {}", ultron.describe());
        eprintln!("apart: the two voices {:.2}; the two clips {:.2}; Ultron from his clip {:.2}, from Jarvis' clip {:.2}; Jarvis from his clip {:.2}, from Ultron's clip {:.2}", apart(&jarvis, &ultron), apart(&jarvis_clip, &ultron_clip), apart(&ultron, &ultron_clip), apart(&ultron, &jarvis_clip), apart(&jarvis, &jarvis_clip), apart(&jarvis, &ultron_clip));
        // The long-term envelopes, crosswise.
        let envelope = |audio: &[f32]| crate::fish_zero_shot_tts::mel_envelope(audio, rate).expect("speech").0;
        let jarvis_profile = load_profile(&crate::voice_reference::reference_path().unwrap(), Path::new(crate::fish_zero_shot_tts::PROFILE_PATH)).unwrap();
        let ultron_profile = reference_profile().unwrap();
        let similar = crate::fish_zero_shot_tts::spectral_similarity;
        eprintln!(
            "envelope similarity: Ultron's voice to his clip {:.3}, to Jarvis' clip {:.3}; Jarvis' voice to his clip {:.3}, to Ultron's clip {:.3}; the clips to each other {:.3}",
            similar(&envelope(&ultron_audio), &ultron_profile.envelope_db),
            similar(&envelope(&ultron_audio), &jarvis_profile.envelope_db),
            similar(&envelope(&jarvis_audio), &jarvis_profile.envelope_db),
            similar(&envelope(&jarvis_audio), &ultron_profile.envelope_db),
            similar(&ultron_profile.envelope_db, &jarvis_profile.envelope_db)
        );
        // Where his finished voice and the chain before the transfer stand against the clip,
        // band by band (dB, each envelope's own mean removed).
        let mut bench = Bench::new(&cfg).expect("the bench");
        let voice = bench.reference.blend.clone().unwrap_or_else(|| VOICE.to_string());
        let dry: Vec<f32> = crate::fish_zero_shot_tts::VOICE_SENTENCES.iter().flat_map(|(_, text)| bench.speak(text, &voice).expect("the chain").0).collect();
        let centres = crate::fish_zero_shot_tts::band_centres();
        let shape = |env: &[f32; crate::fish_zero_shot_tts::MEL_BANDS]| -> Vec<f32> {
            let mean = env.iter().sum::<f32>() / env.len() as f32;
            env.iter().map(|v| v - mean).collect()
        };
        let (clip, before, after) = (shape(&ultron_profile.envelope_db), shape(&envelope(&dry)), shape(&envelope(&ultron_audio)));
        for (lo, hi) in [(60.0, 100.0), (100.0, 160.0), (160.0, 250.0), (250.0, 400.0), (400.0, 630.0), (630.0, 1000.0), (1000.0, 1600.0), (1600.0, 2500.0), (2500.0, 4000.0), (4000.0, 6300.0), (6300.0, 12000.0)] {
            let mean = |v: &[f32]| {
                let inside: Vec<f32> = centres.iter().zip(v).filter(|(f, _)| **f >= lo && **f < hi).map(|(_, x)| *x).collect();
                inside.iter().sum::<f32>() / inside.len().max(1) as f32
            };
            eprintln!("{lo:>6.0}-{hi:<6.0} Hz: clip {:>6.1} dB, chain {:>+6.1} dB off, finished {:>+6.1} dB off", mean(&clip), mean(&before) - mean(&clip), mean(&after) - mean(&clip));
        }
        // Each candidate as the synthesis gives it, and what the chain makes of it.
        let line = "The reactor core is under water, Sir.";
        eprintln!("British : {}", bench.phonemizer.phonemize(line));
        eprintln!("American: {}", bench.phonemizer.phonemize_as(line, crate::phonemizer::Accent::American, bench.kokoro.own_symbols()));
        for voice in candidate_voices() {
            let raw = bench.synthesise(CALIBRATION_LINE, &voice).expect("synthesis");
            let natural = f0_stats(&pitch_track(&raw, crate::tts::KOKORO_RATE)).map_or(0.0, |s| s.median_hz);
            let (m, distance) = bench.fit(&voice).expect("fit");
            eprintln!("{voice:<30} natural F0 {natural:>6.1} Hz, distance {distance:.3}: {}", m.describe());
        }
    }
}
