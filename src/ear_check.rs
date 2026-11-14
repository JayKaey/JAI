//! His hearing, measured. Two runs that print what they find and exit, without a window, a
//! sound or a language model:
//!
//! * `JAI.exe --ear-check`: the clips of Sir's own orders that are kept on this PC
//!   (`voice_clips`) are heard again as the app hears them today: the first hearing, his ear
//!   (`ear`), and for those that lead nowhere the second, careful hearing. For each stage: how
//!   many clips come out as the order they were taken for (or, where Sir corrected the line,
//!   the order he meant). This is the measure on Sir's real voice; it changes nothing.
//! * `JAI.exe --ear-bench`: the same three stages on stand-in voices that say Sir's everyday
//!   orders, once as written and once with a German speaker's sounds (`german_accent`: "th"
//!   as "s" and "z", "w" as "v", the "J" as "Ch", hard final consonants). No stand-in is Sir,
//!   but it shows what each stage is worth when the sounds are off.
//!
//! Both log their lines as `[EARCHECK]` and write nothing but the log.

use crate::config::JaiConfig;
use crate::stt::SttEngine;
use crate::wake::WakeWordMatcher;
use std::process::ExitCode;
use std::time::{Duration, Instant};
use tracing::{error, info};

/// English phonemes (the notation Kokoro speaks) as a German speaker's mouth shapes them: the
/// two "th" as "s" and "z", "w" as "v", the "J" of "Jay" as "Ch", the "a" of "cat" as "e", the
/// German "r", and every voiced consonant at the end of a word hard.
pub fn german_accent(phonemes: &str) -> String {
    let shifted: Vec<char> = phonemes
        .chars()
        .map(|c| match c {
            'θ' => 's',
            'ð' => 'z',
            'w' => 'v',
            'ʤ' => 'ʧ',
            'æ' => 'ɛ',
            'ɹ' => 'ʁ',
            'ʌ' => 'a',
            other => other,
        })
        .collect();
    let mut out = String::with_capacity(phonemes.len());
    for (i, c) in shifted.iter().enumerate() {
        let ends_word = shifted.get(i + 1).is_none_or(|n| n.is_whitespace() || matches!(n, ',' | '.' | '!' | '?' | ';' | ':'));
        out.push(match *c {
            'b' if ends_word => 'p',
            'd' if ends_word => 't',
            'ɡ' if ends_word => 'k',
            'z' if ends_word => 's',
            'v' if ends_word => 'f',
            other => other,
        });
    }
    out
}

/// What an order does, as far as the routes without a language model tell: two sentences with
/// the same signature are the same order.
fn signature(order: &str) -> Option<String> {
    let t = crate::typos::normalize_user_input_typos(order);
    crate::command_corrector::routed_intent(&t)
        .map(|intent| format!("{intent:?}"))
        .or_else(|| crate::intents::mixer_command(&t).map(|args| format!("mixer {args}")))
        .or_else(|| crate::intents::orb_mode_command(&t).map(|args| format!("orb {args}")))
}

/// Whether what was understood is the order that was said.
fn same_order(understood: &str, said: &str) -> bool {
    let plain = |t: &str| crate::ear::words(&crate::typos::normalize_user_input_typos(t)).join(" ");
    plain(understood) == plain(said) || signature(understood).is_some_and(|s| Some(s) == signature(said))
}

/// One clip through the three stages: what each made of it (his name heard, the order).
struct Stages {
    first: String,
    plain: Option<String>,
    with_ear: Option<String>,
    again: Option<String>,
    careful_ms: u64,
    first_ms: u64,
}

async fn stages(stt: &SttEngine, matcher: &WakeWordMatcher, pcm: &[f32], voiced_ms: u32) -> Result<Stages, String> {
    let heard = stt.transcribe(pcm.to_vec(), voiced_ms).await?;
    let first = crate::wake::canon(&heard.text);
    // As it was before 2026-11-13: his name as written, the words as written.
    let plain = Some(matcher.evaluate(&first)).filter(|m| m.matched).map(|m| m.command);
    // With his ear, and his name as it arrives in Sir's voice (these are Sir's clips).
    let read = |text: &str| -> Option<String> {
        let text = crate::ear::read(text, true, &crate::ear::is_order).map_or_else(|| text.to_string(), |r| r.text);
        let (m, _) = matcher.evaluate_in_his_voice(&text);
        m.matched.then_some(m.command)
    };
    let with_ear = read(&first);
    let mut again = None;
    let mut careful_ms = 0;
    let leads = with_ear.as_deref().is_some_and(crate::ear::is_order);
    // (As the app decides it: only a first hearing the recogniser was unsure of is heard again.)
    let quality = crate::stt::HeardQuality { avg_logprob: heard.avg_logprob, language_prob: heard.language_prob, no_speech: heard.no_speech };
    if !leads && crate::ear::worth_hearing_again(&first, quality, &crate::ear::is_order) {
        let second = stt.transcribe_carefully(pcm.to_vec(), voiced_ms, crate::ear::hint()).await?;
        careful_ms = second.stt_ms;
        again = read(&crate::wake::canon(&second.text)).filter(|order| crate::ear::is_order(order));
    }
    Ok(Stages { first, plain, with_ear, again, careful_ms, first_ms: heard.stt_ms })
}

