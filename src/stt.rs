//! Whisper (whisper.cpp) speech-to-text on a dedicated, priority-raised worker thread.
//!
//! Accuracy measures (quiet / mumbled speech):
//! * 80 Hz high-pass (24 dB/oct) against fan and desk rumble, then one linear, peak-safe gain to
//!   a 0.85 peak (`dsp::normalize_for_asr`). No pre-emphasis: Whisper's log-mel front end wants
//!   flat, natural speech (a 0.95 pre-emphasis turned "core" into "car").
//!
//! Latency measures (a short command transcribes in ~100-150 ms instead of ~1.4 s):
//! * one persistent `WhisperState` (no ~400 MB allocation per utterance),
//! * greedy decoding (best_of 1), temperature 0 **and** no temperature fallback,
//! * `no_context`, `single_segment`, `no_timestamps`,
//! * the encoder window (`audio_ctx`) scaled to the clip — `(secs / 30 × 1500) + 64` frames,
//!   at least 128, and 96 for a one-word reflex ("mute", "stop") — instead of a padded 30 s
//!   (whisper.cpp's default 1500),
//! * 4-8 threads, language auto-detection constrained to German/English (configurable).
//!
//! Phantom words (Whisper inventing "Thank you", "Untertitel …", prompt echoes):
//! * no initial prompt (a vocabulary prompt was the main source of invented words on quiet
//!   audio) and no conditioning on previous text,
//! * a pre-STT energy gate: breath, mic hiss and clicks (< 180 ms of voiced energy) never reach
//!   Whisper,
//! * after decoding, OpenAI's no-speech test: the `<|nospeech|>` probability at the
//!   start-of-transcript position (one extra decoder step) above 0.60 with an unconfident decode
//!   discards the result, then the phantom-phrase and stutter filters run.
//!
//! whisper.cpp silently returns nothing for input shorter than 1 s, so short commands
//! ("Stopp!") are padded to 1.3 s.

use crate::config::JarvisConfig;
use crate::dsp::{self, Biquad};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tracing::{error, info, warn};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState, WhisperToken};

const SAMPLE_RATE: usize = 16_000;
/// Longer than this (1.6 s) a clip is a sentence, not a one-word command.
const SENTENCE_SAMPLES: usize = SAMPLE_RATE * 8 / 5;
/// Spoken audio longer than this (1.2 s, before padding) gets the beam search; "stop" / "mute"
/// reflexes (under 1 s) stay greedy and fast.
const BEAM_SAMPLES: usize = SAMPLE_RATE * 6 / 5;
/// The beam search's patience (whisper.cpp's -1 means its default 1.0; stated explicitly).
const BEAM_PATIENCE: f32 = 1.0;
/// German only with this much certainty when English is allowed too: Sir's accented English
/// scores up to 0.87 as German, his real German 0.92 and more (jarvis.log, 2026-10-23/24).
const GERMAN_CERTAINTY: f32 = 0.9;
const MIN_SAMPLES: usize = SAMPLE_RATE * 13 / 10;
/// Less voiced audio than this is breath, hiss or a click, never a word.
const MIN_VOICED_MS: u32 = 180;
/// Whisper's no-speech probability above which an unconfident transcript is discarded.
const NO_SPEECH_THOLD: f32 = 0.60;
/// A decode this confident (mean token log-probability) is kept even with a high no-speech
/// probability: short real commands padded with silence ("Stop.") can score high on both.
const CONFIDENT_LOGPROB: f32 = -0.40;

#[derive(Debug, Clone)]
pub struct Transcript {
    pub text: String,
    pub language: String,
    pub language_prob: f32,
    pub stt_ms: u64,
    pub audio_ms: u64,
    /// Whisper's own confidence: the mean log-probability of the words (0 = certain) …
    pub avg_logprob: f32,
    /// … and the probability that the clip holds no speech at all.
    pub no_speech: f32,
}

struct Job {
    pcm: Vec<f32>,
    voiced_ms: u32,
    reply: oneshot::Sender<Result<Transcript, String>>,
    /// Set when this clip is no longer wanted (a head start Sir talked on after): the decode
    /// stops at its next step.
    called_off: Option<Arc<AtomicBool>>,
}

/// What a transcription that was called off returns.
pub const CALLED_OFF: &str = "called off: Sir carried on";

/// whisper.cpp asks this between its computation steps; `data` points at the job's flag, which
/// the job keeps alive for the whole decode. (whisper-rs 0.12's own closure wrapper hands the
/// callback a pointer of the wrong type, so the flag is passed directly.)
unsafe extern "C" fn called_off_callback(data: *mut std::ffi::c_void) -> bool {
    // SAFETY: `data` is `Arc::as_ptr` of an `Arc<AtomicBool>` that outlives `WhisperState::full`.
    !data.is_null() && unsafe { &*(data as *const AtomicBool) }.load(Ordering::Relaxed)
}

#[derive(Clone)]
pub struct SttEngine {
    tx: mpsc::Sender<Job>,
    ready: Arc<AtomicBool>,
    model: Arc<std::sync::Mutex<String>>,
}

struct Settings {
    threads: i32,
    beam_size: i32,
    prompt: String,
    languages: Vec<String>,
    dynamic_ctx: bool,
    min_ctx: i32,
    /// `<|startoftranscript|>`, `<|nospeech|>` and `<|endoftext|>` of the loaded model.
    sot: WhisperToken,
    no_speech: WhisperToken,
    eot: WhisperToken,
}

/// A transcript with Whisper's own confidence.
struct Decoded {
    text: String,
    /// Mean log-probability of the text tokens (0 = certain).
    avg_logprob: f32,
}

/// Encoder window of the language-detection state: 160 frames = the first 3.2 s, plenty to
/// tell German from English at a fraction of the cost of a full encode.
const DETECT_CTX: i32 = 160;
/// Encoder window of a one-word reflex ("mute", "stop", "weiter"): 96 frames = 1.92 s, which
/// covers the clip padded to 1.3 s and decodes in roughly half the time of the 128-frame floor.
const REFLEX_CTX: i32 = 96;
/// Up to this much voiced audio a clip is a reflex word.
const REFLEX_VOICED_MS: u32 = 850;

/// The reflex window for a clip of `samples` with `voiced_ms` of speech, if it is one word.
fn reflex_ctx(samples: usize, voiced_ms: u32) -> Option<i32> {
    (samples <= MIN_SAMPLES && voiced_ms <= REFLEX_VOICED_MS).then_some(REFLEX_CTX)
}

/// English-only Whisper models (`ggml-base.en.bin`, `ggml-small.en-q8_0.bin`).
fn is_english_only(path: &str) -> bool {
    let name = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path).to_lowercase();
    name.contains(".en.") || name.contains(".en-") || name.ends_with(".en")
}

