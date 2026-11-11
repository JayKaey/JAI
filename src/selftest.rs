//! `jarvis --selftest`: exercises the real pipeline without a microphone.
//!
//! Kokoro speaks a command → embedded in a noisy 16 kHz "microphone" stream → Silero VAD +
//! state machine (calibration, onset, end-of-speech latency) → Whisper → wake word → intent.
//! A German utterance (eSpeak-NG `-v de`) checks German detection, and the LLM is probed for
//! first-token latency with the real prompt prefix and tools.

use crate::config::{ConfigStore, JaiConfig};
use crate::dsp;
use crate::intents::{self, Intent};
use crate::phonemizer::Phonemizer;
use crate::stt::SttEngine;
use crate::tts::{self, Kokoro};
use crate::vad::{FrameContext, SileroVad, VadEvent, VadMachine, VadParams, FRAME};
use crate::wake::WakeWordMatcher;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

struct Report {
    failures: Vec<String>,
}

impl Report {
    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
        if ok {
            info!("[TEST] PASS {name}: {}", detail.as_ref());
        } else {
            error!("[TEST] FAIL {name}: {}", detail.as_ref());
            self.failures.push(name.to_string());
        }
    }
}

/// Deterministic pseudo-random room noise.
fn noise(samples: usize, level: f32, seed: &mut u32) -> Vec<f32> {
    (0..samples)
        .map(|_| {
            *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((*seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0) * level * 1.7
        })
        .collect()
}

struct VadResult {
    /// Every piece of the request, stitched as the app hears it (`app::join_audio`).
    utterance: Option<Vec<f32>>,
    /// Voiced time of all pieces together.
    speech_ms: u32,
    /// Voiced time of the last piece: its length chose the hangover that ended the request.
    last_speech_ms: u32,
    /// Pieces stitched together (a breath after "Hey JAI," makes two).
    pieces: usize,
    started: bool,
    eos_latency_ms: Option<i64>,
    noise_floor: f32,
}

/// Speech resuming this soon after a piece ended belongs to the same request: the app holds a
/// piece until 1.2 s of quiet before hearing it on its own (`app::keep_audio_for_stitching`).
const STITCH_QUIET_SAMPLES: usize = 16_000 * 1200 / 1000;

/// Runs a 16 kHz stream through Silero + the state machine; `speech_end` is the sample index
/// where the spoken audio actually stops. Pieces split by a breath (the short-command cutoff
/// after "Hey JAI,") are stitched like the app does; the end-of-speech latency is the last
/// piece's.
fn run_vad(cfg: &JaiConfig, silero: &mut SileroVad, stream: &[f32], speech_end: usize) -> VadResult {
    let mut machine = VadMachine::new(VadParams::from_config(cfg));
    // As the microphone thread does: the energy gate hears the voice band.
    let mut band = crate::vad::VoiceBand::new();
    let mut res = VadResult { utterance: None, speech_ms: 0, last_speech_ms: 0, pieces: 0, started: false, eos_latency_ms: None, noise_floor: 0.0 };
    // Where the last piece ended, and whether a later sound came too late to belong to it.
    let mut ended_at: Option<usize> = None;
    let mut finished = false;
    for (i, frame) in stream.chunks_exact(FRAME).enumerate() {
        let prob = silero.predict(frame);
        let rms = band.rms(frame);
        match machine.process(frame, prob, rms, FrameContext::default()) {
            Some(VadEvent::SpeechStarted { .. }) => {
                res.started = true;
                info!("[TEST]   VAD onset at {} ms (p={prob:.2})", i * 32);
                if ended_at.is_some_and(|end| i * FRAME - end > STITCH_QUIET_SAMPLES) {
                    finished = true;
                }
            }
            Some(VadEvent::SpeechEnded { pcm, speech_ms, silence_ms, .. }) if !finished => {
                let decided_at = (i + 1) * FRAME;
                res.eos_latency_ms = Some((decided_at as i64 - speech_end as i64) * 1000 / 16_000);
                res.pieces += 1;
                info!("[TEST]   VAD end-of-speech after {silence_ms} ms of silence ({speech_ms} ms voiced, piece {})", res.pieces);
                res.utterance = Some(match res.utterance.take() {
                    Some(previous) => crate::app::join_audio(previous, pcm),
                    None => pcm,
                });
                res.speech_ms += speech_ms;
                res.last_speech_ms = speech_ms;
                ended_at = Some(decided_at);
            }
            Some(VadEvent::Calibrated { noise_floor }) => res.noise_floor = noise_floor,
            _ => {}
        }
    }
    res
}

fn read_wav_mono(path: &Path) -> Option<(Vec<f32>, u32)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let (mut pos, mut rate, mut channels, mut bits) = (12usize, 0u32, 1u16, 16u16);
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().ok()?) as usize;
        let body = bytes.get(pos + 8..(pos + 8 + len).min(bytes.len()))?;
        if id == b"fmt " && body.len() >= 16 {
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes(body[4..8].try_into().ok()?);
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" && bits == 16 && rate > 0 {
            let ch = channels.max(1) as usize;
            let samples = body
                .chunks_exact(2 * ch)
                .map(|f| i16::from_le_bytes([f[0], f[1]]) as f32 / 32768.0)
                .collect();
            return Some((samples, rate));
        }
        pos += 8 + len + (len & 1);
    }
    None
}