async fn recogniser(cfg: &JaiConfig) -> Option<SttEngine> {
    let stt = SttEngine::start(cfg);
    let wait = Instant::now();
    while !stt.is_ready() && wait.elapsed() < Duration::from_secs(120) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !stt.is_ready() {
        error!("[EARCHECK] The recogniser did not load");
        return None;
    }
    Some(stt)
}

#[derive(Default)]
struct Tally {
    clips: usize,
    plain: usize,
    with_ear: usize,
    with_second: usize,
    second_hearings: usize,
    first_ms: Vec<u64>,
    careful_ms: Vec<u64>,
}

impl Tally {
    fn take(&mut self, s: &Stages, said: &str) -> (bool, bool, bool) {
        let plain = s.plain.as_deref().is_some_and(|o| same_order(o, said));
        let ear = s.with_ear.as_deref().is_some_and(|o| same_order(o, said));
        let second = ear || s.again.as_deref().is_some_and(|o| same_order(o, said));
        self.clips += 1;
        self.plain += usize::from(plain);
        self.with_ear += usize::from(ear);
        self.with_second += usize::from(second);
        self.first_ms.push(s.first_ms);
        if s.careful_ms > 0 {
            self.second_hearings += 1;
            self.careful_ms.push(s.careful_ms);
        }
        (plain, ear, second)
    }

    fn line(&self, label: &str) -> String {
        let median = |v: &[u64]| {
            let mut s = v.to_vec();
            s.sort_unstable();
            s.get(s.len() / 2).copied().unwrap_or(0)
        };
        format!(
            "{label}: {} clips; the order as it was said: {} as before, {} with his ear, {} with the second hearing too ({} second hearings, median {} ms; first hearing median {} ms)",
            self.clips,
            self.plain,
            self.with_ear,
            self.with_second,
            self.second_hearings,
            median(&self.careful_ms),
            median(&self.first_ms)
        )
    }
}

/// `--ear-check`: Sir's kept clips heard again.
pub async fn check(cfg: &JaiConfig) -> ExitCode {
    crate::voice_clips::set_home(&cfg.memory_db_path);
    crate::ear::set_db(&cfg.memory_db_path);
    let clips = crate::voice_clips::list(crate::voice_clips::KEPT);
    if clips.is_empty() {
        info!("[EARCHECK] No clips of Sir's voice are kept yet: they gather as he gives orders by voice");
        return ExitCode::SUCCESS;
    }
    let Some(stt) = recogniser(cfg).await else { return ExitCode::FAILURE };
    let matcher = WakeWordMatcher::new(&cfg.wake_words, cfg.wake_word_similarity_threshold);
    let (mut led, mut nowhere) = (Tally::default(), Tally::default());
    for clip in clips.iter().rev() {
        let Some(pcm) = crate::voice_clips::audio(clip) else { continue };
        match stages(&stt, &matcher, &pcm, clip.ms).await {
            Ok(s) => {
                let (plain, ear, second) = if clip.led { led.take(&s, &clip.meant) } else { nowhere.take(&s, &clip.meant) };
                if !(plain && ear && second) || !clip.led {
                    info!(
                        "[EARCHECK] {} '{}' (taken for '{}'{}): as before {}, with his ear {}, second hearing {}",
                        clip.at,
                        s.first,
                        clip.meant,
                        if clip.led { "" } else { ", which led nowhere" },
                        s.plain.as_deref().map_or("no name".to_string(), |o| format!("'{o}'")),
                        s.with_ear.as_deref().map_or("no name".to_string(), |o| format!("'{o}'")),
                        s.again.as_deref().map_or("-".to_string(), |o| format!("'{o}'")),
                    );
                }
            }
            Err(e) => info!("[EARCHECK] {} could not be heard again: {e}", clip.at),
        }
    }
    info!("[EARCHECK] {}", led.line("Orders that led somewhere"));
    if nowhere.clips > 0 {
        let now = clips.iter().filter(|c| !c.led).count();
        info!("[EARCHECK] {} ({now} clips had led nowhere when they were said)", nowhere.line("Words that led nowhere"));
    }
    ExitCode::SUCCESS
}

