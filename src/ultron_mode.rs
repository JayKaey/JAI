//! Ultron Mode ("Jarvis, switch to Ultron mode", "Activate Ultron protocol", "Ultron Modus
//! aktivieren", the header's `[ ULTRON ]`): the whole HUD fades to the blood-crimson Ultron
//! palette (`theme_engine::set_ultron`, the stored theme untouched), the emblem reads
//! `U.L.T.R.O.N.`, and the voice becomes a deep, deliberate baritone (`base_voice()` at `PACE`)
//! through the Ultron chain (`process`):
//!
//! * the pitch goes down to the median F0 of Sir's Ultron clip (measured: 88.5 Hz) in two parts:
//!   8 % by resampling, which lowers the formants with it (a larger chest, `FORMANT_RATIO`), and
//!   the rest as a move of the pitch alone (`fish_zero_shot_tts::reshape_f0`, TD-PSOLA), so the
//!   vowels stay vowels. Reaching the clip's pitch by resampling alone dragged every formant down
//!   by a quarter and had Kokoro speak a third too fast to make up for the stretch: that was the
//!   voice Sir called "not Ultron's".
//! * a 14 % sub-octave chest layer and a 12 % metallic ±9-cent double through a 650-3200 Hz body;
//! * the envelope of the clip (`assets/ultron_voice_reference.mp3`, any `ultron*.mp3|wav` next to
//!   JARVIS.exe), imposed by the same 128-band transfer the Jarvis voice uses.
//!
//! The base voice is picked by measurement (`calibrate`, `--voice-calibrate`): the Kokoro voice
//! or blend whose chain output is closest to the clip, stored in the clip's profile cache.
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

/// The base voice until `calibrate` has picked one: 70 % am_adam + 30 % bm_lewis.
pub const VOICE: &str = "am_adam:0.7+bm_lewis:0.3";
/// Ultron's deliberate pace (× Sir's speaking rate).
pub const PACE: f32 = 0.93;
/// −2.2 semitones: the pitch step without a clip to lock to.
pub const PITCH_SEMITONES: f32 = -2.2;
/// How much lower the formants sit than the base voice's (a larger chest). The rest of the way
/// down to the clip's pitch moves the pitch alone.
pub const FORMANT_RATIO: f32 = 0.92;
pub const SUB_MIX: f32 = 0.14;
pub const DOUBLE_MIX: f32 = 0.12;
/// The metallic double: (cents, delay ms) per voice.
pub const DOUBLES: [(f32, f32); 2] = [(9.0, 4.5), (-9.0, 8.2)];
/// The chassis body the double runs through (Hz).
pub const BODY: (f32, f32) = (650.0, 3200.0);
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
            Some(Mutex::new(SpectralTransfer::new(&p)))
        }
        Err(e) => {
            warn!("[ULTRON] Reference clip {}: {e}", clip.display());
            None
        }
    }
});

// ---------------------------------------------------------------------------- the chain ----

/// Delays `x` by `ms` (zeros in front, same length).
fn delayed(x: &[f32], ms: f32, rate: u32) -> Vec<f32> {
    let d = ((ms / 1000.0) * rate as f32).round() as usize;
    let mut y = vec![0.0; d.min(x.len())];
    y.extend_from_slice(&x[..x.len() - y.len()]);
    y
}

/// The sub-octave chest layer: a flip-flop on the voice's low band (≤ 300 Hz) runs at half its
/// fundamental, follows its envelope and is smoothed into the 55-110 Hz band.
pub fn sub_octave(x: &[f32], rate: u32) -> Vec<f32> {
    let r = rate as f32;
    let (mut low, mut low2) = (Biquad::low_pass(300.0, r), Biquad::low_pass(300.0, r));
    let (mut hp, mut lp1, mut lp2) = (Biquad::high_pass(55.0, r), Biquad::low_pass(110.0, r), Biquad::low_pass(110.0, r));
    let (mut env, mut prev, mut flip) = (0.0f32, 0.0f32, 1.0f32);
    let release = (-1.0 / (0.03 * r)).exp();
    x.iter()
        .map(|&s| {
            let l = low2.process(low.process(s));
            if prev <= 0.0 && l > 0.0 {
                flip = -flip;
            }
            prev = l;
            env = l.abs().max(env * release);
            lp2.process(lp1.process(hp.process(flip * env)))
        })
        .collect()
}

/// One clause through the Ultron chain (48 kHz in and out; the clause gets 1 / `FORMANT_RATIO`
/// longer, which `PACE` and `synthesis_speed` account for).
pub fn process(audio: &mut Vec<f32>, rate: u32) {
    let reference = *REFERENCE_F0.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(step) = process_with(audio, rate, reference) {
        debug!("[ULTRON] Clause put on the clip's pitch: ×{step:.3} in all, ×{:.3} of it the pitch alone", step / FORMANT_RATIO);
    }
}