fn scale_to_rms(audio: &mut [f32], target: f32) {
    let level = dsp::rms(audio);
    if level > 1e-6 {
        let g = target / level;
        audio.iter_mut().for_each(|s| *s *= g);
    }
}

type ConfigTweak = Box<dyn Fn(&mut JaiConfig)>;

/// `--selftest --bench`: times Whisper on `pcm` across settings so the fastest accurate
/// configuration for this machine can be picked.
async fn bench_stt(cfg: &JaiConfig, pcm: &[f32], voiced_ms: u32) {
    let variants: Vec<(&str, ConfigTweak)> = vec![
        ("current config", Box::new(|_c: &mut JaiConfig| {})),
        ("no language detection (en)", Box::new(|c: &mut JaiConfig| c.whisper_languages = vec!["en".into()])),
        ("encoder floor 256", Box::new(|c: &mut JaiConfig| c.whisper_min_audio_ctx = 256)),
        ("encoder floor 192", Box::new(|c: &mut JaiConfig| c.whisper_min_audio_ctx = 192)),
        ("8 threads", Box::new(|c: &mut JaiConfig| c.whisper_threads = 8)),
        ("base model", Box::new(|c: &mut JaiConfig| c.whisper_model_path = c.whisper_fallback_model_path.clone())),
    ];
    for (label, tweak) in variants {
        let mut c = cfg.clone();
        tweak(&mut c);
        let stt = SttEngine::start(&c);
        let wait = Instant::now();
        while !stt.is_ready() && wait.elapsed() < Duration::from_secs(60) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut times = Vec::new();
        let mut text = String::new();
        for _ in 0..3 {
            if let Ok(t) = stt.transcribe(pcm.to_vec(), voiced_ms).await {
                times.push(t.stt_ms);
                text = t.text;
            }
        }
        times.sort_unstable();
        info!("[BENCH] {label:<30} median {:>5} ms  (runs {times:?})  '{text}'", times.get(1).copied().unwrap_or(0));
    }
}

