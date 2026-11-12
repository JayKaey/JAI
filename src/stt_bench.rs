//! Whisper speed and accuracy on fixed command clips, measured on this machine.
//!
//! `cargo test --release --bin jai stt::bench -- --ignored --nocapture`
//!
//! The clips are synthesised offline with JAI's own Kokoro path (only voices already cached
//! under `models/voices/`, so nothing is written into the models folder), resampled to 16 kHz,
//! put into room noise (and a noisier copy with mains hum) with the VAD's 500 ms pre-roll and its
//! hangover, then transcribed through `transcribe` exactly as the microphone's clips are. Every
//! variant hears every clip in the same round, interleaved, so a CPU that other programs load
//! slows all of them alike: compare the numbers within one run.
//!
//! `JAI_MODELS` (default: `models` in the project folder), `JAI_ESPEAK` (the eSpeak-NG folder,
//! default `runtime/espeak-ng` there),
//! `JAI_BENCH_ROUNDS` (default 2) and `JAI_BENCH_ONLY` (comma list of variant labels).
//!
//! 2026-11-09, ggml-small (q8_0), 8 threads, other builds running: v22 844 ms median (783 ms for
//! 1.5-3 s clips), 6.1 % English word errors; one pass 559 ms (509 ms), 4.7 %. The voices are
//! English ones, so the German phrases measure nothing but the language check.

use super::*;
use crate::tts::{self, Kokoro};
use std::path::PathBuf;

const PHRASES: &[&str] = &[
    "JAI, show me the world.",
    "JAI, close the Earth.",
    "Close it.",
    "Switch to satellite view.",
    "JAI, open my stats.",
    "Close yourself.",
    "Move it to the middle of my screen.",
    "JAI, what's playing?",
    "Hide your core.",
    "Open Spotify and play some music.",
    "JAI, schließ die Erde.",
    "Wie spät ist es?",
];
/// An American, a British and a Spanish voice (accented English) for Sir's voice.
const VOICES: &[&str] = &["am_adam", "bm_george", "em_alex"];

pub(super) struct Clip {
    pub text: &'static str,
    pub label: String,
    pub pcm: Vec<f32>,
    pub voiced_ms: u32,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| default.to_string())
}

pub(super) fn models_dir() -> PathBuf {
    PathBuf::from(env_or("JAI_MODELS", concat!(env!("CARGO_MANIFEST_DIR"), "/models")))
}

fn espeak_dir() -> String {
    env_or("JAI_ESPEAK", concat!(env!("CARGO_MANIFEST_DIR"), "/runtime/espeak-ng"))
}