/// The chain with the clip's F0 statistics (`reference`) and, on top, the clip's envelope: see
/// `chain`. Returns the pitch step used when it was locked to the clip.
pub fn process_with(audio: &mut Vec<f32>, rate: u32, reference: Option<F0Stats>) -> Option<f32> {
    if audio.is_empty() {
        return None;
    }
    let (mut out, locked) = chain(audio, rate, reference);
    if let Some(t) = TRANSFER.as_ref() {
        t.lock().unwrap_or_else(|e| e.into_inner()).process(&mut out, rate);
    }
    out.iter_mut().for_each(|s| *s = crate::jarvis_voice_dsp::limit(*s));
    *audio = out;
    locked
}

/// The chain up to (not including) the clip's envelope. With the clip's F0 statistics the
/// clause's intonation is narrowed to the clip's width and its median F0 put on the clip's
/// median; without them the pitch drops by a fixed −2.2 st. `FORMANT_RATIO` of that step lowers
/// the formants too, the rest moves the pitch alone. Then the chest layer and the metallic
/// double. Returns the audio and the whole pitch step when it was locked to the clip.
pub fn chain(audio: &[f32], rate: u32, reference: Option<F0Stats>) -> (Vec<f32>, Option<f32>) {
    let clause = reference.and_then(|r| Some((r, f0_stats(&pitch_track(audio, rate))?)));
    let (step, flatten, locked) = match clause {
        Some((r, c)) => {
            let ratio = lock_ratio(c.median_hz, r.median_hz);
            (ratio, flatten_to(c.std_st, r.std_st), Some(ratio))
        }
        None => (2f32.powf(PITCH_SEMITONES / 12.0), 0.0, None),
    };
    // The pitch alone first (the formants untouched), then the part that makes the chest larger.
    let pitched = reshape_f0(audio, rate, flatten, step / FORMANT_RATIO);
    let dry = crate::jarvis_voice_dsp::hermite_resample(&pitched, FORMANT_RATIO as f64);
    (layers(&dry, rate), locked)
}

/// The chest layer (a sub-octave under the voice) and the metallic double, on the lowered voice.
fn layers(dry: &[f32], rate: u32) -> Vec<f32> {
    let sub = sub_octave(dry, rate);
    let sub_peak = crate::dsp::peak(&sub).max(1e-6);
    let dry_rms = crate::dsp::rms(dry);
    let sub_gain = SUB_MIX * dry_rms / crate::dsp::rms(&sub).max(1e-6);
    let mut out: Vec<f32> = dry.iter().zip(&sub).map(|(d, s)| (1.0 - DOUBLE_MIX) * d + (s * sub_gain).clamp(-sub_peak * sub_gain, sub_peak * sub_gain)).collect();
    for (cents, ms) in DOUBLES {
        let step = 2f64.powf(cents as f64 / 1200.0);
        let mut double = crate::jarvis_voice_dsp::hermite_resample(dry, step);
        double.resize(dry.len(), 0.0);
        let (mut hp, mut lp) = (Biquad::high_pass(BODY.0, rate as f32), Biquad::low_pass(BODY.1, rate as f32));
        let body: Vec<f32> = delayed(&double, ms, rate).into_iter().map(|s| lp.process(hp.process(s))).collect();
        for (o, b) in out.iter_mut().zip(body) {
            *o += DOUBLE_MIX / DOUBLES.len() as f32 * b * 2.0;
        }
    }
    out
}

// ------------------------------------------------------------ calibration & verification ----

/// The base voices the calibration tries: Kokoro's deep male voices, even blends of the deepest
/// four, and the stock blend.
pub fn candidate_voices() -> Vec<String> {
    const DEEP: [&str; 8] = ["am_onyx", "am_fenrir", "am_michael", "am_adam", "am_echo", "am_eric", "bm_george", "bm_lewis"];
    let mut out: Vec<String> = DEEP.iter().map(|v| v.to_string()).collect();
    for (i, a) in DEEP[..4].iter().enumerate() {
        for b in &DEEP[i + 1..4] {
            out.push(format!("{a}:0.50+{b}:0.50"));
        }
    }
    out.push(VOICE.to_string());
    out
}

/// What a calibration or verification run needs: the clip's profile, Kokoro, and the chain set
/// as it is while Jarvis runs.
struct Bench {
    reference: ReferenceProfile,
    phonemizer: crate::phonemizer::Phonemizer,
    kokoro: crate::tts::Kokoro,
    /// The speed handed to `synth_dry`, so Kokoro speaks as it does while Ultron is on (his pace,
    /// and the chain's stretch made up for) whether or not Ultron is on right now.
    speed: f32,
}