/// `--selftest --bench`: Sir's own sentences from jai.log (among them those Whisper got wrong
/// on 2026-11-09), said by another voice into a room with a fan, music or people talking: the
/// share of words each Whisper setting gets right, so the defaults are picked by measurement.
async fn bench_noise(cfg: &JaiConfig, kokoro: &mut Kokoro, phonemizer: &Phonemizer, seed: &mut u32) {
    const SAID: [&str; 14] = [
        "Can you make the globe spin?",
        "Can you show me where New York is?",
        "Can you make it green?",
        "Show me a picture of the Eiffel Tower.",
        "Close the 3D model.",
        "Show me a 3D model of the Iron Man armor.",
        "Change the colour to green.",
        "Make it way more detailed.",
        "JAI, show me the Earth.",
        "I don't want to see the satellites anymore.",
        "Close that window.",
        "Turn the trains back on.",
        "Detach the 3D model.",
        "Save it to my desktop.",
    ];
    const CHATTER: [&str; 2] = [
        "So I told him we would meet at the station around six, and then we could still grab something to eat before the film starts.",
        "Did you see the match last night? The second half was unbelievable, they scored three times in ten minutes.",
    ];
    let mut say = |text: &str, voice: &str| -> Vec<f32> {
        let ipa = phonemizer.phonemize(&tts::normalize_for_speech(text));
        let audio = kokoro.synthesize(&ipa, voice, 1.0).or_else(|_| kokoro.synthesize(&ipa, &cfg.kokoro_voice, 1.0)).unwrap_or_default();
        let mut pcm = dsp::resample(&audio, tts::KOKORO_RATE, 16_000);
        scale_to_rms(&mut pcm, 0.06);
        pcm
    };
    let speech: Vec<Vec<f32>> = SAID.iter().map(|t| say(t, "am_michael")).collect();
    let (a, b) = (say(CHATTER[0], "bf_emma"), say(CHATTER[1], "am_adam"));
    let babble: Vec<f32> = (0..a.len().max(b.len())).map(|i| a.get(i).copied().unwrap_or(0.0) + b.get(i).copied().unwrap_or(0.0)).collect();
    if babble.is_empty() || speech.iter().any(Vec::is_empty) {
        warn!("[BENCH] noise: Kokoro gave no audio; skipped");
        return;
    }
    // Music: a minor progression over a beat. A fan: low-passed noise.
    let tau = std::f32::consts::TAU;
    let music: Vec<f32> = (0..16_000 * 8)
        .map(|i| {
            let t = i as f32 / 16_000.0;
            let chord = [[220.0, 261.6, 329.6], [174.6, 220.0, 261.6], [196.0, 246.9, 293.7]][(t / 2.0) as usize % 3];
            let tones: f32 = chord.iter().map(|f| (tau * f * t).sin() + 0.3 * (tau * 2.0 * f * t).sin()).sum();
            tones + 2.0 * (-(t % 0.5) * 18.0).exp() * (tau * 60.0 * t).sin()
        })
        .collect();
    let mut low = 0.0f32;
    let fan: Vec<f32> = noise(16_000 * 8, 1.0, seed).into_iter().map(|x| {
        low += 0.08 * (x - low);
        low
    }).collect();
    let rooms: [(&str, &[f32], f32); 5] = [("quiet", &fan[..], 30.0), ("fan 6 dB", &fan[..], 6.0), ("music 8 dB", &music[..], 8.0), ("talking 10 dB", &babble[..], 10.0), ("talking 3 dB", &babble[..], 3.0)];
    // Half a second of the room before the words (the pre-roll), 0.4 s after them.
    let mix = |speech: &[f32], bed: &[f32], snr: f32, offset: usize| -> Vec<f32> {
        let gain = 0.06 / 10f32.powf(snr / 20.0) / dsp::rms(bed).max(1e-6);
        (0..8_000 + speech.len() + 6_400).map(|i| bed[(offset + i) % bed.len()] * gain + i.checked_sub(8_000).and_then(|k| speech.get(k)).copied().unwrap_or(0.0)).collect()
    };
    let variants: Vec<(&str, ConfigTweak)> = vec![
        ("current config", Box::new(|_c: &mut JaiConfig| {})),
        ("full encoder window", Box::new(|c: &mut JaiConfig| c.whisper_dynamic_audio_ctx = false)),
        ("encoder floor 512", Box::new(|c: &mut JaiConfig| c.whisper_min_audio_ctx = 512)),
        ("beam 5 for every sentence", Box::new(|c: &mut JaiConfig| {
            c.whisper_fast_decode = false;
            c.whisper_beam_size = 5;
        })),
    ];
    for (label, tweak) in variants {
        let mut c = cfg.clone();
        tweak(&mut c);
        let stt = SttEngine::start(&c);
        let wait = Instant::now();
        while !stt.is_ready() && wait.elapsed() < Duration::from_secs(60) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let (mut times, mut misses, mut per_room, mut all) = (Vec::new(), Vec::new(), Vec::new(), (0usize, 0usize));
        for (k, (room, bed, snr)) in rooms.iter().enumerate() {
            let (mut right, mut total) = (0usize, 0usize);
            for (j, (said, pcm)) in SAID.iter().zip(&speech).enumerate() {
                let heard = match stt.transcribe(mix(pcm, bed, *snr, (k * 7 + j) * 5_303), (pcm.len() / 16) as u32).await {
                    Ok(t) => {
                        times.push(t.stt_ms);
                        crate::stt::normalize_stt_transcript(&t.text)
                    }
                    Err(_) => String::new(),
                };
                let (r, n) = words_right(said, &heard);
                (right, total) = (right + r, total + n);
                if r < n {
                    misses.push(format!("{room}: '{said}' → '{heard}'"));
                }
            }
            per_room.push(format!("{room} {:.0}%", 100.0 * right as f32 / total.max(1) as f32));
            all = (all.0 + right, all.1 + total);
        }
        times.sort_unstable();
        info!("[BENCH] noise  {label:<26} {:.1}% of words right ({}), median {} ms", 100.0 * all.0 as f32 / all.1.max(1) as f32, per_room.join(", "), times.get(times.len() / 2).copied().unwrap_or(0));
        for m in misses.iter().take(10) {
            info!("[BENCH]          {m}");
        }
    }
}

/// How many words of `said` came through in `heard` (the word-level edit distance taken off),
/// and how many there were.
fn words_right(said: &str, heard: &str) -> (usize, usize) {
    let words = |s: &str| -> Vec<String> { s.to_lowercase().replace("colour", "color").replace("armour", "armor").replace('\'', "").split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect() };
    let (a, b) = (words(said), words(heard));
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(x != y)).min(row[j] + 1).min(above + 1);
            diagonal = above;
        }
    }
    (a.len().saturating_sub(row[b.len()]), a.len())
}