fn noise(n: usize, level: f32, hum: f32, seed: &mut u32) -> Vec<f32> {
    (0..n)
        .map(|i| {
            *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let white = ((*seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0) * level;
            white + hum * (std::f32::consts::TAU * 100.0 * i as f32 / SAMPLE_RATE as f32).sin()
        })
        .collect()
}

/// Every phrase in every voice, in room noise; the Spanish voice once more in a noisy room.
pub(super) fn synth_clips() -> Vec<Clip> {
    let models = models_dir();
    let mut cfg = JaiConfig::default();
    cfg.kokoro_model_path = models.join("kokoro-v1.0.onnx").to_string_lossy().into_owned();
    cfg.kokoro_voices_archive_path = models.join("voices-v1.0.bin").to_string_lossy().into_owned();
    cfg.espeak_path = Some(espeak_dir());
    cfg.tts_gpu = false;
    let cache = models.join("voices").join("voices-v1.0");
    let phonemizer = crate::phonemizer::Phonemizer::new(cfg.espeak_path.as_deref());
    let mut kokoro = Kokoro::load(&cfg).expect("Kokoro loads from the models folder");
    let mut seed = 0x0bad_5eedu32;
    let mut clips = Vec::new();
    for voice in VOICES {
        // Only cached voice tables: extracting one would write into the shared models folder.
        assert!(cache.join(format!("{voice}.bin")).exists(), "voice {voice} is not cached in {}", cache.display());
        for (k, text) in PHRASES.iter().enumerate() {
            let ipa = phonemizer.phonemize(&tts::normalize_for_speech(text));
            let audio24 = kokoro.synthesize(&ipa, voice, 1.0).expect("synthesis");
            let mut speech = dsp::resample(&audio24, tts::KOKORO_RATE, SAMPLE_RATE as u32);
            let level = dsp::rms(&speech).max(1e-6);
            speech.iter_mut().for_each(|s| *s *= 0.05 / level);
            let voiced_ms = speech.chunks(SAMPLE_RATE / 50).filter(|w| dsp::rms(w) > 0.012).count() as u32 * 20;
            let tail_ms = if voiced_ms > 1200 { 450 } else { 220 };
            let rooms: &[(&str, f32, f32)] = if *voice == "em_alex" && k % 2 == 0 { &[("quiet", 0.003, 0.0), ("noisy", 0.012, 0.006)] } else { &[("quiet", 0.003, 0.0)] };
            for (room, level, hum) in rooms {
                let mut pcm = noise(SAMPLE_RATE / 2, *level, *hum, &mut seed);
                let bed = noise(speech.len(), *level, *hum, &mut seed);
                pcm.extend(speech.iter().zip(bed).map(|(s, n)| s + n));
                pcm.extend(noise(SAMPLE_RATE * tail_ms / 1000, *level, *hum, &mut seed));
                clips.push(Clip { text, label: format!("{voice}/{room}"), pcm, voiced_ms });
            }
        }
    }
    clips
}

fn words(s: &str) -> Vec<String> {
    normalize(s).split_whitespace().map(str::to_string).collect()
}

/// Word-level edit distance.
pub(super) fn word_errors(heard: &str, said: &str) -> (usize, usize) {
    let (h, r) = (words(heard), words(said));
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    for (i, rw) in r.iter().enumerate() {
        let mut cur = vec![i + 1; h.len() + 1];
        for (j, hw) in h.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(rw != hw)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    (prev[h.len()], r.len())
}

/// The same command for JAI: what the fast path makes of both is the same.
pub(super) fn routes_alike(heard: &str, said: &str) -> bool {
    let route = |t: &str| {
        let t = normalize_stt_transcript(t);
        let wake = crate::wake::WakeWordMatcher::new(&["jai".to_string()], 0.72).evaluate(&t);
        let command = if wake.matched && !wake.command.trim().is_empty() { wake.command } else { t };
        format!("{:?}", crate::intents::parse(&crate::typos::normalize_user_input_typos(&command)))
    };
    normalize(heard) == normalize(said) || route(heard) == route(said) && route(said) != "None"
}

pub(super) struct Variant {
    pub label: &'static str,
    pub flash: bool,
    pub tweak: fn(&mut Settings),
    /// The views that are up while the clips are heard (they go into Whisper's prompt).
    pub scene: &'static [&'static str],
}

#[derive(Default)]
struct Tally {
    ms: Vec<u64>,
    typical_ms: Vec<u64>,
    errors: usize,
    reference: usize,
    exact: usize,
    routed: usize,
    /// English phrases only (the clips' voices are English ones; their German is no measure).
    en_errors: usize,
    en_reference: usize,
    en_routed: usize,
    en_clips: usize,
    second_pass: usize,
    misheard: Vec<String>,
}

fn is_german(text: &str) -> bool {
    text.contains("Erde") || text.starts_with("Wie ")
}

fn median(v: &[u64]) -> u64 {
    let mut s = v.to_vec();
    s.sort_unstable();
    s.get(s.len() / 2).copied().unwrap_or(0)
}

fn pct(v: &[u64], p: usize) -> u64 {
    let mut s = v.to_vec();
    s.sort_unstable();
    s.get((s.len() * p / 100).min(s.len().saturating_sub(1))).copied().unwrap_or(0)
}

fn base_settings(threads: i32) -> Settings {
    Settings {
        threads,
        beam_size: 2,
        prompt: crate::config::WHISPER_DOMAIN_PROMPT.to_string(),
        languages: vec!["de".into(), "en".into()],
        dynamic_ctx: true,
        min_ctx: 128,
        fast: false,
        sot: 0,
        no_speech: 0,
        eot: 0,
    }
}

fn variants() -> Vec<Variant> {
    vec![
        Variant { label: "v22 (beam 2, detect, prompt)", flash: false, tweak: |_| {}, scene: &[] },
        Variant { label: "one pass", flash: false, tweak: |s| s.fast = true, scene: &[] },
        Variant { label: "one pass, Earth up", flash: false, tweak: |s| s.fast = true, scene: &["earth"] },
        Variant { label: "one pass, no beam retry", flash: false, tweak: |s| (s.fast, s.beam_size) = (true, 1), scene: &[] },
        Variant { label: "one pass + flash_attn", flash: true, tweak: |s| s.fast = true, scene: &[] },
        Variant { label: "v22, greedy", flash: false, tweak: |s| s.beam_size = 1, scene: &[] },
        Variant { label: "v22, English only (no detect)", flash: false, tweak: |s| s.languages = vec!["en".into()], scene: &[] },
        Variant { label: "v22, 12 threads", flash: false, tweak: |s| s.threads = 12, scene: &[] },
    ]
}

/// The verifier's spoken commands (its matrix F: `SPOKEN` said by its stand-ins) through the
/// recogniser and the wake word alone, decoded as the app decodes them, without the app: what
/// was heard, whether his name was, and whether the order survived. Minutes instead of a
/// whole verifier run, for work on the vocabulary prompt (`JAI_BENCH_PROMPT`: one or several,
/// separated by `|`) and on the wake word; `JAI_BENCH_ADDRESS` has the stand-ins open with
/// something else than "JAI," ("Hey JAI,"). The verifier's run stays the verdict: only it
/// sends what was heard through the dispatcher.
///
/// `cargo test --release --bin jai spoken_commands -- --ignored --nocapture`
#[test]
#[ignore]
fn spoken_commands_keep_his_name() {
    use crate::exhaustive_scenario_verifier as verifier;
    let models = models_dir();
    let mut cfg = JaiConfig::default();
    cfg.kokoro_model_path = models.join("kokoro-v1.0.onnx").to_string_lossy().into_owned();
    cfg.kokoro_voices_archive_path = models.join("voices-v1.0.bin").to_string_lossy().into_owned();
    cfg.espeak_path = Some(espeak_dir());
    cfg.tts_gpu = false;
    let t = Instant::now();
    let address = env_or("JAI_BENCH_ADDRESS", "");
    let audio = verifier::spoken_audio(&cfg, &address).expect("the stand-ins say the commands");
    println!("{} sentences by {} stand-ins synthesised in {} ms (opening: \"{}\")", verifier::SPOKEN.len(), verifier::STAND_INS.len(), t.elapsed().as_millis(), if address.is_empty() { "JAI," } else { &address });

    // The settings `SttEngine::start` takes from the configuration.
    let mut s = Settings {
        threads: cfg.whisper_thread_count(),
        beam_size: cfg.whisper_beam_size,
        prompt: String::new(),
        languages: cfg.whisper_languages.clone(),
        dynamic_ctx: cfg.whisper_dynamic_audio_ctx,
        min_ctx: cfg.whisper_min_audio_ctx.clamp(64, 1500),
        fast: cfg.whisper_fast_decode,
        sot: 0,
        no_speech: 0,
        eot: 0,
    };
    let model = models.join("ggml-small.bin").to_string_lossy().replace('\\', "/");
    let mut engine = WhisperEngine::load(&[model], cfg.whisper_flash_attn, &mut s).expect("Whisper loads");
    let wake = crate::wake::WakeWordMatcher::new(&cfg.wake_words, cfg.wake_word_similarity_threshold);
    let same_order = |heard: &str, said: &str| {
        let plain = |t: &str| normalize(&crate::typos::normalize_user_input_typos(&crate::typos::imperative_after_name(t)));
        plain(heard) == plain(said) || routes_alike(heard, said)
    };
    for prompt in env_or("JAI_BENCH_PROMPT", crate::config::WHISPER_DOMAIN_PROMPT).split('|').map(str::trim) {
        println!("\nprompt \"{prompt}\"");
        let (mut named, mut kept, mut total, mut ms) = (0, 0, 0, Vec::new());
        for ((who, ..), clips) in verifier::STAND_INS.iter().zip(&audio) {
            for (case, clip) in verifier::SPOKEN.iter().zip(clips) {
                let voiced_ms = (clip.len() * 1000 / SAMPLE_RATE) as u32;
                let heard = transcribe(&mut engine, clip.clone(), voiced_ms, &s, prompt, None).expect("transcribed");
                let m = wake.evaluate(&heard.text);
                let said = case.say.trim_start_matches("JAI,").trim();
                let same = m.matched && same_order(&m.command, said);
                total += 1;
                named += usize::from(m.matched);
                kept += usize::from(same);
                ms.push(heard.stt_ms);
                if !same {
                    let what = if m.matched { "other words" } else { "NAME NOT HEARD" };
                    println!("  {what}: {who} '{}' → '{}' [lp {:.2}, {} pass]", case.say, heard.text, heard.avg_logprob, heard.passes);
                }
            }
        }
        println!("  name heard {named}/{total}, same order {kept}/{total}, median {} ms, p90 {} ms", median(&ms), pct(&ms, 90));
    }
}

#[test]
#[ignore]
fn bench_command_clips() {
    let model = models_dir().join("ggml-small.bin").to_string_lossy().replace('\\', "/");
    let rounds: usize = env_or("JAI_BENCH_ROUNDS", "2").parse().unwrap_or(2);
    let only: Vec<String> = env_or("JAI_BENCH_ONLY", "").split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
    let t = Instant::now();
    let clips = synth_clips();
    println!("{} clips synthesised in {} ms", clips.len(), t.elapsed().as_millis());

    let variants: Vec<Variant> = variants().into_iter().filter(|v| only.is_empty() || only.iter().any(|o| v.label.to_lowercase().contains(o))).collect();
    let threads = JaiConfig::default().whisper_thread_count();
    // One engine (model + states) per flash setting; each variant has its own settings.
    let mut engines: Vec<(bool, WhisperEngine)> = Vec::new();
    let mut settings: Vec<Settings> = Vec::new();
    let mut tokens = None;
    for v in &variants {
        let mut s = base_settings(threads);
        if !engines.iter().any(|(f, _)| *f == v.flash) {
            let e = WhisperEngine::load(&[model.clone()], v.flash, &mut s).expect("Whisper loads");
            engines.push((v.flash, e));
            tokens = Some((s.sot, s.no_speech, s.eot));
        }
        // The token ids come from the model: the first load filled them in.
        (s.sot, s.no_speech, s.eot) = tokens.expect("a model was loaded");
        (v.tweak)(&mut s);
        settings.push(s);
    }

    let prompts: Vec<String> = variants
        .iter()
        .zip(&settings)
        .map(|(v, s)| crate::command_corrector::whisper_prompt(&s.prompt, &crate::command_corrector::Scene { open: v.scene.to_vec() }))
        .collect();
    let mut tallies: Vec<Tally> = variants.iter().map(|_| Tally::default()).collect();
    for round in 0..rounds {
        for clip in &clips {
            for (i, v) in variants.iter().enumerate() {
                let engine = &mut engines.iter_mut().find(|(f, _)| *f == v.flash).expect("engine").1;
                let heard = transcribe(engine, clip.pcm.clone(), clip.voiced_ms, &settings[i], &prompts[i], None).expect("transcribed");
                let tally = &mut tallies[i];
                tally.ms.push(heard.stt_ms);
                if (1500..=3000).contains(&heard.audio_ms) {
                    tally.typical_ms.push(heard.stt_ms);
                }
                if round == 0 {
                    let (errors, n) = word_errors(&heard.text, clip.text);
                    tally.errors += errors;
                    tally.reference += n;
                    tally.exact += usize::from(errors == 0);
                    let alike = routes_alike(&heard.text, clip.text);
                    tally.routed += usize::from(alike);
                    tally.second_pass += usize::from(heard.passes > 1);
                    if !is_german(clip.text) {
                        tally.en_errors += errors;
                        tally.en_reference += n;
                        tally.en_routed += usize::from(alike);
                        tally.en_clips += 1;
                    }
                    if errors > 0 {
                        tally.misheard.push(format!("{} '{}' → '{}' [{} {:.2}, lp {:.2}, {} pass]{}", clip.label, clip.text, heard.text, heard.language, heard.language_prob, heard.avg_logprob, heard.passes, if alike { " (routes alike)" } else { "" }));
                    }
                }
            }
        }
    }
    println!(
        "\n{:<32} {:>7} {:>9} {:>7} {:>6} {:>7} {:>8} {:>7} {:>9} {:>7}",
        "variant", "median", "1.5-3 s", "p90", "WER", "exact", "routes", "EN WER", "EN routes", "2 pass"
    );
    for (v, t) in variants.iter().zip(&tallies) {
        println!(
            "{:<32} {:>4} ms {:>6} ms {:>4} ms {:>5.1}% {:>3}/{:<3} {:>3}/{:<3} {:>6.1}% {:>4}/{:<4} {:>3}/{:<3}",
            v.label,
            median(&t.ms),
            median(&t.typical_ms),
            pct(&t.ms, 90),
            100.0 * t.errors as f32 / t.reference.max(1) as f32,
            t.exact,
            clips.len(),
            t.routed,
            clips.len(),
            100.0 * t.en_errors as f32 / t.en_reference.max(1) as f32,
            t.en_routed,
            t.en_clips,
            t.second_pass,
            clips.len()
        );
    }
    for (v, t) in variants.iter().zip(&tallies) {
        println!("\n-- {} misheard:", v.label);
        for m in &t.misheard {
            println!("   {m}");
        }
    }
}

/// The voice lock on the verifier's two stand-in voices (the real speaker model, on the clips
/// the recogniser's bench uses): a print made of six sentences of one voice passes that voice's
/// other sentences and turns every sentence of the other voice away. Prints the likenesses.
///
/// `cargo test --release --bin jai stand_in_voices -- --ignored --nocapture`
#[test]
#[ignore]
fn stand_in_voices_are_told_apart() {
    use crate::exhaustive_scenario_verifier as verifier;
    let models = models_dir();
    let mut cfg = JaiConfig::default();
    cfg.kokoro_model_path = models.join("kokoro-v1.0.onnx").to_string_lossy().into_owned();
    cfg.kokoro_voices_archive_path = models.join("voices-v1.0.bin").to_string_lossy().into_owned();
    cfg.espeak_path = Some(espeak_dir());
    cfg.tts_gpu = false;
    crate::speaker_id::set_models_dir(&models);
    assert!(crate::speaker_id::available(), "models/speaker holds {}", crate::speaker_id::MODEL);
    let audio = verifier::spoken_audio(&cfg, "").expect("the stand-ins say the commands");
    let t = Instant::now();
    let prints: Vec<Vec<Vec<f32>>> = audio.iter().map(|voice| voice.iter().map(|clip| crate::speaker_id::print_of(clip).expect("a print of the clip")).collect()).collect();
    let clips: usize = prints.iter().map(Vec::len).sum();
    println!("{clips} prints in {} ms ({} ms each, {} numbers)", t.elapsed().as_millis(), t.elapsed().as_millis() as usize / clips.max(1), prints[0][0].len());
    for (who, other) in [(0usize, 1usize), (1, 0)] {
        let print = crate::speaker_id::enrol(&prints[who][..6]).expect("six clips make a print");
        let his: Vec<f32> = prints[who][6..].iter().map(|c| print.is_his(c, 2500).1).collect();
        let strangers: Vec<f32> = prints[other].iter().map(|c| print.is_his(c, 2500).1).collect();
        let (low, high) = (his.iter().copied().fold(f32::INFINITY, f32::min), strangers.iter().copied().fold(f32::NEG_INFINITY, f32::max));
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
        println!(
            "{}: his from {:.2}; his own other sentences {:.2} … (mean {:.2}); the other voice … {:.2} (mean {:.2})",
            verifier::STAND_INS[who].0,
            print.threshold,
            low,
            mean(&his),
            high,
            mean(&strangers)
        );
        let passed = his.iter().filter(|s| **s >= print.threshold).count();
        let turned = strangers.iter().filter(|s| **s < print.threshold).count();
        assert!(passed * 10 >= his.len() * 9, "{} of {} of his own sentences pass", passed, his.len());
        assert_eq!(turned, strangers.len(), "every sentence of the other voice is turned away");
    }
}