impl Bench {
    fn new(cfg: &crate::config::JarvisConfig) -> Result<Self, String> {
        let reference = reference_profile()?;
        // The F0 flattening of the shared first stages, as the running Jarvis sets it.
        let flatten = crate::voice_reference::reference_path()
            .and_then(|clip| load_profile(&clip, Path::new(crate::fish_zero_shot_tts::PROFILE_PATH)).ok())
            .and_then(|p| p.flatten);
        crate::fish_zero_shot_tts::set_f0_flatten(crate::fish_zero_shot_tts::effective_f0_flatten(cfg.voice_f0_flatten, flatten));
        let stretch = if active() { 1.0 } else { FORMANT_RATIO };
        Ok(Self {
            reference,
            phonemizer: crate::phonemizer::Phonemizer::new(cfg.espeak_path.as_deref()),
            kokoro: crate::tts::Kokoro::load(cfg)?,
            speed: cfg.kokoro_speed * PACE / stretch,
        })
    }

    /// `line` in `voice` through the chain, before the clip's envelope is imposed.
    fn speak(&mut self, line: &str, voice: &str) -> Result<Vec<f32>, String> {
        let audio = crate::fish_zero_shot_tts::synth_dry(&mut self.kokoro, &self.phonemizer, line, voice, self.speed)?;
        Ok(chain(&audio, crate::jarvis_voice_dsp::MASTER_RATE, Some(self.reference.f0)).0)
    }

    fn measure(&self, audio: &[f32]) -> Result<Match, String> {
        Match::measure(audio, crate::jarvis_voice_dsp::MASTER_RATE, &self.reference).ok_or_else(|| "the synthesised sentence holds no speech".to_string())
    }

    /// How far `voice` is from the clip over both lines (a voice that only fits one sentence
    /// is no match): the match on the first line and the mean distance.
    fn fit(&mut self, voice: &str) -> Result<(Match, f32), String> {
        let first = self.speak(CALIBRATION_LINE, voice).and_then(|audio| self.measure(&audio))?;
        let second = self.speak(HELD_OUT_LINE, voice).and_then(|audio| self.measure(&audio))?;
        Ok((first, (first.distance() + second.distance()) / 2.0))
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
    let mut transfer = SpectralTransfer::new(&bench.reference);
    let mut audio = bench.speak(CALIBRATION_LINE, &voice)?;
    let dry = bench.measure(&audio)?;
    transfer.process(&mut audio, rate);
    let transferred = bench.measure(&audio)?;
    let held_out = bench.speak(HELD_OUT_LINE, &voice).ok().and_then(|mut a| {
        transfer.process(&mut a, rate);
        bench.measure(&a).ok()
    });
    Ok(Verification { voice, dry, transferred, held_out, reference: bench.reference })
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
    fn the_chain_lowers_the_pitch_adds_a_sub_octave_and_stays_intelligible() {
        // Without a clip: −2.2 st in all, of which only the formant part stretches the clause.
        let step = 2f32.powf(PITCH_SEMITONES / 12.0);
        assert!((step - 0.880).abs() < 0.001);
        assert_eq!(pitch_ratio(), FORMANT_RATIO, "what the Kokoro speed makes up for");
        let input = tone(220.0, 1.0);
        let mut out = input.clone();
        process(&mut out, R);
        let stretched = out.len() as f32 / input.len() as f32;
        assert!((stretched - 1.0 / FORMANT_RATIO).abs() < 0.01, "the clause stretches by 1 / {FORMANT_RATIO} only: ×{stretched}");
        let f = freq(&out[4800..out.len() - 4800]);
        assert!((f - 220.0 * step).abs() < 6.0, "−2.2 st: {f} Hz");
        assert!(out.iter().all(|s| s.abs() <= 1.0 && s.is_finite()));
        // The chest layer runs at half the fundamental.
        let sub = sub_octave(&tone(160.0, 1.0), R);
        let f_sub = freq(&sub[9600..]);
        assert!((f_sub - 80.0).abs() < 4.0, "sub-octave at {f_sub} Hz");
        // Mostly dry: the double and the sub stay under the voice.
        let lowered = tone(176.0, 0.5);
        let layered = layers(&lowered, R);
        assert_eq!(layered.len(), lowered.len());
        let dot: f32 = lowered.iter().zip(&layered).map(|(a, b)| a * b).sum();
        let norm = (lowered.iter().map(|a| a * a).sum::<f32>() * layered.iter().map(|b| b * b).sum::<f32>()).sqrt();
        assert!(dot / norm > 0.85, "the dry voice dominates: {}", dot / norm);
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
}