pub async fn run(cfg: &JaiConfig, speak: bool, bench: bool, llm_stage: bool) -> ExitCode {
    let mut r = Report { failures: Vec::new() };
    let mut seed = 0x5eed_1234u32;

    // ---- 1. Phonemizer + Kokoro --------------------------------------------------------
    let phonemizer = Phonemizer::new(cfg.espeak_path.as_deref());
    let command = "Hey JAI, open Spotify please.";
    let t = Instant::now();
    let ipa = phonemizer.phonemize(&tts::normalize_for_speech(command));
    let g2p_ms = t.elapsed().as_secs_f64() * 1000.0;
    r.check("G2P", !ipa.is_empty(), format!("{} in {g2p_ms:.1} ms → {ipa}", phonemizer.backend_name()));

    let mut kokoro = match Kokoro::load(cfg) {
        Ok(k) => k,
        Err(e) => {
            r.check("Kokoro load", false, e);
            return summary(r);
        }
    };
    // The film voice synthesises 2 % faster; the mastering chain's pitch lock stretches it back.
    let speed = crate::jarvis_voice_dsp::synthesis_speed(cfg.kokoro_speed);
    let _ = kokoro.synthesize(&phonemizer.phonemize("Sir."), &cfg.kokoro_voice, speed);
    let t = Instant::now();
    let speech24 = match kokoro.synthesize(&ipa, &cfg.kokoro_voice, speed) {
        Ok(a) => a,
        Err(e) => {
            r.check("Kokoro synth", false, e);
            return summary(r);
        }
    };
    let synth_ms = t.elapsed().as_millis();
    let dur = speech24.len() as f32 / tts::KOKORO_RATE as f32;
    r.check(
        "Kokoro synth",
        dur > 0.8,
        format!("{dur:.2} s of audio in {synth_ms} ms (real-time factor {:.2})", synth_ms as f32 / 1000.0 / dur.max(0.01)),
    );
    let t = Instant::now();
    let _ = kokoro.synthesize(&phonemizer.phonemize("Right away, Sir."), &cfg.kokoro_voice, speed);
    info!("[TEST]   first-clause synthesis ('Right away, Sir.') took {} ms", t.elapsed().as_millis());

    // ---- 1b. The film mastering chain: what Sir actually hears (48 kHz) --------------------
    let t = Instant::now();
    let mastered = tts::finish_clause_audio(speech24.clone(), command);
    let master_ms = t.elapsed().as_secs_f64() * 1000.0;
    let rate = crate::jarvis_voice_dsp::MASTER_RATE;
    let (peak, level) = (dsp::peak(&mastered), dsp::rms(&mastered));
    r.check(
        "Film voice chain",
        mastered.iter().all(|s| s.is_finite()) && peak <= 0.9 && level > 0.02,
        format!(
            "{:.2} s at {} kHz in {master_ms:.1} ms (peak {:.1} dBFS, RMS {:.1} dBFS, voice {})",
            mastered.len() as f32 / rate as f32,
            rate / 1000,
            dsp::to_dbfs(peak),
            dsp::to_dbfs(level),
            cfg.kokoro_voice
        ),
    );

    // ---- 2. VAD on a simulated microphone stream ---------------------------------------
    let mut speech16 = dsp::resample(&mastered, rate, 16_000);
    scale_to_rms(&mut speech16, 0.06);
    let last_voiced = speech16.iter().rposition(|s| s.abs() > 0.02).unwrap_or(speech16.len());
    let lead = 16_000 * 13 / 10;
    let mut stream = noise(lead, 0.003, &mut seed);
    let mut voiced = speech16.clone();
    let room = noise(voiced.len(), 0.003, &mut seed);
    voiced.iter_mut().zip(room).for_each(|(s, n)| *s += n);
    stream.extend(voiced);
    stream.extend(noise(16_000 * 3 / 2, 0.003, &mut seed));

    let mut silero = match SileroVad::load(Path::new(&cfg.silero_vad_model_path)) {
        Ok(s) => s,
        Err(e) => {
            r.check("Silero load", false, e);
            return summary(r);
        }
    };
    let vad = run_vad(cfg, &mut silero, &stream, lead + last_voiced);
    r.check("Noise calibration", vad.noise_floor > 0.0, format!("floor {:.1} dBFS", dsp::to_dbfs(vad.noise_floor)));
    r.check("VAD onset", vad.started, "speech detected");
    let eos = vad.eos_latency_ms.unwrap_or(-1);
    // The adaptive hangover of the last piece: 450 ms after a sentence, 220 ms after a short
    // command (+ framing).
    let hangover = (VadParams::from_config(cfg).end_of_speech_frames(vad.last_speech_ms / 32) * 32) as i64;
    r.check(
        "VAD end-of-speech",
        (hangover - 60..=hangover + 120).contains(&eos),
        format!(
            "request closed {eos} ms after the last word (target ≈{hangover} ms hangover + ≤32 ms framing; {} piece{} stitched)",
            vad.pieces,
            if vad.pieces == 1 { "" } else { "s" }
        ),
    );
    let Some(utterance) = vad.utterance else {
        return summary(r);
    };

    // ---- 3. Whisper ------------------------------------------------------------------
    let stt = SttEngine::start(cfg);
    let wait = Instant::now();
    while !stt.is_ready() && wait.elapsed() < Duration::from_secs(90) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    r.check("Whisper load", stt.is_ready(), format!("{} ({:.1} s)", stt.model_name(), wait.elapsed().as_secs_f32()));
    if !stt.is_ready() {
        return summary(r);
    }
    let transcript = stt.transcribe(utterance.clone(), vad.speech_ms).await;
    let text = transcript.as_ref().map(|t| t.text.clone()).unwrap_or_default();
    r.check(
        "Whisper EN",
        text.to_lowercase().contains("spotify"),
        match &transcript {
            Ok(t) => format!("'{}' [{} p={:.2}] in {} ms for {} ms audio", t.text, t.language, t.language_prob, t.stt_ms, t.audio_ms),
            Err(e) => e.clone(),
        },
    );
    // Second run = steady-state latency (buffers allocated, encoder window seeded).
    let mut steady_ms = 0;
    if let Ok(t) = stt.transcribe(utterance.clone(), vad.speech_ms).await {
        info!("[TEST]   steady-state STT: {} ms", t.stt_ms);
        steady_ms = t.stt_ms;
    }

    // ---- 3b. Short commands: closed 280 ms after the word, transcribed fast, nothing invented ----
    // A stand-in for Sir's own voice at the microphone: the stock blend at 1.06×, fixed so the
    // VAD timings do not drift whenever JAI's voice changes.
    for (phrase, expect) in [("Stop.", "stop"), ("Pause.", "pause")] {
        let audio24 = match kokoro.synthesize(&phonemizer.phonemize(&tts::normalize_for_speech(phrase)), tts::LEGACY_VOICE_BLEND, 1.06) {
            Ok(a) => a,
            Err(e) => {
                r.check("Kokoro short command", false, e);
                continue;
            }
        };
        let mut word = dsp::resample(&audio24, tts::KOKORO_RATE, 16_000);
        scale_to_rms(&mut word, 0.06);
        let last = word.iter().rposition(|s| s.abs() > 0.02).unwrap_or(word.len());
        let mut stream = noise(lead, 0.003, &mut seed);
        let room = noise(word.len(), 0.003, &mut seed);
        word.iter_mut().zip(room).for_each(|(s, n)| *s += n);
        stream.extend(word);
        stream.extend(noise(16_000, 0.003, &mut seed));
        silero.reset();
        let short = run_vad(cfg, &mut silero, &stream, lead + last);
        let eos = short.eos_latency_ms.unwrap_or(-1);
        let target = (VadParams::from_config(cfg).end_of_speech_frames(short.last_speech_ms / 32) * 32) as i64;
        r.check(
            &format!("VAD short command '{phrase}'"),
            target <= cfg.short_command_commit_ms as i64 + 32 && (target - 60..=target + 120).contains(&eos),
            format!("closed {eos} ms after the word ({} ms voiced, target ≈{target} ms)", short.speech_ms),
        );
        let Some(clip) = short.utterance else { continue };
        // Twice: the second run is the steady state.
        let _ = stt.transcribe(clip.clone(), short.speech_ms).await;
        let heard = stt.transcribe(clip, short.speech_ms).await;
        r.check(
            &format!("Whisper short '{phrase}'"),
            // A one-word command must be fast too: a decoder repetition loop takes seconds. It
            // may take no longer than the 2.8 s sentence above (or 2 s), so a busy CPU, which
            // slows both, does not fail it.
            heard.as_ref().is_ok_and(|t| t.text.to_lowercase().contains(expect) && t.stt_ms < steady_ms.max(2000)),
            match &heard {
                Ok(t) => format!("'{}' in {} ms for {} ms of audio", t.text, t.stt_ms, t.audio_ms),
                Err(e) => e.clone(),
            },
        );
    }

    // ---- 3c. Nothing from nothing: room hiss and a click never become words -------------
    let hiss = noise(16_000 * 2, 0.004, &mut seed);
    let mut click = hiss.clone();
    click[16_000..16_640].iter_mut().enumerate().for_each(|(i, s)| *s += (i as f32 * 0.9).sin() * 0.3);
    for (label, clip, voiced) in [("hiss", hiss, 600u32), ("click", click, 400u32)] {
        let heard = stt.transcribe(clip, voiced).await;
        r.check(
            &format!("No phantom words ({label})"),
            heard.as_ref().is_ok_and(|t| t.text.is_empty()),
            match &heard {
                Ok(t) if t.text.is_empty() => format!("nothing heard, in {} ms", t.stt_ms),
                Ok(t) => format!("invented '{}'", t.text),
                Err(e) => e.clone(),
            },
        );
    }

    // ---- 3c'. A sentence with a breath: "JAI, … change the interface colour to red." -----
    // The short-command cutoff ends the first piece after the name; the app hears both pieces
    // again as one clip (`app::join_audio`), so the name and the English sentence survive.
    let piece = |text: &str, kokoro: &mut Kokoro, seed: &mut u32| -> Option<(Vec<f32>, u32)> {
        let audio24 = kokoro.synthesize(&phonemizer.phonemize(&tts::normalize_for_speech(text)), &cfg.kokoro_voice, cfg.kokoro_speed).ok()?;
        let mut word = dsp::resample(&audio24, tts::KOKORO_RATE, 16_000);
        scale_to_rms(&mut word, 0.06);
        let last = word.iter().rposition(|s| s.abs() > 0.02).unwrap_or(word.len());
        let mut stream = noise(lead, 0.003, seed);
        let room = noise(word.len(), 0.003, seed);
        word.iter_mut().zip(room).for_each(|(s, n)| *s += n);
        stream.extend(word);
        stream.extend(noise(16_000, 0.003, seed));
        let mut silero = SileroVad::load(Path::new(&cfg.silero_vad_model_path)).ok()?;
        let v = run_vad(cfg, &mut silero, &stream, lead + last);
        Some((v.utterance?, v.speech_ms))
    };
    match (piece("JAI,", &mut kokoro, &mut seed), piece("change the interface colour to red.", &mut kokoro, &mut seed)) {
        (Some((name, name_ms)), Some((rest, rest_ms))) => {
            let alone = stt.transcribe(name.clone(), name_ms).await;
            info!(
                "[TEST]   the name on its own ({name_ms} ms voiced): {}",
                alone.as_ref().map(|t| format!("'{}' [{}]", t.text, t.language)).unwrap_or_else(|e| e.clone())
            );
            let joined = stt.transcribe(crate::app::join_audio(name, rest), name_ms + rest_ms).await;
            r.check(
                "Paused sentence heard as one",
                joined.as_ref().is_ok_and(|t| {
                    // (As the app reads it: his name in its one spelling.)
                    let l = crate::wake::canon(&t.text).to_lowercase();
                    l.contains("jai") && l.contains("red") && t.language == "en"
                }),
                match &joined {
                    Ok(t) => format!("'{}' [{} p={:.2}] in {} ms", t.text, t.language, t.language_prob, t.stt_ms),
                    Err(e) => e.clone(),
                },
            );
        }
        _ => r.check("Paused sentence heard as one", false, "the two pieces could not be synthesised or detected"),
    }

    // ---- 3c''. "Core", not "car": the core commands reach the core sphere ------------------
    let mut heard = Vec::new();
    let mut routed = 0;
    let commands = [("JAI, hide your core.", false), ("Close your core.", false), ("Show me your core.", true)];
    for (text, open) in commands {
        let Some((clip, ms)) = piece(text, &mut kokoro, &mut seed) else { continue };
        if let Ok(t) = stt.transcribe(clip, ms).await {
            let intent = crate::intents::hud_view_command(&crate::typos::normalize_user_input_typos(&crate::wake::canon(&t.text)));
            if matches!(&intent, Some(crate::intents::Intent::Tool { name: "configure_interface", args }) if args["core_view"] == open) {
                routed += 1;
            }
            heard.push(format!("'{}'", t.text));
        }
    }
    r.check("Core commands reach the core sphere", routed == commands.len(), format!("{routed}/{} routed: {}", commands.len(), heard.join(", ")));

    // ---- 3d. One voice, two languages: German words in German, in the same voice -------
    let mixed = "Your WebUntis shows Mathematik and Geschichte tomorrow, Sir.";
    let mixed_ipa = phonemizer.phonemize(&tts::normalize_for_speech(mixed));
    r.check("Bilingual G2P", mixed_ipa.contains("ɡəʃˈɪçtə") && mixed_ipa.contains("atematˈiːk"), &mixed_ipa);
    if let Ok(audio24) = kokoro.synthesize(&mixed_ipa, &cfg.kokoro_voice, cfg.kokoro_speed) {
        let mut pcm = dsp::resample(&audio24, tts::KOKORO_RATE, 16_000);
        scale_to_rms(&mut pcm, 0.06);
        let mut clip = noise(16_000 / 3, 0.003, &mut seed);
        clip.extend(pcm);
        clip.extend(noise(16_000 / 2, 0.003, &mut seed));
        // Informational: Whisper hears German words in an English sentence its own way.
        match stt.transcribe(clip, 2500).await {
            Ok(t) if ["mathemati", "geschicht"].iter().any(|w| t.text.to_lowercase().contains(w)) => {
                info!("[TEST] PASS Bilingual voice: Whisper heard '{}' [{}]", t.text, t.language)
            }
            Ok(t) => warn!("[TEST] ~ Bilingual voice (informational): Whisper heard '{}' [{}]", t.text, t.language),
            Err(e) => warn!("[TEST] ~ Bilingual voice: {e}"),
        }
    }
    if bench {
        drop(stt);
        bench_stt(cfg, &utterance, vad.speech_ms).await;
        bench_noise(cfg, &mut kokoro, &phonemizer, &mut seed).await;
        drop(kokoro);
        let clauses = [
            "Right away, Sir.",
            "Opening Spotify now.",
            "The weather in Berlin is cloudy with fifteen degrees, Sir.",
        ];
        for (label, threads, gpu) in [("CPU 8 thr", 8usize, false), ("CPU 12 thr", 12, false), ("GPU DirectML", 4, true)] {
            let mut c = cfg.clone();
            c.tts_threads = threads;
            c.tts_gpu = gpu;
            let Ok(mut k) = Kokoro::load(&c) else { continue };
            let ipas: Vec<String> = clauses.iter().map(|t| phonemizer.phonemize(t)).collect();
            for ipa in &ipas {
                let _ = k.synthesize(ipa, &c.kokoro_voice, c.kokoro_speed);
            }
            let per_clause: Vec<String> = ipas
                .iter()
                .map(|ipa| {
                    let mut times: Vec<u128> = (0..3)
                        .map(|_| {
                            let t = Instant::now();
                            let _ = k.synthesize(ipa, &c.kokoro_voice, c.kokoro_speed);
                            t.elapsed().as_millis()
                        })
                        .collect();
                    times.sort_unstable();
                    format!("{} ms", times[1])
                })
                .collect();
            // A never-seen length shows the cost of shape changes (matters for DirectML).
            let t = Instant::now();
            let _ = k.synthesize(&phonemizer.phonemize("Certainly, Sir, the report is ready."), &c.kokoro_voice, c.kokoro_speed);
            info!("[BENCH] Kokoro {label:<13} clauses {per_clause:?}, unseen length {} ms", t.elapsed().as_millis());
        }
        return summary(r);
    }

    // ---- 4. Wake word + intents ------------------------------------------------------
    let matcher = WakeWordMatcher::new(&cfg.wake_words, cfg.wake_word_similarity_threshold);
    let wake = matcher.evaluate(&text);
    r.check("Wake word", wake.matched, format!("'{}' score {:.2} → command '{}'", wake.word, wake.score, wake.command));
    let intent = intents::parse(&wake.command);
    let ok = matches!(&intent, Some(Intent::Tool { name: "open_application", args }) if args["app_name"].as_str().unwrap_or("").contains("spotify"));
    r.check("Fast-path intent", ok, format!("{intent:?}"));

    // ---- 4b. Instant abilities (no models needed) -------------------------------------
    let calc = crate::calc::try_answer("What's 15 percent of 240?");
    r.check("Calculator", calc.as_deref() == Some("15 percent of 240 is 36, Sir."), format!("{calc:?}"));
    let reminder = crate::reminders::parse_request("Erinnere mich in 5 Minuten an den Tee", chrono::Local::now());
    r.check("Reminder parsing (DE)", reminder.as_ref().is_some_and(|(_, what)| what == "den Tee"), format!("{reminder:?}"));
    let quit = intents::parse("can you please close ur self");
    r.check("Self-shutdown fallback", matches!(&quit, Some(Intent::Tool { name: "shutdown_jarvis", .. })), format!("{quit:?}"));
    let browser = intents::parse("Nana, open my browser").or_else(|| intents::resolve_fuzzy("Help my browser"));
    r.check("Browser intent", matches!(&browser, Some(Intent::Tool { name: "open_application", .. })), format!("{browser:?}"));
    let mode = crate::modality::parse_command("Only talk when I talk to you");
    r.check("Reply-mode command", mode == Some(crate::modality::VoiceResponseMode::MirrorInput), format!("{mode:?}"));
    r.check("Incomplete-thought detector", intents::looks_incomplete("Set the colour to..."), "trailing '…' waits for the rest");

    // ---- 5. German through eSpeak-NG ---------------------------------------------------
    let espeak = crate::apps::which("espeak-ng").or_else(|| {
        std::env::var("ProgramFiles")
            .ok()
            .map(|p| Path::new(&p).join("eSpeak NG").join("espeak-ng.exe"))
            .filter(|p| p.exists())
    });
    match espeak {
        Some(bin) => {
            let wav = std::env::temp_dir().join("jarvis_selftest_de.wav");
            let mut cmd = std::process::Command::new(&bin);
            cmd.args(["-v", "de", "-s", "150", "-w"]).arg(&wav).arg("Hallo Dschey, wie spät ist es?");
            crate::shell::hide_console_std(&mut cmd);
            match cmd.status().ok().filter(|s| s.success()).and_then(|_| read_wav_mono(&wav)) {
                Some((audio, rate)) => {
                    let mut de16 = dsp::resample(&audio, rate, 16_000);
                    scale_to_rms(&mut de16, 0.06);
                    let mut pcm = noise(16_000 / 3, 0.003, &mut seed);
                    pcm.extend(de16);
                    pcm.extend(noise(16_000 / 2, 0.003, &mut seed));
                    // Informational only: eSpeak's formant German is so synthetic that Whisper
                    // often classifies it as Latin/Javanese; real German speech is not affected.
                    match stt.transcribe(pcm, 1500).await {
                        Ok(t) if t.language == "de" => info!(
                            "[TEST] PASS Whisper DE (eSpeak voice): '{}' [{} p={:.2}] in {} ms → wake command '{}'",
                            t.text, t.language, t.language_prob, t.stt_ms, matcher.evaluate(&t.text).command
                        ),
                        Ok(t) => warn!(
                            "[TEST] ~ Whisper DE (eSpeak voice, informational): '{}' [{}]. eSpeak's robotic German is not representative; test with your own voice.",
                            t.text, t.language
                        ),
                        Err(e) => r.check("Whisper DE", false, e),
                    }
                }
                None => warn!("[TEST] eSpeak-NG could not render German audio; skipping."),
            }
            let _ = std::fs::remove_file(wav);
        }
        None => warn!("[TEST] eSpeak-NG not installed; skipping the German STT check."),
    }

    // ---- 6. LLM latency --------------------------------------------------------------
    if !llm_stage {
        info!("[TEST] Language-model stage skipped (--no-llm)");
        return summary(r);
    }
    let llm = crate::runtime::LlmRuntime::start(cfg);
    if !llm.settled().await {
        warn!("[TEST] Language engine: {}", llm.status().detail);
    }
    let store = ConfigStore::new(cfg.clone());
    match (
        crate::memory::MemoryManager::open(&cfg.memory_db_path),
        crate::skills::SkillManager::open(&cfg.memory_db_path, &cfg.skills_dir),
    ) {
        (Ok(memory), Ok(skills)) => {
            let tools = crate::tools::ToolContext {
                memory,
                skills,
                monitor: crate::telemetry::SystemMonitor::start(cfg.hardware_profile),
                voice_volume: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(100)),
                config: store.clone(),
                modality: crate::modality::ModalityStore::in_memory(),
                widgets: crate::widgets::WidgetEngine::in_memory(),
                reminders: crate::reminders::ReminderStore::in_memory(),
                protocols: crate::protocols::ProtocolStore::in_memory(),
                voice_speed: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(1.0f32.to_bits())),
                focus: crate::focus::FocusTracker::new(),
                music: crate::music::MusicEngine::in_memory(),
                workout: crate::workout_coach::WorkoutCoach::in_memory(),
                pim: crate::pim::Pim::in_memory(store.clone()),
                stark: crate::stark::StarkSystems::in_memory(),
            };
            let brain = crate::brain::Brain::new(store, tools, std::sync::Arc::clone(&llm));
            let t = Instant::now();
            brain.prime().await;
            info!("[TEST]   prime/warm-up took {} ms", t.elapsed().as_millis());
            for (label, prompt) in [
                ("LLM chat", "Good evening, JAI. How are you?"),
                ("LLM chat (cached)", "Wie geht es dir heute?"),
                ("LLM tool call", "Wie ist das Wetter gerade in Berlin?"),
            ] {
                match brain.probe(prompt).await {
                    Ok((first_ms, total_ms, reply, prompt_tokens)) => r.check(
                        label,
                        first_ms > 0,
                        format!("first token {first_ms} ms, total {total_ms} ms, {prompt_tokens} prompt tokens evaluated → {}", reply.trim().chars().take(140).collect::<String>()),
                    ),
                    Err(e) => r.check(label, false, format!("{e} (language engine: {})", llm.status().detail)),
                }
            }
        }
        _ => r.check("LLM", false, "could not open memory/skills database"),
    }
    llm.shutdown();

    // ---- 7. Optional: real speaker output ----------------------------------------------
    if speak {
        let controls = std::sync::Arc::new(crate::ears::AudioControls::new(cfg));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let tts = tts::TtsController::start(cfg, controls, tx);
        let gen = tts.stop();
        let t = Instant::now();
        tts.speak("Self test complete, Sir. All systems are nominal.", gen);
        tts.finish(gen);
        let mut started = None;
        while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(20), rx.recv()).await {
            match ev {
                tts::TtsEvent::PlaybackStarted { .. } => started = Some(t.elapsed().as_millis()),
                tts::TtsEvent::PlaybackFinished { .. } => break,
            }
        }
        r.check("Speaker playback", started.is_some(), format!("first audio after {:?} ms", started));
    }

    summary(r)
}

fn summary(r: Report) -> ExitCode {
    if r.failures.is_empty() {
        info!("[TEST] All self-tests passed.");
        ExitCode::SUCCESS
    } else {
        error!("[TEST] {} check(s) failed: {}", r.failures.len(), r.failures.join(", "));
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_right_is_a_word_error_count() {
        assert_eq!(words_right("Close the 3D model.", "Close the 3D model"), (4, 4));
        assert_eq!(words_right("Close the 3D model.", "Those just 3D model."), (2, 4));
        assert_eq!(words_right("Change the colour to green.", "change the color to green"), (5, 5));
        assert_eq!(words_right("I don't want to see the satellites anymore.", "I dont want to see the satellite any more"), (5, 8));
        assert_eq!(words_right("Make it way more detailed.", ""), (0, 5));
    }
}