/// What the stand-ins say: Sir's everyday orders, as he gave them on 2026-11-12 and after.
const SAID: &[&str] = &[
    "JAI, open Discord.",
    "JAI, can you open Discord?",
    "JAI, mute everything except Discord.",
    "JAI, zoom out.",
    "JAI, zoom in.",
    "JAI, show me the Earth.",
    "JAI, open the mixer.",
    "JAI, what time is it?",
    "JAI, show me my calendar.",
    "JAI, open your settings.",
    "JAI, hide your core.",
    "JAI, open Spotify.",
    "JAI, next song.",
    "JAI, what's playing?",
    "JAI, learn my voice.",
    "JAI, night mode.",
    "JAI, clear the board.",
    "JAI, system status.",
    "JAI, close the Earth.",
    "JAI, what is the weather tomorrow?",
];

/// `--ear-bench`: the stand-ins, as written and with a German speaker's sounds.
pub async fn bench(cfg: &JaiConfig) -> ExitCode {
    use crate::exhaustive_scenario_verifier::STAND_INS;
    let mut quiet = cfg.clone();
    quiet.tts_gpu = false;
    let phonemizer = crate::phonemizer::Phonemizer::new(quiet.espeak_path.as_deref());
    let mut kokoro = match crate::tts::Kokoro::load(&quiet) {
        Ok(k) => k,
        Err(e) => {
            error!("[EARCHECK] The stand-ins have no voice: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(stt) = recogniser(cfg).await else { return ExitCode::FAILURE };
    let matcher = WakeWordMatcher::new(&cfg.wake_words, cfg.wake_word_similarity_threshold);
    let mut seed = 0x0ea4_2026u32;
    let mut hiss = |samples: usize| -> Vec<f32> {
        (0..samples)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0) * 0.005
            })
            .collect()
    };
    for accent in [false, true] {
        let mut tally = Tally::default();
        for (who, voice, speed) in STAND_INS {
            for say in SAID {
                let ipa = phonemizer.phonemize(&crate::tts::normalize_for_speech(say));
                let ipa = if accent { german_accent(&ipa) } else { ipa };
                let audio24 = match kokoro.synthesize(&ipa, voice, speed) {
                    Ok(a) => a,
                    Err(e) => {
                        info!("[EARCHECK] {who} could not say \"{say}\": {e}");
                        continue;
                    }
                };
                let mut speech = crate::dsp::resample(&audio24, crate::tts::KOKORO_RATE, 16_000);
                let level = crate::dsp::rms(&speech);
                if level > 1e-6 {
                    speech.iter_mut().for_each(|s| *s *= 0.06 / level);
                }
                let under = hiss(speech.len());
                speech.iter_mut().zip(under).for_each(|(s, n)| *s += n);
                let mut clip = hiss(16_000 / 4);
                let voiced_ms = (speech.len() / 16) as u32;
                clip.extend(speech);
                clip.extend(hiss(16_000 / 4));
                let said = say.trim_start_matches("JAI,").trim();
                match stages(&stt, &matcher, &clip, voiced_ms).await {
                    Ok(s) => {
                        let (plain, ear, second) = tally.take(&s, said);
                        if !(plain && ear && second) {
                            info!(
                                "[EARCHECK] {who}{} \"{say}\" was heard as '{}': as before {}, with his ear {}, second hearing {}",
                                if accent { ", German sounds," } else { "" },
                                s.first,
                                if plain { "right".to_string() } else { s.plain.as_deref().map_or("no name".to_string(), |o| format!("'{o}'")) },
                                if ear { "right".to_string() } else { s.with_ear.as_deref().map_or("no name".to_string(), |o| format!("'{o}'")) },
                                if second && !ear { "right".to_string() } else { s.again.as_deref().map_or("-".to_string(), |o| format!("'{o}'")) },
                            );
                        }
                    }
                    Err(e) => info!("[EARCHECK] \"{say}\" could not be heard: {e}"),
                }
            }
        }
        info!("[EARCHECK] {}", tally.line(if accent { "Stand-ins with a German speaker's sounds" } else { "Stand-ins as written" }));
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_german_mouth_shapes_english_sounds() {
        // "The weather", "with Jay", "this card", "what", "have": th as z and s, w as v, the J
        // as Ch, and the last consonant hard.
        assert_eq!(german_accent("ðə wˈɛðə"), "zə vˈɛzə");
        assert_eq!(german_accent("wɪð ʤˈA,"), "vɪs ʧˈA,");
        assert_eq!(german_accent("ðɪs kˈɑːd."), "zɪs kˈɑːt.");
        assert_eq!(german_accent("hæv ɡˈʊd dˈɒɡ"), "hɛf ɡˈʊt dˈɒk");
        // What a German mouth says as an English one stays.
        assert_eq!(german_accent("ˈQpən spˈɒtɪfI"), "ˈQpən spˈɒtɪfI");
    }

    #[test]
    fn two_ways_of_putting_one_order_are_the_same_order() {
        assert!(same_order("Open Spotify.", "open spotify"));
        assert!(same_order("show me the earth", "Show me the Earth."));
        assert!(!same_order("open spotify", "open discord"));
        assert!(!same_order("tell me a story about the sea", "open discord"));
    }
}