/// An English-only model cannot recognise German at all, so the multilingual model next to it
/// is used instead when there is one ("ggml-base.en.bin" → "ggml-base.bin", else small/base).
/// The models to try, in order: the configured one and its fallback, then the stock small,
/// base and quantised builds (missing files are skipped by `load_model`).
fn model_candidates(primary: &str, fallback: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in [primary, fallback, "models/ggml-small.bin", "models/ggml-base.bin", "models/ggml-small-q8_0.bin", "models/ggml-base-q8_0.bin"] {
        let p = p.replace('\\', "/");
        if !p.is_empty() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

fn prefer_multilingual(path: &str) -> String {
    if path.is_empty() || !is_english_only(path) {
        return path.to_string();
    }
    let p = Path::new(path);
    let dir = p.parent().unwrap_or_else(|| Path::new("."));
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let same = name.replace(".en.", ".").replace(".en-", "-");
    for candidate in [same.as_str(), "ggml-small.bin", "ggml-small-q8_0.bin", "ggml-base.bin", "ggml-base-q8_0.bin"] {
        let alt = dir.join(candidate);
        if alt.exists() {
            warn!("[STT] {name} is an English-only Whisper model and cannot hear German; using {} instead.", alt.display());
            return alt.to_string_lossy().into_owned();
        }
    }
    warn!("[STT] {name} is an English-only Whisper model: German will not be recognised. Put ggml-small.bin or ggml-base.bin into {} for German.", dir.display());
    path.to_string()
}

impl SttEngine {
    /// Spawns the worker; the model loads in the background (`is_ready` turns true).
    pub fn start(cfg: &JarvisConfig) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let ready = Arc::new(AtomicBool::new(false));
        let model = Arc::new(std::sync::Mutex::new(String::new()));
        let mut settings = Settings {
            threads: cfg.whisper_thread_count(),
            beam_size: cfg.whisper_beam_size,
            prompt: cfg.whisper_initial_prompt.clone(),
            languages: cfg.whisper_languages.clone(),
            dynamic_ctx: cfg.whisper_dynamic_audio_ctx,
            min_ctx: cfg.whisper_min_audio_ctx.clamp(64, 1500),
            sot: 0,
            no_speech: 0,
            eot: 0,
        };
        // Multilingual models only (German and English); an `.en` model is swapped for its sibling.
        let paths = model_candidates(&prefer_multilingual(&cfg.whisper_model_path), &prefer_multilingual(&cfg.whisper_fallback_model_path));
        let flash_attn = cfg.whisper_flash_attn;
        let (ready_w, model_w) = (Arc::clone(&ready), Arc::clone(&model));

        let spawned = std::thread::Builder::new().name("jarvis-stt".into()).spawn(move || {
            crate::platform::raise_thread_priority(false);
            let Some(first) = WhisperEngine::load(&paths, flash_attn, &mut settings) else {
                error!("[STT] No Whisper model could be loaded; voice input is disabled.");
                crate::vram_guard::report_stt(false);
                for job in rx {
                    let _ = job.reply.send(Err("speech recognition model is not loaded".into()));
                }
                return;
            };
            *model_w.lock().unwrap_or_else(|e| e.into_inner()) = first.path.clone();
            ready_w.store(true, Ordering::Release);

            // Zero-VRAM standby: the guard sets this to `None` (Drop frees every whisper.cpp
            // buffer) and the next clip or wake-up re-creates it.
            let mut engine = Some(first);
            let mut last_job = Instant::now();
            let mut epoch = crate::vram_guard::flush_epoch();
            let mut retry_at: Option<Instant> = None;
            loop {
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(job) => {
                        if engine.is_none() {
                            engine = WhisperEngine::load(&paths, flash_attn, &mut settings);
                            crate::vram_guard::report_stt(engine.is_some());
                        }
                        let result = match engine.as_mut() {
                            // A head start that was called off while it waited is not decoded at all.
                            _ if job.called_off.as_ref().is_some_and(|flag| flag.load(Ordering::Relaxed)) => Err(CALLED_OFF.into()),
                            // A panic inside whisper.cpp's bindings fails this clip only; the
                            // model is re-created for the next one (its state may be torn).
                            Some(e) => crate::crash_guard::guard("Whisper transcription", || transcribe(&mut e.state, e.detect_state.as_mut(), job.pcm, job.voiced_ms, &settings, job.called_off.as_ref()))
                                .unwrap_or_else(|| {
                                    engine = None;
                                    Err("speech recognition failed on this clip".into())
                                }),
                            None => Err("speech recognition model could not be re-loaded".into()),
                        };
                        let _ = job.reply.send(result);
                        last_job = Instant::now();
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let action = crate::vram_guard::worker_action(engine.is_some(), &mut epoch, last_job, retry_at);
                        let changed = crate::vram_guard::apply_worker_action(&mut engine, action, || WhisperEngine::load(&paths, flash_attn, &mut settings));
                        match action {
                            crate::vram_guard::WorkerAction::Unload if changed => info!("[STT] Whisper unloaded (zero-VRAM standby)."),
                            crate::vram_guard::WorkerAction::Reload => {
                                retry_at = engine.is_none().then(|| Instant::now() + crate::vram_guard::RELOAD_BACKOFF);
                            }
                            _ => {}
                        }
                        if changed || action == crate::vram_guard::WorkerAction::Reload {
                            crate::vram_guard::report_stt(engine.is_some());
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        if let Err(e) = spawned {
            error!("[STT] Could not start worker thread: {e}");
        }
        Self { tx, ready, model }
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn model_name(&self) -> String {
        self.model.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// `voiced_ms` is the VAD's measure of real speech in the clip; short clips get the strict
    /// hallucination filter ("Thank you." from a cough), long ones keep polite phrases.
    pub async fn transcribe(&self, pcm: Vec<f32>, voiced_ms: u32) -> Result<Transcript, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Job { pcm, voiced_ms, reply, called_off: None })
            .map_err(|_| "speech recognition worker is not running".to_string())?;
        rx.await.map_err(|_| "speech recognition worker stopped".to_string())?
    }

    /// A head start: the words so far, transcribed while it is not yet certain that Sir has
    /// finished. Returns the flag that calls the decode off (set it when he carries on) and the
    /// transcript to come. None when the worker is gone.
    pub fn transcribe_ahead(&self, pcm: Vec<f32>, voiced_ms: u32) -> Option<(Arc<AtomicBool>, oneshot::Receiver<Result<Transcript, String>>)> {
        let (reply, rx) = oneshot::channel();
        let called_off = Arc::new(AtomicBool::new(false));
        self.tx.send(Job { pcm, voiced_ms, reply, called_off: Some(Arc::clone(&called_off)) }).ok()?;
        Some((called_off, rx))
    }
}

/// A loaded Whisper model with its decoding states. Dropping it frees the model and all compute
/// buffers.
struct WhisperEngine {
    state: WhisperState,
    detect_state: Option<WhisperState>,
    _ctx: WhisperContext,
    path: String,
}

impl WhisperEngine {
    /// Loads the first model that works and warms it up (also used to re-load after standby).
    fn load(paths: &[String], flash_attn: bool, settings: &mut Settings) -> Option<Self> {
        let t0 = Instant::now();
        let (ctx, path) = load_model(paths, flash_attn)?;
        let mut state = match ctx.create_state() {
            Ok(s) => s,
            Err(e) => {
                error!("[STT] Could not create Whisper state: {e}");
                return None;
            }
        };
        if is_english_only(&path) {
            // No multilingual model on disk: language detection would fail on every clip.
            settings.languages = vec!["en".to_string()];
        }
        settings.sot = ctx.token_sot();
        settings.no_speech = ctx.token_nosp();
        settings.eot = ctx.token_eot();

        // Warm-up: allocates compute buffers and seeds the encoder windows. whisper.cpp keeps
        // the last `audio_ctx` per state and language detection reuses it, so a second state
        // pinned to a 3.2 s window makes detection ~3× cheaper than on the main state.
        let warm: Vec<f32> = (0..MIN_SAMPLES).map(|i| ((i as f32) * 0.37).sin() * 0.0005).collect();
        let _ = run_full(&mut state, &warm, warm.len(), settings, Some("en"), None, None);
        let detect_state = if settings.languages.len() > 1 {
            match ctx.create_state() {
                Ok(mut s) => {
                    let _ = run_full(&mut s, &warm, warm.len(), settings, Some("en"), Some(DETECT_CTX), None);
                    Some(s)
                }
                Err(e) => {
                    warn!("[STT] No separate language-detection state ({e}); using the main state.");
                    None
                }
            }
        } else {
            None
        };
        info!(
            "[STT] Whisper ready: {path} ({} threads, {}, load + warm-up {} ms)",
            settings.threads,
            if settings.beam_size > 1 { format!("beam search {}", settings.beam_size) } else { "greedy".to_string() },
            t0.elapsed().as_millis()
        );
        Some(Self { state, detect_state, _ctx: ctx, path })
    }
}

fn load_model(paths: &[String], flash_attn: bool) -> Option<(WhisperContext, String)> {
    for path in paths.iter().filter(|p| !p.is_empty()) {
        if !Path::new(path).exists() {
            warn!("[STT] Whisper model not found at {path}");
            continue;
        }
        let mut params = WhisperContextParameters::default();
        params.flash_attn(flash_attn);
        match WhisperContext::new_with_params(path, params) {
            Ok(ctx) => return Some((ctx, path.clone())),
            Err(e) => warn!("[STT] Failed to load {path}: {e}"),
        }
    }
    None
}

/// Rumble filter + peak-safe gain; `None` for effectively silent buffers. No pre-emphasis or
/// other spectral shaping: Whisper's log-mel front end expects natural, flat speech, and a
/// `y[n] = x[n] - 0.95 x[n-1]` filter wiped out the 200-600 Hz vowel formants that tell "core"
/// from "car" and "call".
fn preprocess(pcm: &[f32]) -> Option<Vec<f32>> {
    // Two cascaded 2nd-order sections: an 80 Hz, 24 dB/octave high-pass.
    let mut hp1 = Biquad::high_pass(80.0, SAMPLE_RATE as f32);
    let mut hp2 = Biquad::high_pass(80.0, SAMPLE_RATE as f32);
    let mut audio: Vec<f32> = pcm.iter().map(|&s| hp2.process(hp1.process(s))).collect();
    let peak = dsp::peak(&audio);
    let rms = dsp::rms(&audio);
    if peak < 0.003 || rms < 0.0006 {
        return None;
    }
    let gain = dsp::normalize_for_asr(&mut audio);
    tracing::debug!("[STT] Input peak {:.1} dBFS, RMS {:.1} dBFS, gain {:+.1} dB", dsp::to_dbfs(peak), dsp::to_dbfs(rms), dsp::to_dbfs(gain));
    if audio.len() < MIN_SAMPLES {
        audio.resize(MIN_SAMPLES, 0.0);
    }
    Some(audio)
}

/// The encoder window for `samples` of audio: 1500 encoder frames cover 30 s, so the window is
/// the clip's share of them plus 64 frames (1.28 s) of margin, at least 128 (and `min_ctx`):
/// a 0.6 s "stop" (padded to 1.3 s) encodes 129 frames instead of 1500.
fn audio_ctx_for(samples: usize, min_ctx: i32) -> i32 {
    // ceil(secs / 30 × 1500) in exact integers (f32 turns 2 s into 100.00001 → 101 frames).
    let frames = (samples * 1500).div_ceil(SAMPLE_RATE * 30) as i32;
    let dynamic_ctx = (frames + 64).clamp(128, 1500);
    dynamic_ctx.max(min_ctx.min(1500))
}

/// The decoder's token budget for `samples` of audio: 10 tokens a second (fast German speech
/// needs about 6) plus 16, between 24 and whisper.cpp's own 220. Without timestamps only
/// end-of-text stops the decoder, so a greedy repetition loop ("Pause. Pause. Pause. …") would
/// otherwise run to 220 tokens: seconds of decoding for a one-word command.
fn max_tokens_for(samples: usize) -> i32 {
    let per_second = (samples * 10).div_ceil(SAMPLE_RATE) as i32;
    (per_second + 16).clamp(24, 220)
}

/// Breath, mic hiss and mouse clicks are dropped before Whisper: less than 180 ms of voiced audio
/// by the VAD's count, too quiet, or (in a quiet room) less than 100 ms of 20 ms windows clearly
/// above the clip's own room level. Returns why, or None for real speech.
fn energy_gate(pcm: &[f32], voiced_ms: u32) -> Option<String> {
    if voiced_ms < MIN_VOICED_MS {
        return Some(format!("{voiced_ms} ms voiced"));
    }
    let peak = dsp::peak(pcm);
    if peak < 0.003 {
        return Some(format!("peak {:.1} dBFS", dsp::to_dbfs(peak)));
    }
    let windows: Vec<f32> = pcm.chunks(SAMPLE_RATE / 50).map(dsp::rms).collect();
    if windows.len() < 10 || voiced_ms >= 1000 {
        return None;
    }
    let mut sorted = windows.clone();
    sorted.sort_by(f32::total_cmp);
    // The quietest fifth is the room (the VAD's pre-roll and tail are part of every clip).
    let floor = sorted[sorted.len() / 5].max(0.0004);
    // Over loud music or a game the room level says nothing about the words: Silero and
    // Whisper's own no-speech test decide there ("stop" said over a song must get through).
    if floor > 0.008 {
        return None;
    }
    let gate = (floor * 2.5).max(0.0015);
    let loud_ms = windows.iter().filter(|w| **w > gate).count() as u32 * 20;
    (loud_ms < 100).then(|| format!("only {loud_ms} ms above the room level"))
}

/// Beam search for `spoken` samples of real audio (before the 1.3 s padding)?
fn use_beam(spoken: usize, beam_size: i32) -> bool {
    beam_size > 1 && spoken > BEAM_SAMPLES
}

fn run_full(
    state: &mut WhisperState,
    audio: &[f32],
    spoken: usize,
    s: &Settings,
    language: Option<&str>,
    audio_ctx: Option<i32>,
    called_off: Option<&Arc<AtomicBool>>,
) -> Result<Decoded, String> {
    // Short commands ("stop", "pause") decode greedily for speed; a sentence over 1.2 s gets the
    // beam search, which hears names, accents and "core" far better. (`audio` is padded to
    // ≥ 1.3 s, so the decision uses the spoken length.)
    let strategy = if use_beam(spoken, s.beam_size) {
        SamplingStrategy::BeamSearch { beam_size: s.beam_size, patience: BEAM_PATIENCE }
    } else {
        SamplingStrategy::Greedy { best_of: 2 }
    };
    let mut params = FullParams::new(strategy);
    params.set_n_threads(s.threads);
    params.set_language(language);
    params.set_translate(false);
    params.set_no_context(true);
    params.set_single_segment(true);
    params.set_no_timestamps(true);
    params.set_temperature(0.0);
    // A looping or unconfident sentence ("Change the colour to red. Change the colour to red. …")
    // is decoded again at a higher temperature instead of being returned. One-word commands are
    // not (each retry costs ~0.4 s, and their loops are cut by the token cap).
    params.set_temperature_inc(if audio.len() > SENTENCE_SAMPLES { 0.2 } else { 0.0 });
    params.set_suppress_blank(true);
    params.set_suppress_non_speech_tokens(true);
    params.set_no_speech_thold(0.65);
    params.set_logprob_thold(-0.85);
    params.set_entropy_thold(2.2);
    params.set_max_tokens(max_tokens_for(audio.len()));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    if !s.prompt.is_empty() {
        params.set_initial_prompt(&s.prompt);
    }
    match audio_ctx {
        Some(ctx) => params.set_audio_ctx(ctx),
        None if s.dynamic_ctx => params.set_audio_ctx(audio_ctx_for(audio.len(), s.min_ctx)),
        None => {}
    }
    if let Some(flag) = called_off {
        // SAFETY: the callback only reads the flag, and the caller's `Arc` keeps it alive until
        // `full` below has returned (the parameters do not outlive that call).
        unsafe {
            params.set_abort_callback(Some(called_off_callback));
            params.set_abort_callback_user_data(Arc::as_ptr(flag) as *mut std::ffi::c_void);
        }
    }
    let decoded = state.full(params, audio);
    if called_off.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return Err(CALLED_OFF.into());
    }
    decoded.map_err(|e| format!("whisper failed: {e}"))?;
    let n = state.full_n_segments().map_err(|e| e.to_string())?;
    let mut text = String::new();
    let (mut logprob, mut tokens) = (0.0f32, 0usize);
    for i in 0..n {
        if let Ok(seg) = state.full_get_segment_text_lossy(i) {
            text.push_str(&seg);
            text.push(' ');
        }
        for t in 0..state.full_n_tokens(i).unwrap_or(0) {
            if let Ok(data) = state.full_get_token_data(i, t) {
                // Text tokens only (timestamps and control tokens sit above end-of-text).
                if data.id < s.eot {
                    logprob += data.plog;
                    tokens += 1;
                }
            }
        }
    }
    let avg_logprob = if tokens > 0 { logprob / tokens as f32 } else { 0.0 };
    Ok(Decoded { text: text.trim().to_string(), avg_logprob })
}

/// OpenAI Whisper's no-speech probability: the softmax weight of `<|nospeech|>` predicted at the
/// start-of-transcript position, from one decoder step over the encoder output `full` left in
/// the state (whisper.cpp itself does not report it).
fn no_speech_probability(state: &mut WhisperState, s: &Settings) -> Option<f32> {
    state.decode(&[s.sot], 0, s.threads.max(1) as usize).ok()?;
    let logits = state.get_logits().ok()?;
    let target = *logits.get(usize::try_from(s.no_speech).ok()?)?;
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let sum: f32 = logits.iter().map(|l| (l - max).exp()).sum();
    (sum > 0.0).then(|| ((target - max).exp() / sum).clamp(0.0, 1.0))
}

/// Picks the most probable language among the allowed codes.
fn detect_language(state: &mut WhisperState, audio: &[f32], s: &Settings) -> Option<(&'static str, f32)> {
    let threads = s.threads.max(1) as usize;
    state.pcm_to_mel(audio, threads).ok()?;
    let (_, probs) = state.lang_detect(0, threads).ok()?;
    if tracing::enabled!(tracing::Level::DEBUG) {
        let mut ranked: Vec<(usize, f32)> = probs.iter().copied().enumerate().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        let top: Vec<String> = ranked
            .iter()
            .take(4)
            .filter_map(|(id, p)| whisper_rs::get_lang_str(*id as i32).map(|l| format!("{l}={p:.2}")))
            .collect();
        tracing::debug!("[STT] Language probabilities: {}", top.join(", "));
    }
    let allowed: Vec<(&'static str, f32)> = s
        .languages
        .iter()
        .filter_map(|code| {
            let id = whisper_rs::get_lang_id(code)?;
            let name = whisper_rs::get_lang_str(id)?;
            Some((name, probs.get(id as usize).copied().unwrap_or(0.0)))
        })
        .collect();
    pick_language(&allowed)
}

fn transcribe(
    state: &mut WhisperState,
    detect_state: Option<&mut WhisperState>,
    pcm: Vec<f32>,
    voiced_ms: u32,
    s: &Settings,
    called_off: Option<&Arc<AtomicBool>>,
) -> Result<Transcript, String> {
    let started = Instant::now();
    let audio_ms = (pcm.len() * 1000 / SAMPLE_RATE) as u64;
    let nothing = |started: Instant| Transcript { text: String::new(), language: "-".into(), language_prob: 0.0, stt_ms: started.elapsed().as_millis() as u64, audio_ms, avg_logprob: 0.0, no_speech: 1.0 };
    if let Some(why) = energy_gate(&pcm, voiced_ms) {
        info!("[STT] Skipped {audio_ms} ms of audio without Whisper: {why} (breath, hiss or a click)");
        return Ok(nothing(started));
    }
    let Some(audio) = preprocess(&pcm) else {
        return Ok(nothing(started));
    };

    let (language, language_prob) = match s.languages.len() {
        0 => (None, 0.0),
        1 => (whisper_rs::get_lang_id(&s.languages[0]).and_then(whisper_rs::get_lang_str), 1.0),
        _ => match detect_language(detect_state.unwrap_or(&mut *state), &audio, s) {
            Some((code, p)) => (Some(code), p),
            None => (None, 0.0),
        },
    };
    // The language pass cannot be interrupted; a head start called off meanwhile stops here.
    if called_off.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return Err(CALLED_OFF.into());
    }
    // German and English are both transcribed as spoken (`set_translate(false)`); the language
    // comes from detection among the allowed ones, so German is never forced into English.
    let decoded = run_full(state, &audio, pcm.len(), s, language, reflex_ctx(pcm.len(), voiced_ms), called_off)?;
    let raw = decoded.text;
    let no_speech = if raw.is_empty() { 0.0 } else { no_speech_probability(state, s).unwrap_or(0.0) };
    if no_speech > NO_SPEECH_THOLD && decoded.avg_logprob < CONFIDENT_LOGPROB {
        info!(
            "[STT] Discarded '{raw}' from {audio_ms} ms of audio: no-speech {no_speech:.2}, log-probability {:.2} (in {} ms)",
            decoded.avg_logprob,
            started.elapsed().as_millis()
        );
        return Ok(nothing(started));
    }
    let text = normalize_stt_transcript(&filter_hallucinations(&raw, &s.prompt, voiced_ms < 1500));
    let lang = language
        .map(String::from)
        .or_else(|| state.full_lang_id_from_state().ok().and_then(whisper_rs::get_lang_str).map(String::from))
        .unwrap_or_else(|| "auto".into());
    let stt_ms = started.elapsed().as_millis() as u64;
    info!(
        "[STT] Transcribed {audio_ms} ms of audio in {stt_ms} ms [{lang} p={language_prob:.2}, no-speech {no_speech:.2}, logprob {:.2}]: '{text}'{}",
        decoded.avg_logprob,
        if text.is_empty() && !raw.is_empty() { format!(" (filtered '{raw}')") } else { String::new() }
    );
    Ok(Transcript {
        text,
        language: lang,
        language_prob,
        stt_ms,
        audio_ms,
        avg_logprob: decoded.avg_logprob,
        no_speech,
    })
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Removes Whisper's classic silence/noise hallucinations and prompt echoes. `strict` also drops
/// short stock phrases ("Thank you.", "Danke.") that Whisper invents for coughs and clicks.
pub fn filter_hallucinations(raw: &str, prompt: &str, strict: bool) -> String {
    // Drop bracketed annotations: [Musik], (applause), *laughs*, music notes
    let mut text = String::with_capacity(raw.len());
    let mut depth = 0i32;
    for c in raw.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = (depth - 1).max(0),
            '\u{266A}' | '*' => {}
            _ if depth == 0 => text.push(c),
            _ => {}
        }
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let norm = normalize(&text);
    if norm.is_empty() || norm.chars().count() < 2 {
        return String::new();
    }

    const ALWAYS: &[&str] = &[
        "untertitel", "copyright", "music", "musik", "applause", "silence", "hm", "hmm", "mhm", "you", "subtitles", "watching", "mbc", "mbc news",
        "thanks for watching", "untertitelung",
    ];
    // Humming and sung syllables: no word in it ("hmm hmm", "la la la", "mmmm").
    const HUM: &[&str] = &["hm", "hmm", "hmmm", "mhm", "mm", "mmm", "mmmm", "uh", "um", "ah", "oh", "ooh", "la", "lala", "na", "nana", "da", "dum", "doo", "ba", "pa"];
    let one_sound = |w: &str| w.chars().count() >= 4 && w.chars().collect::<std::collections::BTreeSet<_>>().len() <= 2 && !w.chars().any(|c| c.is_ascii_digit());
    if norm.split_whitespace().all(|w| HUM.contains(&w) || one_sound(w)) {
        return String::new();
    }
    const STRICT: &[&str] = &[
        "you", "thank you", "thanks", "thank you very much", "bye", "bye bye", "danke", "danke schön",
        "vielen dank", "tschüss", "auf wiedersehen", "bis zum nächsten mal",
    ];
    if ALWAYS.contains(&norm.as_str()) || (strict && STRICT.contains(&norm.as_str())) {
        return String::new();
    }
    const CONTAINS: &[&str] = &[
        "untertitel im auftrag", "untertitel der amara", "untertitelung", "amara org", "subtitles by",
        "transcribed by", "thanks for watching", "thank you for watching", "vielen dank fürs zuschauen",
        "vielen dank für s zuschauen", "swr 2020", "zdf 2020", "bis zum nächsten video", "abonniert den kanal",
        "please subscribe", "subtitles", "untertitel", "mbc 뉴스", "mbc news",
    ];
    if CONTAINS.iter().any(|h| norm.contains(h)) {
        return String::new();
    }
    // Prompt echo: Whisper repeating (a long stretch of) its initial prompt on unclear audio.
    // Commands that happen to appear in the prompt ("Hey Jarvis, open my browser") stay.
    let prompt_norm = normalize(prompt);
    if norm.split_whitespace().count() >= 8 && prompt_norm.contains(&norm) {
        return String::new();
    }
    // A comma list of the prompt's own words ("Sonora, Spotify, WebUntis, itslearning."): the
    // domain vocabulary read back on quiet audio, not a command.
    let prompt_words: std::collections::HashSet<&str> = prompt_norm.split_whitespace().collect();
    if norm.split_whitespace().count() >= 4 && text.matches(',').count() >= 2 && norm.split_whitespace().all(|w| prompt_words.contains(w)) {
        return String::new();
    }
    // Non-Latin script output for German/English speech means a mis-decode.
    let letters: Vec<char> = text.chars().filter(|c| c.is_alphabetic()).collect();
    if !letters.is_empty() {
        let latin = letters.iter().filter(|c| c.is_ascii() || "äöüßÄÖÜéèàçñ".contains(**c)).count();
        if latin * 10 < letters.len() * 7 {
            return String::new();
        }
    }
    collapse_stutters(&collapse_repetitions(&dedupe_sentences(&text)))
}

/// "Make the interface red. Make the interface red." → one sentence: the beam search repeats
/// short sentences whole.
fn dedupe_sentences(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut last_key = String::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut pieces: Vec<&str> = Vec::new();
    for (i, b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'!' | b'?') && bytes.get(i + 1).is_none_or(|n| *n == b' ') {
            pieces.push(&text[start..=i]);
            start = i + 1;
        }
    }
    if start < text.len() {
        pieces.push(&text[start..]);
    }
    for piece in pieces {
        let key = normalize(piece);
        if key.is_empty() {
            continue;
        }
        if key == last_key && key.split_whitespace().count() >= 2 {
            continue;
        }
        last_key = key;
        out.push(piece.trim());
    }
    out.join(" ")
}

/// Decoder stutters: a word three or more times in a row ("the the the") is said once, and a
/// doubled function word ("the the", "to to") once as well; "very very" stays.
fn collapse_stutters(text: &str) -> String {
    const FUNCTION: &[&str] = &["the", "a", "an", "i", "to", "and", "of", "in", "it", "is", "der", "die", "das", "und", "ich", "zu", "ein"];
    let key = |w: &str| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let k = key(words[i]);
        let mut run = 1;
        while i + run < words.len() && !k.is_empty() && key(words[i + run]) == k {
            run += 1;
        }
        let keep = if run >= 3 || (run == 2 && FUNCTION.contains(&k.as_str())) { 1 } else { run };
        // The last copy keeps any punctuation that closes the run ("the the the end.").
        out.extend(&words[i + run - keep..i + run]);
        i += run;
    }
    out.join(" ")
}

/// Whisper's usual mishearings of Sir's own vocabulary: "paws" / "pos" → "pause", "un mute" →
/// "unmute", "senora" → "Sonora", "jay's rock" / "chase rock" → "JaysRock". Words that are
/// also ordinary English ("post", "pass", "on mute") are only corrected when they are the whole
/// command.
pub fn normalize_stt_transcript(text: &str) -> String {
    use regex::Regex;
    use std::sync::LazyLock;
    static ANYWHERE: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
        [
            (r"(?i)\b(?:senora|señora|sonorah|sonnora|so nora|sonara)\b", "Sonora"),
            (r"(?i)\b(?:jay'?s rock|jays rock|j's rock|chase rock|jace rock|jaysrock|jay rock)\b", "JaysRock"),
            (r"(?i)\bun[- ]mute\b", "unmute"),
            (r"(?i)\bp\.\s?o\.\s?s\.?(?:\s|$)", "pause "),
            (r"(?i)\b(?:paws|pauze|pawse|paus)\b", "pause"),
            (r"(?i)\bweb untis\b", "WebUntis"),
            (r"(?i)\bits learning\b", "itslearning"),
            (r"(?i)\bwhats app\b", "WhatsApp"),
            (r"(?i)\bhands free\b", "hands-free"),
            // The workspace trigger ("bring up everything you know about …") heard sloppily.
            (r"(?i)\bbring(?:ing)? op\b|\bbringup\b|\bbrin up\b", "bring up"),
            (r"(?i)\bpull op\b|\bpullup\b|\bpool up\b", "pull up"),
            (r"(?i)\bshow mi\b", "show me"),
            // German self-close and window commands as Whisper tends to spell them.
            (r"(?i)\b(?:schliesst|schließt|schliest|schlies|schliess) dich\b", "schließ dich"),
            (r"(?i)\b(?:beendet|beende) dich\b", "beende dich"),
            (r"(?i)\bmach (?:dich|dicht) zu\b", "mach dich zu"),
            (r"(?i)\bmach die fenster(?: wieder)? zu\b", "mach die Fenster wieder zu"),
            (r"(?i)\b(?:schliess|schließt|schliesst) die fenster\b", "schließ die Fenster"),
            (r"(?i)\bfenster schliessen\b", "Fenster schließen"),
            (r"(?i)\baus meiner (?:play list|play-list|pleylist|playlis)\b", "aus meiner Playlist"),
            (r"(?i)\bfrom my (?:play list|play-list|playlis)\b", "from my playlist"),
            (r"(?i)\berstell(?:e)? (?:ein|einen) (?:tul|tool|tuhl)\b", "erstelle ein Tool"),
            (r"(?i)\bclose (?:the )?(?:windows|window's) (?:you|u) (?:opened|open)\b", "close the windows you opened"),
            (r"(?i)\bsci[- ]?fi\b|\bscifi\b|\bsigh fi\b", "Sci-Fi"),
            (r"(?i)\bo led\b|\bo-led\b", "OLED"),
        ]
        .into_iter()
        .map(|(p, r)| (Regex::new(p).expect("valid STT correction"), r))
        .collect()
    });
    // "No, forget that idea" heard as "No fool forgets that idea." / "no full forget that".
    static MISHEARD_CANCEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:no )?(?:fool|full|foo|fall) (?:forgets?|forgot|for get) that(?: idea)?$").expect("valid cancel repair"));
    let mut out = text.trim().to_string();
    if out.is_empty() {
        return out;
    }
    for (re, rep) in ANYWHERE.iter() {
        out = re.replace_all(&out, *rep).trim().to_string();
    }
    // "Close your car" / "hide your call" / "for C-O-R-E": the core, shown right in the HUD too.
    out = crate::typos::core_homophones(&out);
    // Whole-utterance corrections ("Post." / "Pass, Jarvis" / "On mute").
    let bare: String = out
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter(|w| !matches!(*w, "jarvis" | "please" | "bitte"))
        .collect::<Vec<_>>()
        .join(" ");
    let whole = match bare.as_str() {
        "post" | "pass" | "pos" | "pose" | "pours" | "post it" | "pass it" | "post the music" | "pass the music" | "post music" | "pass music" | "boss"
        | "bos" | "bose" => Some("Pause."),
        // "Pause it" heard as "How's it", "Stop" clipped to "Top" when spoken over Jarvis.
        "how s it" | "hows it" | "house it" => Some("Pause it."),
        "top" | "stob" | "stopp" | "stop it now" => Some("Stop."),
        // "Weitermachen" in an English decode.
        "weitebachen" | "weiterbachen" | "weite machen" | "weitermachen" | "weiter machen" | "wider machen" => Some("Weitermachen."),
        "on mute" | "an mute" | "and mute" | "un mute" => Some("Unmute."),
        // "Jarvis, clip it" / "clipped it" / "clip that": Instant Replay of the last minute.
        "clip it" | "clipped it" | "clip that" | "clipped that" | "clip this" => Some("Jarvis, clip the last 60 seconds."),
        // "Show my stats" heard as "steps" (the model then printed a raw tool call).
        "show my steps" | "open my steps" | "show me my steps" | "show my step" | "show my staffs" | "show my stacks" => Some("Show my stats."),
        _ if MISHEARD_CANCEL.is_match(&bare) => Some("No, forget that idea."),
        _ => None,
    };
    match whole {
        Some(fixed) => fixed.to_string(),
        None => out.split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

/// The most probable allowed language; German needs `GERMAN_CERTAINTY` when English is allowed
/// as well (accented English must not be transcribed as German nonsense).
fn pick_language(allowed: &[(&'static str, f32)]) -> Option<(&'static str, f32)> {
    let best = allowed.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1))?;
    let english = allowed.iter().copied().find(|(name, _)| *name == "en");
    match (best.0, english) {
        ("de", Some(en)) if best.1 < GERMAN_CERTAINTY => Some(en),
        _ => Some(best),
    }
}

/// "go go go go go", a phrase or a whole sentence looped by the decoder → a single occurrence.
/// Short units (≤ 4 words) need four repeats ("no no" stays); five words or more twice.
fn collapse_repetitions(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let key = |w: &&str| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    let keys: Vec<String> = words.iter().map(key).collect();
    for n in 1..=16 {
        let needed = if n <= 4 { 4 } else { 2 };
        if words.len() < n * needed {
            continue;
        }
        let head = &keys[..n];
        let repeats = keys.chunks(n).take_while(|c| c.len() == n && *c == head).count();
        if repeats >= needed {
            // A trailing partial copy of the loop goes too (its last word may be cut off).
            let mut rest = repeats * n;
            while rest < keys.len() {
                let expected = &head[(rest - repeats * n) % n];
                let last = rest + 1 == keys.len();
                if keys[rest] == *expected || (last && !keys[rest].is_empty() && expected.starts_with(keys[rest].as_str())) {
                    rest += 1;
                } else {
                    break;
                }
            }
            return words[..n].iter().chain(words[rest..].iter()).copied().collect::<Vec<_>>().join(" ");
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LEGACY_WHISPER_PROMPT;

    #[test]
    fn phonetic_intent_repair() {
        // The screenshot: "No, forget that idea" misheard.
        assert_eq!(normalize_stt_transcript("No fool forgets that idea."), "No, forget that idea.");
        assert_eq!(normalize_stt_transcript("no full forget that"), "No, forget that idea.");
        assert_eq!(normalize_stt_transcript("Fool forget that."), "No, forget that idea.");
        assert!(crate::intent_engine::is_cancel(&normalize_stt_transcript("No fool forgets that idea.")));
        // Instant Replay.
        assert_eq!(normalize_stt_transcript("Jarvis, clip it."), "Jarvis, clip the last 60 seconds.");
        assert_eq!(normalize_stt_transcript("Jarvis clipped it"), "Jarvis, clip the last 60 seconds.");
        assert_eq!(normalize_stt_transcript("Clip that!"), "Jarvis, clip the last 60 seconds.");
        let route = crate::features::route_text(&normalize_stt_transcript("clip that")).expect("replay route");
        assert_eq!((route.tool, route.args["seconds"].as_u64()), ("save_instant_replay", Some(60)));
        // A single "pos" / "paws" / "post" is "pause".
        for heard in ["Pos.", "Paws", "Post."] {
            assert_eq!(normalize_stt_transcript(heard).to_lowercase().trim_end_matches('.'), "pause", "{heard}");
        }
        // The workspace trigger, heard sloppily.
        assert_eq!(normalize_stt_transcript("bring op everything you know about Spotify"), "bring up everything you know about Spotify");
        assert_eq!(normalize_stt_transcript("pullup all data on Sonora"), "pull up all data on Sonora");
        assert_eq!(crate::intent_engine::knowledge_workspace_topic(&normalize_stt_transcript("show mi everything on Spotify")).as_deref(), Some("Spotify"));
        // Ordinary speech is untouched.
        assert_eq!(normalize_stt_transcript("Forget that I like jazz"), "Forget that I like jazz");
    }

    #[test]
    fn repeated_short_sentences_are_heard_once() {
        assert_eq!(dedupe_sentences("Make the interface right. Make the interface right."), "Make the interface right.");
        assert_eq!(dedupe_sentences("Open the interface settings. Open the interface settings."), "Open the interface settings.");
        assert_eq!(dedupe_sentences("Stop. Stop."), "Stop. Stop.", "a single word may be said twice");
        assert_eq!(dedupe_sentences("Hey Jarvis. What's up? Can you change the interface to red?"), "Hey Jarvis. What's up? Can you change the interface to red?");
        assert_eq!(normalize_stt_transcript("Top."), "Stop.");
        assert_eq!(normalize_stt_transcript("Boss."), "Pause.");
        assert_eq!(normalize_stt_transcript("How's it?"), "Pause it.");
        assert_eq!(normalize_stt_transcript("Weitebachen."), "Weitermachen.");
        assert_eq!(normalize_stt_transcript("The top of the page"), "The top of the page");
    }

    #[test]
    fn decoder_loops_collapse_to_one_sentence() {
        let looped = "Changes the interface color to red. Changes the interface color to red. Changes the interface color to red. Changes the interface colo";
        assert_eq!(collapse_repetitions(looped), "Changes the interface color to red.");
        assert_eq!(collapse_repetitions("go go go go go now"), "go now");
        assert_eq!(collapse_repetitions("no no, not that"), "no no, not that", "a real double stays");
        assert_eq!(collapse_repetitions("Open Spotify and open Discord"), "Open Spotify and open Discord");
    }

    #[test]
    fn accented_english_is_not_transcribed_as_german() {
        assert_eq!(pick_language(&[("de", 0.74), ("en", 0.21)]), Some(("en", 0.21)));
        assert_eq!(pick_language(&[("de", 0.87), ("en", 0.1)]), Some(("en", 0.1)));
        assert_eq!(pick_language(&[("de", 0.96), ("en", 0.02)]), Some(("de", 0.96)), "real German");
        assert_eq!(pick_language(&[("de", 0.4)]), Some(("de", 0.4)), "German only");
        assert_eq!(pick_language(&[("en", 0.9), ("de", 0.05)]), Some(("en", 0.9)));
        assert_eq!(pick_language(&[]), None);
    }

    #[test]
    fn hallucination_filter() {
        let p = LEGACY_WHISPER_PROMPT;
        let f = |s: &str| filter_hallucinations(s, p, true);
        assert_eq!(f(" Thank you."), "");
        assert_eq!(filter_hallucinations(" Thank you.", p, false), "Thank you.");
        assert_eq!(filter_hallucinations("Okay.", p, true), "Okay.");
        assert_eq!(f("[Musik]"), "");
        assert_eq!(f("Untertitel im Auftrag des ZDF, 2020"), "");
        assert_eq!(f("Jarvis, pause, resume, play, stop, mute, unmute, louder, quieter, Sonora, Spotify, JaysRock."), "");
        assert_eq!(f("Open BastiGHG on YouTube, search Minecraft, Twitch, Discord, Steam."), "");
        assert_eq!(f("Hmm hmm hmm."), "", "humming");
        assert_eq!(f("La la la la."), "", "sung syllables");
        assert_eq!(f("Mmmmm"), "");
        assert_eq!(f("Hey Jarvis, open my browser."), "Hey Jarvis, open my browser.");
        assert_eq!(f("Open BastiGHG on YouTube."), "Open BastiGHG on YouTube.", "a real command that is also in the prompt");
        assert_eq!(f("Hey Jarvis, öffne Spotify."), "Hey Jarvis, öffne Spotify.");
        assert_eq!(f("Jarvis, wie spät ist es?"), "Jarvis, wie spät ist es?");
        assert_eq!(f("(laughs) Open Discord please"), "Open Discord please");
        assert_eq!(f("你好你好你好"), "");
        assert_eq!(f("stop stop stop stop stop now"), "stop now");
        // Whisper's phantom lines on silence and noise.
        for phantom in ["You", "You.", "Subtitles by the Amara.org community", "Untertitel von Stephanie Geiges", "MBC 뉴스 김철수입니다", "Watching.", "Vielen Dank."] {
            assert_eq!(f(phantom), "", "{phantom}");
        }
        assert_eq!(f("Thank you, Jarvis, that was perfect."), "Thank you, Jarvis, that was perfect.", "real thanks stay");
        assert_eq!(f("Open the the the browser"), "Open the browser");
        assert_eq!(f("Play the the music"), "Play the music");
        assert_eq!(f("That is very very good"), "That is very very good");
    }

    #[test]
    fn breath_hiss_and_clicks_never_reach_whisper() {
        let mut seed = 7u32;
        let mut noise = |n: usize, level: f32| -> Vec<f32> {
            (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    ((seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0) * level
                })
                .collect()
        };
        // Room noise with a 40 ms click in it: loud, but not speech.
        let mut click = noise(16_000, 0.004);
        click[8000..8640].iter_mut().enumerate().for_each(|(i, s)| *s += ((i as f32) * 0.9).sin() * 0.3);
        assert!(energy_gate(&click, 400).is_some(), "a click");
        assert!(energy_gate(&noise(16_000, 0.004), 120).is_some(), "too little voiced audio");
        assert!(energy_gate(&noise(16_000, 0.0015), 400).is_some(), "near-silent hiss");
        // Room noise, then 500 ms of a voice-like tone 20 dB above it.
        let mut word = noise(16_000, 0.004);
        word[5000..13000].iter_mut().enumerate().for_each(|(i, s)| *s += ((i as f32) * 0.07).sin() * 0.08);
        assert_eq!(energy_gate(&word, 500), None, "a short word passes");
    }

    #[test]
    fn stt_vocabulary_corrections() {
        assert_eq!(normalize_stt_transcript("Paws."), "pause.");
        assert_eq!(normalize_stt_transcript("Post."), "Pause.");
        assert_eq!(normalize_stt_transcript("Pass, Jarvis."), "Pause.");
        assert_eq!(normalize_stt_transcript("Post this on WhatsApp"), "Post this on WhatsApp", "a real sentence keeps its words");
        assert_eq!(normalize_stt_transcript("Un mute"), "unmute");
        assert_eq!(normalize_stt_transcript("On mute."), "Unmute.");
        assert_eq!(normalize_stt_transcript("Put Discord on mute"), "Put Discord on mute");
        assert_eq!(normalize_stt_transcript("Set Senora to 30 percent"), "Set Sonora to 30 percent");
        assert_eq!(normalize_stt_transcript("play so nora"), "play Sonora");
        assert_eq!(normalize_stt_transcript("Play Back in Black from Jay's Rock"), "Play Back in Black from JaysRock");
        assert_eq!(normalize_stt_transcript("play chase rock"), "play JaysRock");
        assert_eq!(normalize_stt_transcript("Switch to hands free mode"), "Switch to hands-free mode");
        assert_eq!(normalize_stt_transcript(""), "");
    }

    #[test]
    fn audio_ctx_scales_with_length() {
        // (secs / 30 × 1500) + 64, at least 128 and the configured floor.
        assert_eq!(audio_ctx_for(MIN_SAMPLES, 128), 129, "a padded 1.3 s command");
        assert_eq!(audio_ctx_for(16_000 / 2, 64), 128, "never below 128");
        assert_eq!(audio_ctx_for(16_000 * 2, 128), 164);
        assert_eq!(audio_ctx_for(16_000 * 2, 384), 384, "the configured floor wins");
        assert_eq!(audio_ctx_for(16_000 * 10, 128), 564);
        assert_eq!(audio_ctx_for(16_000 * 30, 128), 1500);
        assert_eq!(audio_ctx_for(16_000 * 40, 128), 1500);
    }

    #[test]
    fn reflex_words_get_the_small_window() {
        assert_eq!(reflex_ctx(16_000 / 2, 500), Some(96), "a 0.5 s 'mute'");
        assert_eq!(reflex_ctx(MIN_SAMPLES, 850), Some(96));
        assert_eq!(reflex_ctx(MIN_SAMPLES + 1, 800), None, "longer than the padded clip");
        assert_eq!(reflex_ctx(16_000, 900), None, "more voice than a word");
    }

    #[test]
    fn english_only_models_are_recognised() {
        assert!(is_english_only("models/ggml-base.en.bin"));
        assert!(is_english_only("models/ggml-small.en-q8_0.bin"));
        assert!(!is_english_only("models/ggml-small.bin"));
        assert!(!is_english_only("models/ggml-base-q8_0.bin"));
        assert_eq!(prefer_multilingual("models/ggml-small.bin"), "models/ggml-small.bin");
    }

    #[test]
    fn decoder_budget_scales_with_length() {
        assert_eq!(max_tokens_for(16_000 / 2), 24, "never below 24");
        assert_eq!(max_tokens_for(16_000 * 3 / 2), 31, "a padded 1.5 s command");
        assert_eq!(max_tokens_for(16_000 * 10), 116);
        assert_eq!(max_tokens_for(16_000 * 20), 216, "the longest utterance");
        assert_eq!(max_tokens_for(16_000 * 30), 220, "whisper.cpp's own cap");
    }

    #[test]
    fn preprocess_pads_short_audio_and_rejects_silence() {
        assert!(preprocess(&vec![0.0; 8000]).is_none());
        let tone: Vec<f32> = (0..8000).map(|i| (i as f32 * 0.2).sin() * 0.05).collect();
        let out = preprocess(&tone).unwrap();
        assert_eq!(out.len(), MIN_SAMPLES);
        assert!(dsp::peak(&out) <= 1.0);
    }

    #[test]
    fn whisper_hears_flat_speech_lifted_to_a_safe_peak() {
        // A quiet vowel ("o" of "core", 500 Hz) and a fricative (4 kHz): the preprocessed clip
        // keeps their balance (no pre-emphasis tilt) and peaks near 0.85 without clipping.
        let rate = SAMPLE_RATE as f32;
        let tone = |f: f32, a: f32| (0..24_000).map(move |i| a * (std::f32::consts::TAU * f * i as f32 / rate).sin());
        let speech: Vec<f32> = tone(500.0, 0.02).zip(tone(4000.0, 0.01)).map(|(v, s)| v + s).collect();
        let out = preprocess(&speech).unwrap();
        let peak = dsp::peak(&out);
        assert!(peak > 0.8 && peak < 1.0, "peak {peak}");
        // Goertzel-style band energy at both frequencies.
        let energy = |x: &[f32], f: f32| {
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, s) in x.iter().enumerate() {
                let a = std::f32::consts::TAU * f * i as f32 / rate;
                re += s * a.cos();
                im += s * a.sin();
            }
            (re * re + im * im).sqrt()
        };
        let before = energy(&speech, 500.0) / energy(&speech, 4000.0);
        let after = energy(&out, 500.0) / energy(&out, 4000.0);
        assert!((after / before - 1.0).abs() < 0.05, "vowel/consonant balance {before:.2} → {after:.2}");
    }

    #[test]
    fn sentences_get_the_beam_and_reflex_words_stay_greedy() {
        assert!(!use_beam(SAMPLE_RATE * 9 / 10, 2), "a 0.9 s \"stop\" decodes greedily");
        assert!(!use_beam(SAMPLE_RATE, 2));
        assert!(use_beam(SAMPLE_RATE * 13 / 10, 2), "a 1.3 s \"hide your core\" gets the beam");
        assert!(!use_beam(SAMPLE_RATE * 3, 1), "beam size 1 is greedy");
        assert_eq!(BEAM_PATIENCE, 1.0);
        let candidates = model_candidates("models/ggml-small.bin", "models/ggml-base.bin");
        assert_eq!(&candidates[..2], &["models/ggml-small.bin".to_string(), "models/ggml-base.bin".to_string()]);
        assert!(candidates.iter().filter(|c| c.as_str() == "models/ggml-small.bin").count() == 1, "no duplicates");
    }

    #[test]
    fn misheard_core_and_stats_are_repaired_in_the_transcript() {
        assert_eq!(normalize_stt_transcript("Close your car."), "Close your core.");
        assert_eq!(normalize_stt_transcript("Right, hide your call."), "Right, hide your core.");
        assert_eq!(normalize_stt_transcript("Hide your for C-O-R-E."), "Hide your core.");
        assert_eq!(normalize_stt_transcript("Show my steps."), "Show my stats.");
        // Ordinary sentences keep their words.
        assert_eq!(normalize_stt_transcript("Call my mom."), "Call my mom.");
        assert_eq!(normalize_stt_transcript("Park the car in the garage."), "Park the car in the garage.");
        // The domain prompt read back as a list is no command.
        let prompt = crate::config::WHISPER_DOMAIN_PROMPT;
        assert_eq!(filter_hallucinations("Sonora, Spotify, WebUntis, itslearning.", prompt, false), "");
        assert_eq!(filter_hallucinations("Hide core.", prompt, true), "Hide core.");
    }

    /// A developer tool: `JARVIS_TRANSCRIBE="a.wav;b.wav" cargo test transcribe_wav_files --
    /// --ignored --nocapture` prints what Whisper hears in 16 kHz mono 16-bit WAV files (in 10 s
    /// pieces, through the same pipeline as the microphone).
    #[test]
    #[ignore]
    fn transcribe_wav_files() {
        let Ok(list) = std::env::var("JARVIS_TRANSCRIBE") else { return };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let stt = SttEngine::start(&JarvisConfig::load());
        for path in list.split(';').filter(|p| !p.trim().is_empty()) {
            let bytes = std::fs::read(path.trim()).expect("readable WAV");
            let data = bytes.windows(4).position(|w| w == b"data").expect("a data chunk") + 8;
            let pcm: Vec<f32> = bytes[data..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
            println!("== {path} ({:.1} s)", pcm.len() as f32 / 16_000.0);
            for (k, piece) in pcm.chunks(SAMPLE_RATE * 10).enumerate() {
                let ms = (piece.len() * 1000 / SAMPLE_RATE) as u32;
                match rt.block_on(stt.transcribe(piece.to_vec(), ms)) {
                    Ok(t) => println!("  [{:>3} s] ({}) {}", k * 10, t.language, t.text),
                    Err(e) => println!("  [{:>3} s] error: {e}", k * 10),
                }
            }
        }
    }
}

