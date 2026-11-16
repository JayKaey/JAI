//! Orchestrator: wires audio events → STT → wake-word gating → brain turns → TTS, keeps the HUD
//! state machine (idle / listening / thinking / speaking / interrupted / sleeping) and handles
//! UI commands.
//!
//! Every turn is tagged with its `InputSource`; Sir's `VoiceResponseMode` decides whether it is
//! spoken. Voice turns are *stitched*: when Sir resumes speaking while his previous words are
//! still being transcribed, or before JAI has acted or made a sound, the premature turn is
//! withdrawn and the new speech is appended to the earlier transcript as one request.

use crate::brain::{Brain, BrainEvent, TurnLink, TurnRequest};
use crate::config::ConfigStore;
use crate::ears::{AudioControls, AudioEvent, Utterance};
use crate::hud::{ConfigState, HudCommand, HudEvent};
use crate::lifecycle::{self, Signal};
use crate::modality::{InputSource, Trigger};
use crate::runtime::{LlmRuntime, Phase};
use crate::stt::SttEngine;
use crate::telemetry::SystemTelemetry;
use crate::tools::{ToolContext, ToolOutcome};
use crate::tts::{TtsController, TtsEvent};
use crate::vad::EndReason;
use crate::wake::WakeWordMatcher;
use crate::widgets::WidgetKind;
use serde::Serialize;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// A voice turn younger than this may still be withdrawn when Sir keeps talking.
const STITCH_TURN_MAX_AGE: Duration = Duration::from_secs(15);
/// Words waiting for the rest of the sentence expire after this.
const CARRY_MAX_AGE: Duration = Duration::from_secs(25);
/// After a bare "JAI", how long JAI waits for the rest before he only listens on — with
/// the 160 ms reflex cutoff this keeps the old ~0.7 s of room for a breath.
const BARE_WAKE_GRACE: Duration = Duration::from_millis(560);

/// The audio of a sentence Sir paused in ("JAI, … change the colour"): the next utterance is
/// transcribed together with it, so Whisper hears the whole sentence instead of fragments.
struct CarryAudio {
    id: u64,
    pcm: Vec<f32>,
    speech_ms: u32,
    barge_in: bool,
    ptt: bool,
    /// The piece was spoken to him (`held_for_him`): only then does the sentence it becomes
    /// part of count as said in a conversation with him.
    addressed: bool,
    at: Instant,
}

/// Whether a piece of speech that is held for the rest of its sentence was spoken to him. One
/// that was judged and accepted was (his name, his window, an order he knows: `judged`); one
/// that was held before anything was known of it (Sir was already speaking again, or Whisper
/// found no words in it) was only if it began inside a conversation with him, with the
/// push-to-talk key held, or over his own voice; and a piece joined to an earlier one inherits
/// what that one was (`earlier`).
fn held_for_him(judged: bool, earlier: Option<bool>, began_in_window: bool, ptt: bool, barge_in: bool) -> bool {
    judged || earlier == Some(true) || began_in_window || ptt || barge_in
}

/// Layer A of the addressee detector for one sentence: whether it is heard as part of a
/// conversation with him. It began inside the conversation window (or during a turn), words of
/// his are held for it (`held_words`: always from a sentence that was his to hear), the window
/// is open or a turn is running right now, or it was stitched from held audio that was itself
/// spoken to him (`held_audio`: Some(whether it was) for a sentence heard together with a held
/// piece).
///
/// (2026-11-13, 18:11: a song played in the room. Whisper found no words in a first piece, the
/// song went on, and the piece was held "for the rest". Stitched to the next one it counted as
/// said in a conversation for being stitched alone: with the window closed for a minute and no
/// name in it, a lyric went to the model as a follow-up, without a `[WAKE]` line. A stitched
/// sentence is judged like any other; the stitching proves nothing about whom it was for.)
fn in_conversation(began_in_window: bool, held_words: bool, held_audio: Option<bool>, window_open: bool, turn_active: bool) -> bool {
    began_in_window || held_words || held_audio == Some(true) || window_open || turn_active
}

/// Silence put between two stitched pieces of audio (16 kHz samples, 150 ms).
const STITCH_GAP_SAMPLES: usize = 16_000 * 150 / 1000;
/// Whisper hears at most 30 s: stitched audio keeps its newest 28 s.
const STITCH_MAX_SAMPLES: usize = 16_000 * 28;
/// A clip longer than this (8 s) gets no second hearing: that is talk, not an order.
const SECOND_HEARING_MOST: usize = 16_000 * 8;

/// A dispatched voice turn this young is re-heard with the rest of the sentence; older ones are
/// stitched as text.
const STITCH_AUDIO_MAX_AGE: Duration = Duration::from_secs(6);

/// Two pieces of one sentence with a short silence between them (the newest 28 s at most).
pub(crate) fn join_audio(mut first: Vec<f32>, second: Vec<f32>) -> Vec<f32> {
    first.extend(std::iter::repeat_n(0.0, STITCH_GAP_SAMPLES));
    first.extend(second);
    if first.len() > STITCH_MAX_SAMPLES {
        first.drain(..first.len() - STITCH_MAX_SAMPLES);
    }
    first
}

/// Sir's unfinished words, waiting for the rest of the sentence.
struct Carry {
    id: u64,
    text: String,
    at: Instant,
}

/// A head start: the words so far, being transcribed while it is not yet certain that Sir has
/// finished (`AudioEvent::Paused`). When the utterance ends with exactly this audio, the
/// transcript is taken from here; when he carries on, the decode is called off.
struct HeadStart {
    samples: usize,
    speech_ms: u32,
    /// When Whisper was given the audio.
    at: Instant,
    called_off: Arc<AtomicBool>,
    transcript: tokio::sync::watch::Receiver<Option<Result<crate::stt::Transcript, String>>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HudState {
    pub state: String,
    pub detail: String,
}

struct ActiveTurn {
    id: String,
    cancel: CancellationToken,
    generation: u64,
    /// End of speech (or submit) – the reference point for latency metrics.
    started: Instant,
    stt_ms: u64,
    first_token_ms: Option<u64>,
    first_audio_logged: bool,
    /// Conversation window opens after this turn's audio finishes.
    opens_window: bool,
    /// None for JAI's own lines (greeting, alerts).
    source: Option<InputSource>,
    /// Sir's request of a voice turn (what gets stitched when he keeps talking).
    request: Option<String>,
    /// Its audio (16 kHz) and voiced milliseconds, re-heard with the rest of the sentence.
    audio: Option<(Vec<f32>, u32)>,
    link: Arc<TurnLink>,
}

struct Inner {
    config: ConfigStore,
    controls: Arc<AudioControls>,
    brain: Brain,
    stt: SttEngine,
    tts: TtsController,
    events: broadcast::Sender<HudEvent>,
    brain_tx: mpsc::UnboundedSender<BrainEvent>,
    wake: RwLock<WakeWordMatcher>,
    turn: Mutex<Option<ActiveTurn>>,
    hud: Mutex<HudState>,
    llm: Arc<LlmRuntime>,
    /// Cancelled to quit the app (HUD power button, tray menu, voice, Ctrl+C).
    shutdown: CancellationToken,
    /// A spoken goodbye is playing; the app quits when it ends.
    quitting: AtomicBool,
    /// Incremented whenever Sir starts speaking (an utterance whose epoch moved on while it was
    /// being transcribed is not finished yet).
    speech_epoch: AtomicU64,
    carry: Mutex<Option<Carry>>,
    carry_seq: AtomicU64,
    /// Audio of an unfinished sentence (see `CarryAudio`).
    carry_audio: Mutex<Option<CarryAudio>>,
    /// The words of the current pause, already with Whisper (see `HeadStart`).
    head_start: Mutex<Option<HeadStart>>,
    /// The live line's decode of the words so far, while it runs (its call-off flag).
    live_decode: Mutex<Option<Arc<AtomicBool>>>,
    /// The audio of the voice request about to be dispatched (moved into its `ActiveTurn`).
    turn_audio: Mutex<Option<(Vec<f32>, u32)>>,
    /// Utterances decide about stitching one after the other (a later one waits for the earlier).
    utterance_order: tokio::sync::Mutex<()>,
    /// Utterances being transcribed right now.
    stt_pending: AtomicUsize,
    /// How Sir last talked to JAI (voice macros run their steps the same way).
    last_source: Mutex<InputSource>,
    /// A voice macro is running its steps.
    macro_running: AtomicBool,
    /// The last few things heard (addressed or not): the retroactive wake word stitches them.
    heard: Mutex<crate::ears::RollingSpeechTranscriptBuffer>,
    /// The utterance being dispatched was spoken over JAI's own voice.
    barge_in_turn: AtomicBool,
    /// How sure Whisper was of the utterance being dispatched (taken by `dispatch_turn`).
    heard_turn: Mutex<Option<crate::stt::HeardQuality>>,
    /// The last requests and replies, for the hands-free continuity check.
    recent_turns: Mutex<std::collections::VecDeque<String>>,
}

#[derive(Clone)]
pub struct Jai {
    inner: Arc<Inner>,
}

pub struct Channels {
    pub audio: mpsc::UnboundedReceiver<AudioEvent>,
    pub tts: mpsc::UnboundedReceiver<TtsEvent>,
}

/// What a burst of commands spoken over JAI left behind (`JAI::probe_barge_in`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BargeIn {
    /// A turn was running (he was speaking) when the first command arrived.
    pub was_speaking: bool,
    /// How long it took to hand all commands to the dispatcher.
    pub fired_in: Duration,
    /// When no turn was running and the thinking guard was released again (None: never, within
    /// the limit).
    pub settled: Option<Duration>,
    /// The thinking guard is still held.
    pub thinking: bool,
    /// A turn is still running.
    pub busy: bool,
    /// The sentence he was saying was cut off.
    pub speech_cut: bool,
}

/// What the dispatcher does with a request before any turn exists (`JAI::admit`).
#[derive(Debug, Clone, PartialEq)]
pub enum Admission {
    /// The privacy lock and nothing else.
    Privacy(crate::listening_lock::LockCommand),
    /// Several commands in one sentence: the privacy switch among them, and the other clauses
    /// as one request for the brain.
    Compound { privacy: Option<crate::listening_lock::LockCommand>, rest: String },
    /// A turn for the brain.
    Turn,
}

fn now_hms() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

impl Jai {
    pub fn start(
        config: ConfigStore,
        controls: Arc<AudioControls>,
        brain: Brain,
        stt: SttEngine,
        tts: TtsController,
        channels: Channels,
        shutdown: CancellationToken,
    ) -> Self {
        let llm = Arc::clone(brain.llm());
        let (events, _) = broadcast::channel(1024);
        let (brain_tx, brain_rx) = mpsc::unbounded_channel();
        let wake = config.read(|c| WakeWordMatcher::new(&c.wake_words, c.wake_word_similarity_threshold));
        let app = Self {
            inner: Arc::new(Inner {
                config,
                controls,
                brain,
                stt,
                tts,
                events,
                brain_tx,
                wake: RwLock::new(wake),
                turn: Mutex::new(None),
                hud: Mutex::new(HudState { state: "idle".into(), detail: "Starting up".into() }),
                llm,
                shutdown,
                quitting: AtomicBool::new(false),
                speech_epoch: AtomicU64::new(0),
                carry: Mutex::new(None),
                carry_seq: AtomicU64::new(0),
                carry_audio: Mutex::new(None),
                head_start: Mutex::new(None),
                live_decode: Mutex::new(None),
                turn_audio: Mutex::new(None),
                utterance_order: tokio::sync::Mutex::new(()),
                stt_pending: AtomicUsize::new(0),
                last_source: Mutex::new(InputSource::Voice),
                macro_running: AtomicBool::new(false),
                heard: Mutex::new(crate::ears::RollingSpeechTranscriptBuffer::default()),
                barge_in_turn: AtomicBool::new(false),
                heard_turn: Mutex::new(None),
                recent_turns: Mutex::new(std::collections::VecDeque::new()),
            }),
        };
        app.spawn_loops(channels, brain_rx);
        // Background work (a self-rebuild) talks to Sir through these, strictly in order (an
        // announcement is spoken before the shutdown that follows it).
        let (signals, mut pending) = mpsc::unbounded_channel::<Signal>();
        lifecycle::set_hook(move |signal| {
            let _ = signals.send(signal);
        });
        let hook = app.clone();
        tokio::spawn(async move {
            while let Some(signal) = pending.recv().await {
                match signal {
                    Signal::Notice(message) => hook.emit(HudEvent::Notice { message }),
                    Signal::Alert(message) => hook.raise_alert(message),
                    Signal::Shutdown => hook.shutdown_after_reply(),
                }
            }
        });
        // Sir's Discord call (off until he asks): Whisper, the voice and the tools for the bridge.
        crate::discord_bridge::install(app.inner.stt.clone(), app.inner.tts.clone(), app.tools().clone());
        // JAI's own Discord account (connects once Sir has pasted its token).
        crate::discord_bot::start(app.clone(), app.inner.stt.clone(), app.inner.tts.clone(), app.inner.shutdown.clone());
        app
    }

    // ---------------------------------------------------------------- accessors ---------

    pub fn config(&self) -> &ConfigStore {
        &self.inner.config
    }

    pub fn tools(&self) -> &ToolContext {
        self.inner.brain.tools()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HudEvent> {
        self.inner.events.subscribe()
    }

    pub fn telemetry(&self) -> SystemTelemetry {
        self.tools().monitor.latest()
    }

    /// A turn is running (thinking or speaking).
    pub fn busy(&self) -> bool {
        self.inner.controls.turn_active.load(Ordering::Acquire)
    }

    /// One request through the real dispatcher stages and the real brain turn as a dry run (the
    /// live self-test): what it would have done, and how long deciding took. Nothing is carried
    /// out, spoken or logged. `voice`: as if Sir had said it (else typed).
    pub async fn dry_run(&self, text: &str, voice: bool) -> (Vec<crate::live_self_tester::Recorded>, Duration) {
        use crate::live_self_tester::Recorded;
        let started = Instant::now();
        let privacy = |cmd: crate::listening_lock::LockCommand| Recorded { kind: "privacy", tool: "listening_lock".into(), args: json!({ "command": format!("{cmd:?}") }) };
        // The privacy lock answers in the dispatcher, before any turn exists; in a sentence that
        // asks for more, the other clauses go on to the brain.
        let mut recorded: Vec<Recorded> = Vec::new();
        let text = match Self::admit(text) {
            Admission::Privacy(cmd) => return (vec![privacy(cmd)], started.elapsed()),
            Admission::Compound { privacy: lock, rest } => {
                recorded.extend(lock.map(privacy));
                if rest.is_empty() {
                    return (recorded, started.elapsed());
                }
                rest
            }
            Admission::Turn => text.to_string(),
        };
        let (events, _kept) = mpsc::unbounded_channel();
        let req = TurnRequest {
            text,
            message_id: uuid::Uuid::new_v4().to_string(),
            generation: self.inner.tts.generation(),
            cancel: CancellationToken::new(),
            started,
            source: if voice { InputSource::Voice } else { InputSource::TextGui },
            voice: crate::modality::VoiceGate::Silent,
            link: Arc::new(TurnLink::default()),
            barge_in: false,
        };
        recorded.extend(crate::live_self_tester::record(Box::pin(self.inner.brain.run_turn(req, &self.inner.tts, &events))).await);
        (recorded, started.elapsed())
    }

    /// An order JAI gives himself whose answer Sir is to hear (the catch-up after a long time
    /// away): carried out like a spoken order, so it is answered aloud where a typed one is
    /// answered in writing.
    pub fn run_spoken(&self, order: &str) {
        self.dispatch_turn(order.to_string(), Instant::now(), 0, InputSource::Voice);
    }

    /// The barge-in pass of the exhaustive verifier: JAI starts saying `line`, then `commands`
    /// arrive `gap` apart through the real dispatcher (each cancels the turn before it). What
    /// had happened once everything settled, or after `limit`. Meant for a process in which
    /// every request is a dry run (`live_self_tester::set_everything_dry`).
    pub async fn probe_barge_in(&self, line: &str, commands: &[&str], gap: Duration, limit: Duration) -> BargeIn {
        self.speak_standalone(line, false, Trigger::Forced);
        let spoken = self.inner.tts.generation();
        // Until his voice is really in the air (the first clause is synthesised first).
        let waiting = Instant::now();
        while !self.inner.tts.is_speaking() && waiting.elapsed() < Duration::from_secs(5) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let was_speaking = self.inner.tts.is_speaking() && self.busy();
        let started = Instant::now();
        for (i, command) in commands.iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(gap).await;
            }
            self.dispatch_turn(command.to_string(), Instant::now(), 0, InputSource::Voice);
        }
        let fired_in = started.elapsed();
        let mut settled = None;
        while started.elapsed() < limit {
            if !self.busy() && !crate::thinking_guard::is_thinking() {
                settled = Some(started.elapsed());
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        BargeIn { was_speaking, fired_in, settled, thinking: crate::thinking_guard::is_thinking(), busy: self.busy(), speech_cut: self.inner.tts.generation() != spoken }
    }

    /// What the speech recogniser makes of `pcm` (16 kHz mono). The exhaustive verifier hands it
    /// JAI's own sentences as samples (no speaker, no microphone) to check that his voice is
    /// understood. Nothing else happens with the words.
    pub async fn hear(&self, pcm: Vec<f32>) -> Result<String, String> {
        let voiced_ms = (pcm.len() as u64 * 1000 / 16_000) as u32;
        self.inner.stt.transcribe(pcm, voiced_ms).await.map(|t| t.text)
    }

    /// What the wake word leaves of a heard sentence, as the voice path takes it; `None` when
    /// his name is not in it (the exhaustive verifier's spoken commands).
    pub fn wake_command(&self, heard: &str) -> Option<String> {
        let wake = self.inner.wake.read().unwrap_or_else(|e| e.into_inner()).evaluate(heard);
        wake.matched.then_some(wake.command)
    }

    /// Where the conversation history stands (the self-test rolls its dry turns back to here).
    pub async fn history_mark(&self) -> usize {
        self.inner.brain.history_len().await
    }

    pub async fn history_rollback(&self, mark: usize) {
        self.inner.brain.history_truncate(mark).await;
    }

    /// Something Sir must hear about (reminder, finished timer, overheating GPU). Spoken right
    /// away when JAI is idle, otherwise as soon as the current answer is finished — an alert
    /// never cuts a reply short.
    /// A notice in the HUD (toast).
    pub fn notice(&self, message: String) {
        self.emit(HudEvent::Notice { message });
    }

    pub fn raise_alert(&self, message: String) {
        // While Sir is away: kept for his digest and sent to his phone (Discord DM).
        crate::away_mode::relay_alert(&message);
        self.emit(HudEvent::Notice { message: message.clone() });
        // In a game: a line over it as well, which takes no focus and no click.
        if crate::vram_guard::in_game() {
            crate::live_subtitles::toast(&message);
        }
        // Sir's phone shows it too (the mobile satellite).
        self.tools().stark.push_to_satellite(&message);
        if !self.busy() || self.inner.quitting.load(Ordering::Acquire) {
            self.speak_standalone(&message, false, Trigger::Alert);
            return;
        }
        let app = self.clone();
        tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_secs(45);
            while app.busy() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
            app.speak_standalone(&message, false, Trigger::Alert);
        });
    }

    /// VRAM (MB) the language engine's loaded models hold (`/api/ps`).
    async fn llm_vram_mb(&self) -> Option<u64> {
        let url = format!("{}/api/ps", self.inner.llm.url());
        let resp = reqwest::Client::new().get(url).timeout(Duration::from_secs(2)).send().await.ok()?;
        let v: serde_json::Value = resp.json().await.ok()?;
        Some(v["models"].as_array()?.iter().filter_map(|m| m["size_vram"].as_u64()).sum::<u64>() / 1_048_576)
    }

    /// "Good evening, Sir. All systems are online. You have 2 reminders today." (varied each start)
    pub fn startup_greeting(&self, restarted: bool) -> String {
        use chrono::Timelike;
        if restarted {
            return "I'm back online, Sir.".into();
        }
        let now = chrono::Local::now();
        // One look before the greeting: what is wrong is said instead of "all systems online".
        let missing: Vec<&str> = crate::models::missing(&self.inner.config.snapshot()).iter().map(|f| f.label).collect();
        let problem = crate::briefing::startup_problem(&missing, &self.tools().monitor.latest().disks);
        let mut line = crate::briefing::startup_line_with(now.hour(), now.timestamp_subsec_micros() as usize, problem.as_deref());
        let today = self.tools().reminders.list().into_iter().filter(|r| r.due().date_naive() == now.date_naive()).count();
        match today {
            0 => {}
            1 => line.push_str(" You have one reminder today."),
            n => line.push_str(&format!(" You have {n} reminders today.")),
        }
        line
    }

    pub fn hud_state(&self) -> HudState {
        self.inner.hud.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn emit(&self, ev: HudEvent) {
        let _ = self.inner.events.send(ev);
    }

    fn turn_lock(&self) -> std::sync::MutexGuard<'_, Option<ActiveTurn>> {
        self.inner.turn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn config_state(&self) -> ConfigState {
        let c = &self.inner.controls;
        let (start, release, multiplier, silence) = c.vad_live();
        let cfg = self.inner.config.snapshot();
        ConfigState {
            mic_enabled: c.mic_enabled.load(Ordering::Relaxed),
            ptt_active: c.ptt_active.load(Ordering::Relaxed),
            wake_word_required: c.wake_word_required.load(Ordering::Relaxed),
            vad_start_threshold: start,
            vad_release_threshold: release,
            silence_ms: silence,
            noise_multiplier: multiplier,
            window_secs: cfg.active_conversation_window_secs,
            model: cfg.primary_model,
            fallback_model: cfg.fallback_model,
            voice: self.inner.tts.voice(),
            voices: self.inner.tts.voices(),
            loudness_pct: self.inner.tts.volume(),
            requested_mic: c.requested_mic(),
            active_mic: c.active_mic(),
            requested_speaker: self.inner.tts.requested_device(),
            active_speaker: self.inner.tts.active_device(),
            stt_ready: self.inner.stt.is_ready(),
            stt_model: self.inner.stt.model_name(),
            tts_status: self.inner.tts.status(),
            hardware_profile: cfg.hardware_profile.display_name().to_string(),
            llm: self.inner.llm.status().detail,
            response_mode: self.tools().modality.get(),
            voice_muted: self.tools().modality.voice_muted(),
            sleeping: lifecycle::is_sleeping(),
        }
    }

    /// Tells the HUD that JAI is going offline.
    pub fn announce_shutdown(&self) {
        self.emit(HudEvent::Shutdown);
    }

    fn devices_event() -> HudEvent {
        HudEvent::Devices {
            inputs: crate::ears::list_input_devices(),
            outputs: crate::tts::list_output_devices(),
        }
    }

    /// Everything a freshly opened HUD needs.
    pub fn snapshot_events(&self) -> Vec<HudEvent> {
        let hud = self.hud_state();
        vec![
            HudEvent::ChatHistory { lines: self.tools().memory.recent_turns(24) },
            HudEvent::ConfigState(self.config_state()),
            Self::devices_event(),
            HudEvent::Memories { items: self.tools().memory.list(200).unwrap_or_default(), query: None },
            HudEvent::Skills { items: self.tools().skills.list().unwrap_or_default() },
            HudEvent::SystemMetrics { telemetry: self.telemetry() },
            HudEvent::State { state: hud.state, detail: hud.detail, window_ms: self.inner.controls.window_remaining_ms() },
        ]
    }

    fn set_state(&self, state: &str, detail: impl Into<String>) {
        let detail = detail.into();
        {
            let mut hud = self.inner.hud.lock().unwrap_or_else(|e| e.into_inner());
            if hud.state == state && hud.detail == detail {
                return;
            }
            if hud.state != state {
                info!("[STATE] {} → {state} ({detail})", hud.state);
            }
            hud.state = state.to_string();
            hud.detail = detail.clone();
        }
        self.emit(HudEvent::State {
            state: state.to_string(),
            detail,
            window_ms: self.inner.controls.window_remaining_ms(),
        });
    }

    fn rest_state(&self) {
        if self.inner.controls.turn_active.load(Ordering::Acquire) {
            return;
        }
        let window = self.inner.controls.window_remaining_ms();
        if lifecycle::is_sleeping() {
            self.set_state("idle", "Sleeping — say \"JAI, wake up\"");
        } else if window > 0 {
            self.set_state("listening", "Conversation window open — speak without the wake word");
        } else if self.inner.controls.wake_word_required.load(Ordering::Relaxed) {
            self.set_state("idle", "Awaiting \"JAI\"");
        } else {
            self.set_state("idle", "Hands-free — listening for speech");
        }
    }

    // ---------------------------------------------------------------- event loops -------

    fn spawn_loops(&self, channels: Channels, mut brain_rx: mpsc::UnboundedReceiver<BrainEvent>) {
        let Channels { mut audio, tts: mut tts_rx } = channels;

        let app = self.clone();
        tokio::spawn(async move {
            while let Some(ev) = audio.recv().await {
                app.on_audio(ev);
            }
        });

        let app = self.clone();
        tokio::spawn(async move {
            while let Some(ev) = tts_rx.recv().await {
                app.on_tts(ev);
            }
        });

        let app = self.clone();
        tokio::spawn(async move {
            while let Some(ev) = brain_rx.recv().await {
                app.on_brain(ev);
            }
        });

        // Language engine status (start-up, model installation progress, failures).
        let app = self.clone();
        let mut llm = self.inner.llm.subscribe();
        tokio::spawn(async move {
            let mut phase = llm.borrow().phase;
            while llm.changed().await.is_ok() {
                let status = llm.borrow_and_update().clone();
                if status.phase != phase {
                    phase = status.phase;
                    match phase {
                        Phase::Ready => app.emit(HudEvent::Notice { message: format!("Language engine ready ({}).", status.detail) }),
                        Phase::Installing => app.emit(HudEvent::Notice { message: format!("Language engine: {}", status.detail) }),
                        Phase::Unavailable => app.emit(HudEvent::Error { message: format!("Language engine: {}", status.detail) }),
                        Phase::Starting => {}
                    }
                }
                app.emit(HudEvent::ConfigState(app.config_state()));
            }
        });

        let app = self.clone();
        let mut telemetry = self.tools().monitor.subscribe();
        tokio::spawn(async move {
            while telemetry.changed().await.is_ok() {
                let t = telemetry.borrow_and_update().clone();
                app.emit(HudEvent::SystemMetrics { telemetry: t });
            }
        });

        // Reply-mode changes (voice, chat, tool, HUD selector) reach the HUD immediately.
        let app = self.clone();
        let mut modes = self.tools().modality.subscribe();
        tokio::spawn(async move {
            while modes.changed().await.is_ok() {
                modes.borrow_and_update();
                app.emit(HudEvent::ConfigState(app.config_state()));
            }
        });

        // Finished timers and other widget alerts.
        let app = self.clone();
        let mut alerts = self.tools().widgets.subscribe_alerts();
        tokio::spawn(async move {
            loop {
                match alerts.recv().await {
                    Ok(alert) => {
                        info!("[WIDGET] Alert from '{}': {}", alert.widget_id, alert.message);
                        app.raise_alert(alert.message);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Due reminders.
        let app = self.clone();
        let mut due = self.tools().reminders.subscribe();
        tokio::spawn(async move {
            loop {
                match due.recv().await {
                    Ok(message) => app.raise_alert(message),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // The personal manager: watcher alerts are spoken like reminders; its activity lights the core.
        let app = self.clone();
        let mut pim_events = self.tools().pim.subscribe();
        tokio::spawn(async move {
            loop {
                match pim_events.recv().await {
                    Ok(crate::pim::PimEvent::Alert(message)) => app.raise_alert(message),
                    Ok(crate::pim::PimEvent::Pulse { path, alert }) => app.emit(HudEvent::CorePulse { path, alert }),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // The Stark systems: core pulses, alerts, arrival at home, voice macros.
        let app = self.clone();
        let mut stark = self.tools().stark.subscribe();
        tokio::spawn(async move {
            loop {
                match stark.recv().await {
                    Ok(crate::stark::StarkEvent::Pulse { path, alert }) => app.emit(HudEvent::CorePulse { path, alert }),
                    Ok(crate::stark::StarkEvent::Alert(message)) => app.raise_alert(message),
                    Ok(crate::stark::StarkEvent::Notice(message)) => app.emit(HudEvent::Notice { message }),
                    Ok(crate::stark::StarkEvent::Arrived { distance_m }) => app.arrival_protocol(distance_m).await,
                    Ok(crate::stark::StarkEvent::RunMacro { name, steps }) => app.run_macro(name, steps),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // The orders Sir put off ("play music in one minute", `later`): when one falls due it
        // is carried out as if he had just said it, one after the other.
        let app = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                // (An order Sir took from the ones on offer by a click: `did_you_mean::pick`.)
                for order in crate::did_you_mean::picked() {
                    info!("[MEANT] Carrying out the order Sir picked: '{order}'");
                    app.emit(HudEvent::Transcript { text: order.clone(), language: "text".into(), source: "chat".into(), wake_word: None, stt_ms: 0, audio_ms: 0 });
                    app.dispatch_turn(order, Instant::now(), 0, InputSource::TextGui);
                }
                for order in crate::later::fall_due(chrono::Local::now()) {
                    app.run_put_off(order).await;
                }
                // Games: after-game reminders, the school-night nudge, a download done; a game's
                // routine when it starts.
                for line in crate::kit_games::due_alerts() {
                    app.raise_alert(line);
                }
                for order in crate::kit_games::due_orders() {
                    app.run_put_off(order).await;
                }
            }
        });

        // WhatsApp message cards (prepared, sent, cancelled) for the HUD chat.
        let app = self.clone();
        let mut cards = self.tools().stark.whatsapp.subscribe_cards();
        tokio::spawn(async move {
            loop {
                match cards.recv().await {
                    Ok(c) => app.emit(HudEvent::MessageCard { id: c.id, recipient: c.recipient, phone: c.phone, text: c.text, status: c.status }),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // The Guardian: every 10 s JAI looks for overheating, full VRAM / memory, a pegged CPU
        // or a nearly full drive, names the culprit and offers help ("yes" accepts the offer).
        let app = self.clone();
        tokio::spawn(async move {
            let mut guardian = crate::guardian::Guardian::new();
            let mut tick = tokio::time::interval(crate::guardian::CHECK_INTERVAL);
            loop {
                tick.tick().await;
                if !app.inner.config.read(|c| c.proactive_alerts) {
                    continue;
                }
                let t = app.telemetry();
                let mut due = guardian.check(&t, Instant::now());
                if due.iter().any(|a| a.kind == crate::guardian::Kind::VramFull) && crate::guardian::vram_is_ours(&t, app.llm_vram_mb().await) {
                    info!("[GUARDIAN] VRAM is full with my own language model; not an anomaly.");
                    due.retain(|a| a.kind != crate::guardian::Kind::VramFull);
                }
                if due.is_empty() {
                    continue;
                }
                let (processes, gpu) = if crate::guardian::wants_processes(&due) {
                    tokio::task::spawn_blocking(|| (crate::diagnostics::top_processes(), crate::guardian::gpu_process_names())).await.unwrap_or_default()
                } else {
                    (Vec::new(), Vec::new())
                };
                for anomaly in due {
                    let culprit = crate::guardian::culprit(&anomaly.kind, &processes, &gpu);
                    let (message, offer) = crate::guardian::compose(&anomaly, culprit.as_deref(), &t);
                    info!("[GUARDIAN] {:?} at {:.1}: {message}", anomaly.kind, anomaly.value);
                    // The same offer as a card: "yes" and "terminate it" are its buttons. A
                    // telemetry alert comes with its two pips.
                    crate::tts::play_cue(crate::jai_voice_dsp::Earcon::Alert);
                    crate::proactive_butler::show(&crate::proactive_butler::hardware(&anomaly, &message, &offer));
                    app.tools().focus.note_offer(crate::focus::PendingOffer {
                        tool: offer.primary.0.to_string(),
                        args: offer.primary.1,
                        terminate: offer.terminate,
                    });
                    app.emit(HudEvent::Guardian { message: message.clone() });
                    app.raise_alert(message);
                }
            }
        });

        // The butler: what starts within the quarter of an hour, a Minecraft crash, a failed
        // build — each as a card with its fix one click away.
        crate::proactive_butler::start(self.clone());

        // Conversation-window expiry and periodic state refresh.
        let app = self.clone();
        tokio::spawn(async move {
            let mut was_open = false;
            // (The switches as they stood at the last look: one that goes on while the privacy
            // lock is on is Sir's explicit word that he wants to see what is heard.)
            let mut showed = (true, true);
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            loop {
                tick.tick().await;
                let open = app.inner.controls.window_remaining_ms() > 0;
                if was_open && !open {
                    info!("[STATE] Conversation window closed");
                    app.rest_state();
                }
                was_open = open;
                // "Don't listen for ten minutes", "until eight": the time is up.
                if crate::listening_lock::take_lapsed() {
                    app.listening_again();
                }
                let shows = (app.inner.config.read(|c| c.interface.live_words), crate::hud_feel::subtitles().is_some());
                if (shows.0 && !showed.0) || (shows.1 && !showed.1) {
                    crate::listening_lock::show_asked();
                }
                showed = shows;
            }
        });
    }

    /// A lock with an end has run out by itself ("don't listen for ten minutes", "until eight"):
    /// he listens again and says so, as a notice on the HUD and in a short line of his own (once
    /// he is free to speak; never over an answer). When something else still keeps him from
    /// hearing, the notice says that instead and he claims nothing.
    fn listening_again(&self) {
        info!("[PRIVACY] The time is up: the lock is lifted");
        self.emit(HudEvent::ConfigState(self.config_state()));
        crate::gui::refresh();
        let kept_from_hearing = if !self.inner.controls.mic_enabled.load(Ordering::Acquire) {
            Some("the microphone is switched off")
        } else if lifecycle::is_sleeping() {
            Some("I am asleep until you wake me")
        } else if !crate::vram_guard::voice_wake_allowed() {
            Some("I am in standby")
        } else {
            None
        };
        if let Some(why) = kept_from_hearing {
            self.notice(format!("The time you gave me is up, Sir, but {why}."));
            return;
        }
        let line = crate::listening_lock::reply(crate::listening_lock::LockCommand::Resume);
        self.notice(line.clone());
        let app = self.clone();
        tokio::spawn(async move {
            // (As an alert waits: an answer under way is never cut short.)
            let deadline = Instant::now() + Duration::from_secs(45);
            while app.busy() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            // (Told not to listen again in the meantime: then he does not say that he listens.)
            if !crate::listening_lock::is_locked() && !app.busy() {
                app.speak_standalone(&line, false, Trigger::System);
            }
        });
    }

    fn on_audio(&self, ev: AudioEvent) {
        match ev {
            AudioEvent::Level(level) => self.emit(HudEvent::AudioLevel(level)),
            AudioEvent::Calibrated { noise_floor } => {
                self.emit(HudEvent::VadEvent {
                    event: "calibrated".into(),
                    detail: format!("noise floor {:.1} dBFS", crate::dsp::to_dbfs(noise_floor)),
                });
                self.emit(HudEvent::ConfigState(self.config_state()));
                self.rest_state();
            }
            AudioEvent::DeviceActive { name, sample_rate, channels } => {
                self.emit(HudEvent::Notice { message: format!("Microphone: {name} ({sample_rate} Hz, {channels} ch)") });
                self.emit(HudEvent::ConfigState(self.config_state()));
            }
            AudioEvent::SpeechStarted { barge_in, ptt } => {
                // (Whisper is his from here: a video's clip being written along is dropped.)
                crate::live_subtitles::sir_speaks();
                crate::core_sphere_3d::CoreActivityBus::chain(&["ears.rs", "vad.rs"]);
                self.inner.speech_epoch.fetch_add(1, Ordering::AcqRel);
                // Sir is speaking: the desktop and the media session are read now, so the turn
                // that follows finds them in memory.
                crate::hyper_cortex::prefetch_context();
                // Sir is still finishing his thought: pull back the turn his first words started.
                let withdrawn = self.withdraw_unanswered_turn();
                if barge_in && !withdrawn {
                    info!("[TURN] Barge-in: user speaking over JAI");
                    self.interrupt(if ptt { "push-to-talk" } else { "barge-in" });
                    self.emit(HudEvent::VadEvent { event: "barge_in".into(), detail: String::new() });
                }
                self.emit(HudEvent::VadEvent { event: "listening_active".into(), detail: if ptt { "ptt".into() } else { String::new() } });
                self.set_state("listening", if ptt { "Push-to-talk — listening" } else { "listening_active" });
            }
            AudioEvent::Paused { pcm, speech_ms } => {
                self.call_off_live_words();
                self.start_head_start(pcm, speech_ms)
            }
            AudioEvent::SoFar { pcm, speech_ms } => self.live_words_ahead(pcm, speech_ms),
            AudioEvent::Resumed => self.call_off_head_start(),
            AudioEvent::Discarded { speech_ms } => {
                self.call_off_head_start();
                self.call_off_live_words();
                crate::hud_lines::speech_over();
                self.emit(HudEvent::VadEvent { event: "discarded".into(), detail: format!("{speech_ms} ms") });
                self.rest_state();
            }
            AudioEvent::Utterance(u) => {
                // (The sentence itself must never wait behind a guess at half of it.)
                self.call_off_live_words();
                let why = match u.reason {
                    EndReason::Silence => format!("{} ms silence", u.silence_ms),
                    EndReason::MaxLength => "length cap".to_string(),
                    EndReason::PttReleased => "push-to-talk released".to_string(),
                };
                self.emit(HudEvent::VadEvent {
                    event: "speech_ended".into(),
                    detail: format!("{} ms voiced, {why}", u.speech_ms),
                });
                if !self.inner.stt.is_ready() {
                    warn!("[STT] Speech captured but Whisper is not ready yet.");
                    self.rest_state();
                    return;
                }
                self.set_state("thinking", "Transcribing…");
                let epoch = self.inner.speech_epoch.load(Ordering::Acquire);
                self.inner.stt_pending.fetch_add(1, Ordering::AcqRel);
                let app = self.clone();
                tokio::spawn(async move {
                    app.process_utterance(u, epoch).await;
                    app.inner.stt_pending.fetch_sub(1, Ordering::AcqRel);
                });
            }
            AudioEvent::MicTest { pcm, peak, rms, mic } => {
                let app = self.clone();
                tokio::spawn(async move { app.finish_mic_test(pcm, peak, rms, mic).await });
            }
            AudioEvent::Acoustic(ev) => {
                let app = self.clone();
                tokio::spawn(async move { app.on_acoustic(ev).await });
            }
        }
    }

    /// A pause that may be the end of the utterance: Whisper starts on the words so far, and what
    /// they ask for (when they carry JAI's name) is fetched ahead. Nothing is carried out here.
    fn start_head_start(&self, pcm: Vec<f32>, speech_ms: u32) {
        self.call_off_head_start();
        // Not while Whisper sleeps, and not for a piece that will be heard together with audio
        // held from before (the transcript would be of the wrong clip).
        let held = self.inner.carry_audio.lock().unwrap_or_else(|e| e.into_inner()).is_some();
        if held || !self.inner.stt.is_ready() || !crate::vram_guard::voice_wake_allowed() {
            return;
        }
        let samples = pcm.len();
        let Some((called_off, reply)) = self.inner.stt.transcribe_ahead(pcm, speech_ms) else { return };
        let (ready, transcript) = tokio::sync::watch::channel(None);
        *self.inner.head_start.lock().unwrap_or_else(|e| e.into_inner()) = Some(HeadStart { samples, speech_ms, at: Instant::now(), called_off, transcript });
        let app = self.clone();
        tokio::spawn(async move {
            let result = reply.await.unwrap_or_else(|_| Err("speech recognition worker stopped".to_string()));
            if let Ok(t) = &result {
                if app.live_words_wanted() {
                    crate::hud_lines::heard(&t.text);
                }
                let wake = app.inner.wake.read().unwrap_or_else(|e| e.into_inner()).evaluate(&t.text);
                if wake.matched && !wake.command.trim().is_empty() && !crate::listening_lock::is_locked() && !lifecycle::is_sleeping() {
                    crate::hyper_cortex::anticipate(app.tools(), &wake.command);
                }
            }
            let _ = ready.send(Some(result));
        });
    }

    /// Whether Sir's words are shown while he speaks: the setting is on and no game is in front
    /// (there the extra decodes would cost frames). Not while he is told not to listen: what
    /// the microphone hears then is neither decoded for the screen nor put on it (2026-11-13:
    /// after "don't listen" the song in the room went on appearing under the emblem), unless
    /// Sir asks to see it while the lock is on.
    fn live_words_wanted(&self) -> bool {
        self.inner.config.read(|c| c.interface.live_words) && crate::listening_lock::may_show_heard() && !crate::hud_feel::game_in_front() && !crate::platform::in_test_session()
    }

    /// The words so far, while Sir is still speaking: transcribed for the HUD's live line when
    /// Whisper has nothing else to do. Called off the moment the utterance pauses or ends.
    fn live_words_ahead(&self, pcm: Vec<f32>, speech_ms: u32) {
        // In standby his name in the words so far starts loading the model before he has finished.
        let warm_up = crate::vram_guard::is_dormant() && !crate::vram_guard::in_game();
        if !(warm_up || self.live_words_wanted()) || !self.inner.stt.is_ready() || !crate::vram_guard::voice_wake_allowed() {
            return;
        }
        // One decode at a time, and never beside a head start.
        if self.inner.head_start.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            return;
        }
        let mut running = self.inner.live_decode.lock().unwrap_or_else(|e| e.into_inner());
        if running.is_some() {
            return;
        }
        let Some((called_off, reply)) = self.inner.stt.transcribe_ahead(pcm, speech_ms) else { return };
        *running = Some(Arc::clone(&called_off));
        drop(running);
        let app = self.clone();
        tokio::spawn(async move {
            let result = reply.await;
            let mut running = app.inner.live_decode.lock().unwrap_or_else(|e| e.into_inner());
            if running.as_ref().is_some_and(|flag| Arc::ptr_eq(flag, &called_off)) {
                *running = None;
            }
            drop(running);
            if let (Ok(Ok(t)), false) = (result, called_off.load(Ordering::Acquire)) {
                if warm_up && crate::vram_guard::is_dormant() && app.wake_command(&t.text).is_some() {
                    crate::vram_guard::wake("his name, mid-sentence");
                }
                if app.live_words_wanted() {
                    crate::hud_lines::heard(&t.text);
                }
            }
        });
    }

    /// The utterance paused or ended: a guess at the words so far that is still being decoded
    /// stops, so Whisper is free for the sentence.
    fn call_off_live_words(&self) {
        if let Some(flag) = self.inner.live_decode.lock().unwrap_or_else(|e| e.into_inner()).take() {
            flag.store(true, Ordering::Release);
        }
    }

    /// The ear's gate follows his voice. How far each of his sentences stood over the room is
    /// kept for the last eight; every fourth sentence the gate is read from their middle and
    /// moved one step: opened when his voice arrives quietly (a tired evening, the microphone
    /// further away), narrowed when it arrives strong. The set-up does this once; this keeps it
    /// true.
    fn gate_follows(&self, (peak_db, floor_db): (f32, f32)) {
        static OVER: Mutex<(std::collections::VecDeque<f32>, u32)> = Mutex::new((std::collections::VecDeque::new(), 0));
        let over = peak_db - floor_db;
        if !over.is_finite() || !(0.0..=90.0).contains(&over) {
            return;
        }
        let kept: Vec<f32> = {
            let mut guard = OVER.lock().unwrap_or_else(|e| e.into_inner());
            guard.0.push_back(over);
            while guard.0.len() > crate::voice_setup::GATE_SENTENCES {
                guard.0.pop_front();
            }
            guard.1 += 1;
            if guard.1 % 4 != 0 {
                return;
            }
            guard.0.iter().copied().collect()
        };
        let current = self.inner.config.read(|c| c.vad_noise_multiplier);
        if let Some(gate) = crate::voice_setup::gate_step(&kept, current) {
            self.inner.config.update(|c| c.vad_noise_multiplier = gate);
            self.inner.controls.set_vad(None, None, None, Some(gate));
            info!("[EAR] The gate follows his voice: {current:.1} -> {gate:.1} (the middle of his last {} sentences stands {:.0} dB over the room)", kept.len(), {
                let mut sorted = kept.clone();
                sorted.sort_by(|a, b| a.total_cmp(b));
                sorted[sorted.len() / 2]
            });
        }
    }

    /// An order given by name while no print of Sir's voice is known: its voice is gathered,
    /// and once enough of them are one voice, that is his print (the lock holds from then on).
    fn gather_voice(&self, db: &str, clip: Vec<f32>, speech_ms: u32) {
        if let Some(print) = crate::speaker_id::gather(db, clip, speech_ms, true) {
            crate::hud_feel::set_voice_known(true);
            info!("[VOICE] Sir's voice print was gathered from {} orders given by name (a voice is his from a likeness of {:.2})", print.clips, print.threshold);
            crate::hud_notify::record("I know your voice now, Sir, from the orders you gave me. I answer to you alone; say \u{201C}listen to everyone\u{201D} to change that.", false);
        }
    }

    /// The voice set-up's six lines are read: what JAI learns from them. The spellings the
    /// recogniser wrote for his name join the names he answers to, the ear's gate is set to
    /// Sir's voice over his room, and the clips' mean becomes his voice print.
    fn learn_from_setup(&self, prints: Vec<Vec<f32>>, heard: Vec<String>, levels: Vec<(f32, f32)>) {
        let mut outcome = crate::voice_setup::Outcome::default();
        let spellings = {
            let wake = self.inner.wake.read().unwrap_or_else(|e| e.into_inner());
            crate::voice_setup::learnt_spellings(&heard, |word| wake.evaluate(&format!("{word}, open the mixer")).matched)
        };
        if !spellings.is_empty() {
            self.inner.config.update(|c| {
                for s in &spellings {
                    if !c.wake_words.iter().any(|w| w.eq_ignore_ascii_case(s)) {
                        c.wake_words.push(s.clone());
                    }
                }
            });
            *self.inner.wake.write().unwrap_or_else(|e| e.into_inner()) = self.inner.config.read(|c| WakeWordMatcher::new(&c.wake_words, c.wake_word_similarity_threshold));
        }
        outcome.spellings = spellings;
        // The everyday words his name is written as in his voice ("They ...", "Day ..."): soft
        // names from now on.
        let soft = crate::voice_setup::soft_names(&heard);
        if !soft.is_empty() {
            self.inner.config.update(|c| {
                for s in &soft {
                    let entry = format!("~{s}");
                    if !c.wake_words.iter().any(|w| w.eq_ignore_ascii_case(&entry)) {
                        c.wake_words.push(entry);
                    }
                }
            });
            *self.inner.wake.write().unwrap_or_else(|e| e.into_inner()) = self.inner.config.read(|c| WakeWordMatcher::new(&c.wake_words, c.wake_word_similarity_threshold));
        }
        outcome.soft = soft;
        // The gate: by how far his voice stands over the room in the middle line of the six.
        let mut over: Vec<f32> = levels.iter().map(|(peak, floor)| peak - floor).filter(|d| d.is_finite()).collect();
        over.sort_by(f32::total_cmp);
        let current = self.inner.config.read(|c| c.vad_noise_multiplier);
        if let Some(gate) = over.get(over.len() / 2).and_then(|over| crate::voice_setup::gate_for(*over, current)) {
            self.inner.config.update(|c| c.vad_noise_multiplier = gate);
            self.inner.controls.set_vad(None, None, None, Some(gate));
            outcome.gate = Some((current, gate));
        }
        let db = self.inner.config.read(|c| c.memory_db_path.clone());
        match crate::speaker_id::enrol(&prints) {
            Some(print) => match crate::speaker_id::keep(&db, &print) {
                Ok(()) => {
                    outcome.print = true;
                    crate::hud_feel::set_voice_known(true);
                    info!("[VOICE] Sir's voice print is kept ({} clips; a voice is his from a likeness of {:.2})", print.clips, print.threshold);
                }
                Err(e) => outcome.no_print = Some(e),
            },
            None => outcome.no_print = Some(if crate::speaker_id::available() { "too few lines came through clearly".to_string() } else { "the voice model is not installed".to_string() }),
        }
        outcome.lock = self.inner.config.read(|c| c.interface.voice_lock);
        info!("[VOICE] Set-up done: spellings {:?}, soft names {:?}, gate {:?}, print {}", outcome.spellings, outcome.soft, outcome.gate, outcome.print);
        let line = outcome.spoken();
        crate::voice_setup::finished(outcome);
        self.speak_standalone(&line, true, Trigger::Input(InputSource::Voice));
    }

    /// A clip of Sir's voice recorded elsewhere (the phone's hold-to-talk; 16 kHz): its words,
    /// as he hears them at the PC.
    pub async fn hear_clip(&self, pcm: Vec<f32>) -> Result<String, String> {
        let ms = (pcm.len() / 16) as u32;
        let heard = self.inner.stt.transcribe(pcm, ms).await?;
        Ok(crate::wake::canon(&heard.text))
    }

    /// A clip of what the PC plays, for the live subtitles (`live_subtitles`): the flag that
    /// calls it off and the words to come. None while Whisper sleeps or is not loaded.
    pub fn subtitle_ahead(&self, pcm: Vec<f32>, translate: bool) -> Option<(Arc<AtomicBool>, tokio::sync::oneshot::Receiver<Result<crate::stt::Transcript, String>>)> {
        if !self.inner.stt.is_ready() || !crate::vram_guard::voice_wake_allowed() {
            return None;
        }
        self.inner.stt.subtitle(pcm, translate)
    }

    /// Sir carried on (or the sound was nothing): the head start is void and Whisper stops.
    fn call_off_head_start(&self) {
        if let Some(head) = self.inner.head_start.lock().unwrap_or_else(|e| e.into_inner()).take() {
            head.called_off.store(true, Ordering::Release);
        }
    }

    /// The head start for exactly this audio, if Whisper got one.
    fn take_head_start(&self, samples: usize, speech_ms: u32) -> Option<HeadStart> {
        let head = self.inner.head_start.lock().unwrap_or_else(|e| e.into_inner()).take()?;
        if head.samples == samples && head.speech_ms == speech_ms {
            return Some(head);
        }
        head.called_off.store(true, Ordering::Release);
        None
    }

    /// Live acoustic-radar settings (the Quick Connections tab).
    pub fn set_acoustic(&self, a: &crate::config::AcousticSettings) {
        self.inner.controls.set_acoustic(a);
    }

    /// A double clap or a knock on the door.
    async fn on_acoustic(&self, ev: crate::acoustic_radar::AcousticEvent) {
        use crate::acoustic_radar::AcousticEvent;
        let settings = self.inner.config.read(|c| c.acoustic.clone());
        let stark = &self.tools().stark;
        match ev {
            AcousticEvent::DoubleClap => {
                stark.pulse(&["acoustic", "ears", "brain"], false);
                if self.busy() || !self.inner.controls.mic_enabled.load(Ordering::Acquire) {
                    return;
                }
                if settings.clap_action == "toggle_music" {
                    let playing = crate::music::now_playing().await.is_some_and(|n| n.playing);
                    info!("[RADAR] Double clap → music {}", if playing { "pause" } else { "resume" });
                    self.run_ui_tool("control_music", json!({ "action": if playing { "pause" } else { "resume" } })).await;
                    return;
                }
                info!("[RADAR] Double clap → listening");
                if lifecycle::set_sleeping(false) {
                    self.emit(HudEvent::ConfigState(self.config_state()));
                }
                self.wake_listening("Double clap");
            }
            AcousticEvent::Knock { count } => {
                let music = crate::music::now_playing().await.filter(|n| n.playing);
                if settings.knock_only_with_music && music.is_none() {
                    info!("[RADAR] {count} knocks heard, but no music is playing; staying quiet.");
                    return;
                }
                info!("[RADAR] {count} knocks while Sir has music on: alerting");
                stark.pulse(&["acoustic", "guardian", "voice"], true);
                let message = "Pardon me, Sir, it sounds like someone just knocked on your door.".to_string();
                self.emit(HudEvent::RoomAlert { message: message.clone() });
                let restore = duck_audio(&self.tools().voice_volume).await;
                self.raise_alert(message);
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(9)).await;
                    restore.await;
                });
            }
        }
    }

    /// The home geofence fired: wake the HUD, start the playlist / protocol, greet Sir.
    async fn arrival_protocol(&self, distance_m: f64) {
        let cfg = self.inner.config.read(|c| c.location.clone());
        info!("[GEOFENCE] Arrival protocol ({distance_m:.0} m from home)");
        self.tools().stark.pulse(&["gps", "commute", "brain"], false);
        if cfg.arrival_wake_hud {
            crate::gui::show();
        }
        if !cfg.arrival_protocol.trim().is_empty() {
            self.run_ui_tool("manage_stark_protocol", json!({ "action": "run", "protocol_name": cfg.arrival_protocol })).await;
        }
        if !cfg.arrival_playlist.trim().is_empty() {
            self.run_ui_tool("control_music", json!({ "action": "play", "playlist_name": cfg.arrival_playlist })).await;
        }
        if cfg.arrival_greeting {
            use chrono::Timelike;
            let greeting = match chrono::Local::now().hour() {
                5..=11 => "Welcome home, Sir. Good morning.",
                12..=17 => "Welcome home, Sir.",
                _ => "Welcome home, Sir. Good evening.",
            };
            self.raise_alert(greeting.to_string());
        }
    }

    /// Runs a voice macro's steps as separate requests, each after the previous answer.
    fn run_macro(&self, name: String, steps: Vec<String>) {
        if self.inner.macro_running.swap(true, Ordering::AcqRel) {
            warn!("[MACRO] '{name}' asked to run while another macro is running; ignored.");
            return;
        }
        let source = *self.inner.last_source.lock().unwrap_or_else(|e| e.into_inner());
        let app = self.clone();
        tokio::spawn(async move {
            info!("[MACRO] Running '{name}' ({} steps)", steps.len());
            app.wait_idle(Duration::from_secs(30)).await;
            for (i, step) in steps.iter().enumerate() {
                info!("[MACRO] '{name}' step {}/{}: {step}", i + 1, steps.len());
                app.emit(HudEvent::Transcript { text: step.clone(), language: "macro".into(), source: format!("macro {name}"), wake_word: None, stt_ms: 0, audio_ms: 0 });
                app.dispatch_turn(step.clone(), Instant::now(), 0, source);
                app.wait_idle(Duration::from_secs(90)).await;
            }
            app.inner.macro_running.store(false, Ordering::Release);
            info!("[MACRO] '{name}' finished");
        });
    }

    /// An order Sir put off has fallen due (`later`): it runs as if he had just said it, once
    /// he and whatever is running are done. One that is carried out without a word ("pause",
    /// "close the board") gets a short word first, so that what happens is no surprise.
    async fn run_put_off(&self, order: String) {
        // (A scripted session carries it out only where every tool that reaches outside
        // J.A.I. is noted and left undone; any other test session leaves the PC alone.)
        if !crate::later::may_run_here() {
            info!("[LATER] '{order}' fell due: noted, not carried out (a test session leaves the PC alone)");
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(120);
        while (self.busy() || self.inner.macro_running.load(Ordering::Acquire)) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        info!("[LATER] Due now: '{order}'");
        crate::hud_notify::record(&format!("As planned: {order}"), false);
        if crate::later::runs_without_a_word(&order) {
            self.speak_standalone("As planned, Sir.", false, Trigger::System);
            self.wait_idle(Duration::from_secs(10)).await;
        }
        let source = *self.inner.last_source.lock().unwrap_or_else(|e| e.into_inner());
        self.emit(HudEvent::Transcript { text: order.clone(), language: "later".into(), source: "planned".into(), wake_word: None, stt_ms: 0, audio_ms: 0 });
        self.dispatch_turn(order, Instant::now(), 0, source);
        self.wait_idle(Duration::from_secs(90)).await;
    }

    /// Waits until the running turn (and its audio) is over, at most `limit`.
    async fn wait_idle(&self, limit: Duration) {
        let deadline = Instant::now() + limit;
        tokio::time::sleep(Duration::from_millis(400)).await;
        while self.busy() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Called when Sir starts speaking: a voice turn that has not acted or made a sound yet is
    /// withdrawn and its words are kept for the stitched request. True when one was withdrawn.
    fn withdraw_unanswered_turn(&self) -> bool {
        if !self.inner.config.read(|c| c.utterance_stitching) {
            return false;
        }
        let withdrawn = {
            let mut turn = self.turn_lock();
            let stitchable = turn.as_ref().is_some_and(|t| {
                t.source == Some(InputSource::Voice) && t.request.is_some() && t.started.elapsed() < STITCH_TURN_MAX_AGE
            });
            if stitchable && turn.as_ref().is_some_and(|t| t.link.retract()) {
                turn.take()
            } else {
                None
            }
        };
        let Some(t) = withdrawn else { return false };
        t.cancel.cancel();
        self.inner.tts.stop();
        self.inner.controls.turn_active.store(false, Ordering::Release);
        let text = t.request.unwrap_or_default();
        match t.audio.filter(|_| t.started.elapsed() < STITCH_AUDIO_MAX_AGE) {
            Some((pcm, speech_ms)) => {
                info!("[STITCH] Sir kept talking; '{text}' is heard again with the rest of the sentence");
                // (It had become a turn: it was his to hear.)
                self.keep_audio_for_stitching(pcm, speech_ms, false, false, true);
            }
            None => {
                info!("[STITCH] Sir kept talking; holding '{text}' for the rest of the sentence");
                self.keep_for_stitching(text);
            }
        }
        self.emit(HudEvent::Retracted { message_id: t.id });
        true
    }

    /// Holds the audio of an unfinished sentence; the next utterance is transcribed together with
    /// it. If nothing follows (the new sound was a cough), it is heard on its own. `addressed`:
    /// the piece was spoken to him (`held_for_him`).
    fn keep_audio_for_stitching(&self, pcm: Vec<f32>, speech_ms: u32, barge_in: bool, ptt: bool, addressed: bool) {
        let id = self.inner.carry_seq.fetch_add(1, Ordering::AcqRel) + 1;
        {
            let mut carry = self.inner.carry_audio.lock().unwrap_or_else(|e| e.into_inner());
            // Two pauses in one sentence: all of it, in order (and his to hear if a part was).
            let (pcm, speech_ms, addressed) = match carry.take() {
                Some(previous) if previous.at.elapsed() < CARRY_MAX_AGE => (join_audio(previous.pcm, pcm), previous.speech_ms + speech_ms, addressed || previous.addressed),
                _ => (pcm, speech_ms, addressed),
            };
            *carry = Some(CarryAudio { id, pcm, speech_ms, barge_in, ptt, addressed, at: Instant::now() });
        }
        let app = self.clone();
        tokio::spawn(async move {
            let quiet_since = Instant::now();
            let mut quiet = Duration::ZERO;
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                let busy = app.inner.controls.in_speech.load(Ordering::Acquire) || app.inner.stt_pending.load(Ordering::Acquire) > 0;
                quiet = if busy { Duration::ZERO } else { quiet + Duration::from_millis(200) };
                let still_mine = app.inner.carry_audio.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|c| c.id == id);
                if !still_mine {
                    return;
                }
                if quiet >= Duration::from_millis(1200) || quiet_since.elapsed() > CARRY_MAX_AGE {
                    break;
                }
            }
            let carried = app.inner.carry_audio.lock().unwrap_or_else(|e| e.into_inner()).take_if(|c| c.id == id);
            if let Some(c) = carried {
                info!("[STITCH] Nothing followed; hearing {} ms of speech on its own", c.speech_ms);
                let u = Utterance {
                    pcm: c.pcm,
                    speech_ms: c.speech_ms,
                    silence_ms: 0,
                    reason: EndReason::Silence,
                    barge_in: c.barge_in,
                    ptt: c.ptt,
                    // (In a conversation only if it was his to hear: a piece that was held
                    // before anything was known of it is judged like any other sentence.)
                    in_window: c.addressed,
                    ended_at: c.at,
                };
                let epoch = app.inner.speech_epoch.load(Ordering::Acquire);
                app.inner.stt_pending.fetch_add(1, Ordering::AcqRel);
                app.process_utterance(u, epoch).await;
                app.inner.stt_pending.fetch_sub(1, Ordering::AcqRel);
            }
        });
    }

    fn take_carry_audio(&self) -> Option<CarryAudio> {
        self.inner.carry_audio.lock().unwrap_or_else(|e| e.into_inner()).take().filter(|c| c.at.elapsed() < CARRY_MAX_AGE)
    }

    /// Queues unfinished words; the next transcript is appended to them. If nothing follows
    /// (the new sound was a cough), they are answered on their own.
    fn keep_for_stitching(&self, text: String) {
        let id = self.inner.carry_seq.fetch_add(1, Ordering::AcqRel) + 1;
        {
            let mut carry = self.inner.carry.lock().unwrap_or_else(|e| e.into_inner());
            let text = match carry.take() {
                Some(previous) if previous.at.elapsed() < CARRY_MAX_AGE => format!("{} {text}", previous.text),
                _ => text,
            };
            *carry = Some(Carry { id, text, at: Instant::now() });
        }
        let app = self.clone();
        tokio::spawn(async move {
            let quiet_since = Instant::now();
            let mut quiet = Duration::ZERO;
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                let busy = app.inner.controls.in_speech.load(Ordering::Acquire) || app.inner.stt_pending.load(Ordering::Acquire) > 0;
                quiet = if busy { Duration::ZERO } else { quiet + Duration::from_millis(200) };
                let still_mine = app.inner.carry.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|c| c.id == id);
                if !still_mine {
                    return;
                }
                if quiet >= Duration::from_millis(1200) || quiet_since.elapsed() > CARRY_MAX_AGE {
                    break;
                }
            }
            let carried = app.inner.carry.lock().unwrap_or_else(|e| e.into_inner()).take_if(|c| c.id == id);
            if let Some(c) = carried {
                info!("[STITCH] Nothing followed; answering '{}' on its own", c.text);
                app.dispatch_turn(c.text, Instant::now(), 0, InputSource::Voice);
            }
        });
    }

    fn take_carry(&self) -> Option<String> {
        self.inner
            .carry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .filter(|c| c.at.elapsed() < CARRY_MAX_AGE)
            .map(|c| c.text)
    }

    fn has_carry(&self) -> bool {
        self.inner.carry.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|c| c.at.elapsed() < CARRY_MAX_AGE)
    }

    async fn process_utterance(&self, u: Utterance, epoch: u64) {
        // One utterance at a time decides about stitching: a sentence's second half waits until
        // its first half has been set aside.
        let _order = self.inner.utterance_order.lock().await;
        let stitching_enabled = self.inner.config.read(|c| c.utterance_stitching);
        let superseded = || self.inner.speech_epoch.load(Ordering::Acquire) != epoch;
        // The first half of a sentence Sir paused in: Whisper hears both halves together.
        // (`joined`: Some(whether the held piece was spoken to him) for a stitched sentence.)
        let (pcm, speech_ms, joined) = match self.take_carry_audio() {
            Some(previous) => {
                info!("[STITCH] Hearing {} ms + {} ms of speech as one sentence", previous.speech_ms, u.speech_ms);
                (join_audio(previous.pcm, u.pcm), previous.speech_ms + u.speech_ms, Some(previous.addressed))
            }
            None => (u.pcm, u.speech_ms, None),
        };
        // The head start Whisper got at the pause (void for audio joined with an earlier piece).
        let ahead = if joined.is_some() {
            self.call_off_head_start();
            None
        } else {
            self.take_head_start(pcm.len(), speech_ms)
        };
        // What this audio is when it is held before anything is known of its words.
        let unjudged = held_for_him(false, joined, u.in_window, u.ptt, u.barge_in);
        // Sir is already speaking again: the rest of the sentence follows, so this part is not
        // transcribed on its own.
        if stitching_enabled && superseded() {
            if let Some(head) = &ahead {
                head.called_off.store(true, Ordering::Release);
            }
            info!("[STITCH] Sir carried on after {speech_ms} ms of speech; holding the audio for the rest");
            self.keep_audio_for_stitching(pcm, speech_ms, u.barge_in, u.ptt, unjudged);
            return;
        }
        // Zero-VRAM standby with voice wake off: only the hotkey, the standby button or push-to-talk wake JAI.
        if !u.ptt && !crate::vram_guard::voice_wake_allowed() {
            tracing::debug!("[VRAM] Standby: {speech_ms} ms of speech not transcribed (voice wake is off).");
            self.rest_state();
            return;
        }
        let kept = stitching_enabled.then(|| pcm.clone());
        // Whose voice it is (for the voice lock and the voice set-up): worked out on the
        // processor while Whisper hears the words.
        // (A scripted session speaks with stand-in voices and learns nothing that is kept: the
        // lock is the affair of Sir's own sessions. Nor does it hold while guests are welcome.)
        let scripted = crate::platform::in_test_session();
        let lock_on = !u.ptt && !scripted && self.inner.config.read(|c| c.interface.voice_lock) && !crate::speaker_id::guests_welcome();
        let in_setup = crate::voice_setup::active();
        let voice_levels = (crate::voice_setup::peak_db(&pcm), crate::dsp::to_dbfs(self.inner.controls.noise_floor_now()));
        // (Lock or no lock: his print learns from every sentence that is his.)
        let voice_print = (crate::speaker_id::available() && (in_setup || (!u.ptt && !scripted))).then(|| {
            let clip = pcm.clone();
            tokio::task::spawn_blocking(move || crate::speaker_id::print_of(&clip))
        });
        // The clip once more, for a second and careful hearing should its words lead nowhere,
        // and to be kept with what it was taken for (`voice_clips`).
        let again_audio = (self.inner.config.read(|c| c.whisper_second_hearing) && pcm.len() <= SECOND_HEARING_MOST).then(|| pcm.clone());
        let clip_audio = (!scripted && pcm.len() <= SECOND_HEARING_MOST && self.inner.config.read(|c| c.keep_voice_clips)).then(|| pcm.clone());
        crate::core_sphere_3d::pulse("stt.rs");
        // With a head start the transcript is already on its way (or here): only the rest of the
        // decode is waited for.
        let waiting = Instant::now();
        let heard_ahead = match ahead {
            Some(mut head) => {
                let done = head.transcript.wait_for(|t| t.is_some()).await.ok().and_then(|t| (*t).clone());
                match done {
                    Some(Ok(mut t)) => {
                        let waited = waiting.elapsed().as_millis() as u64;
                        info!(
                            "[STT] Head start: Whisper began {} ms before the end of speech was certain; the transcript was there {waited} ms after it (the decode took {} ms)",
                            u.ended_at.saturating_duration_since(head.at).as_millis(),
                            t.stt_ms
                        );
                        // What the turn waited for, not what the decode cost.
                        t.stt_ms = waited;
                        Some(t)
                    }
                    _ => None,
                }
            }
            None => None,
        };
        let mut transcript = match heard_ahead {
            Some(t) => t,
            None => match self.inner.stt.transcribe(pcm, speech_ms).await {
                Ok(t) => t,
                Err(e) => {
                    warn!("[STT] {e}");
                    self.emit(HudEvent::Error { message: format!("Speech recognition failed: {e}") });
                    self.rest_state();
                    return;
                }
            },
        };
        // His name as the fixed phrases know it: "Jay, wake up" / "wake up, J.A.I." → "JAI, wake up".
        transcript.text = crate::wake::canon(&transcript.text);
        if transcript.text.is_empty() {
            // A first piece too short or unclear on its own ("JAI" alone): with the rest of
            // the sentence it may well be words.
            if let Some(pcm) = kept.filter(|_| stitching_enabled && superseded()) {
                info!("[STITCH] Nothing clear in {speech_ms} ms on its own; hearing it again with the rest");
                self.keep_audio_for_stitching(pcm, speech_ms, u.barge_in, u.ptt, unjudged);
                return;
            }
            self.rest_state();
            return;
        }

        // Sir's privacy lock ("stop listening"): nothing gets through — no command, no question,
        // not even "JAI" — but "you can listen again", and the question how long the lock still
        // holds. Nor do words that were recorded just before the microphone was switched off.
        // The words are not logged (nor by the recogniser: `stt::words_for_the_log`), and
        // nothing of them stays on screen.
        match crate::listening_lock::verdict(&transcript.text, u.ptt || self.inner.controls.mic_enabled.load(Ordering::Acquire)) {
            crate::listening_lock::Verdict::Heard => {}
            crate::listening_lock::Verdict::Resumes => {
                crate::hud_lines::speech_over();
                self.set_listening_lock(crate::listening_lock::LockCommand::Resume, Trigger::Input(InputSource::Voice));
                return;
            }
            crate::listening_lock::Verdict::Asks => {
                let line = crate::listening_lock::how_long();
                info!("[PRIVACY] Asked how long he does not listen: '{line}' (the lock stays)");
                crate::hud_lines::speech_over();
                self.speak_standalone(&line, false, Trigger::Input(InputSource::Voice));
                return;
            }
            crate::listening_lock::Verdict::Ignored => {
                info!("[PRIVACY] Not listening: {} words ignored", transcript.text.split_whitespace().count());
                // (Nothing of them stays on screen. Where Sir asked to see what is heard under
                // the lock, the words stand their moment and fade, as a guess does.)
                if !crate::listening_lock::may_show_heard() {
                    crate::hud_lines::speech_over();
                }
                self.rest_state();
                return;
            }
        }
        let voice: Option<Result<Vec<f32>, String>> = match voice_print {
            Some(print) => print.await.ok(),
            None => None,
        };
        // The voice set-up is listening: what is said is a line read to it, not an order.
        if in_setup && crate::voice_setup::heard(&transcript.text, voice.clone().and_then(Result::ok), voice_levels) {
            crate::hud_lines::speech_over();
            if let Some((prints, texts, levels)) = crate::voice_setup::take_finished() {
                self.learn_from_setup(prints, texts, levels);
            }
            self.rest_state();
            return;
        }
        // "Heard: … – do it?" stands on the HUD and Sir says yes: the offered order is his.
        if !lifecycle::is_sleeping() {
            if let Some(order) = crate::hud_lines::accept_by_voice(&transcript.text) {
                info!("[WAKE] Sir confirmed the order on offer: '{order}'");
                crate::hud_lines::settled(&order);
                self.emit(HudEvent::Transcript { text: order.clone(), language: transcript.language.clone(), source: "voice".into(), wake_word: Some("confirmed".into()), stt_ms: transcript.stt_ms, audio_ms: transcript.audio_ms });
                self.dispatch_turn(order, Instant::now(), transcript.stt_ms, InputSource::Voice);
                return;
            }
            // He asked "Did you mean …?" (`ask_first`) or offers the orders he does know
            // (`did_you_mean`), and Sir answers ("yes", "no", "the second one"): the answer is
            // his to hear, with or without his name in it.
            if crate::ask_first::would_answer(&transcript.text) || crate::did_you_mean::would_answer(&transcript.text) {
                info!("[WAKE] Sir answers what he was asked: '{}'", transcript.text);
                crate::hud_lines::settled(&transcript.text);
                self.emit(HudEvent::Transcript { text: transcript.text.clone(), language: transcript.language.clone(), source: "voice".into(), wake_word: Some("answer".into()), stt_ms: transcript.stt_ms, audio_ms: transcript.audio_ms });
                self.dispatch_turn(transcript.text.clone(), Instant::now(), transcript.stt_ms, InputSource::Voice);
                return;
            }
        }
        let c = &self.inner.controls;
        let homecoming = crate::ears::is_homecoming_phrase(&transcript.text);
        // Sleep mode: nothing but "wake up" gets through.
        if lifecycle::is_sleeping() {
            if homecoming {
                lifecycle::set_sleeping(false);
                self.emit(HudEvent::ConfigState(self.config_state()));
            } else if lifecycle::is_wake_up_phrase(&transcript.text) {
                lifecycle::set_sleeping(false);
                self.emit(HudEvent::ConfigState(self.config_state()));
                self.speak_standalone("Welcome back, Sir. I'm listening.", true, Trigger::Input(InputSource::Voice));
                return;
            } else {
                info!("[SLEEP] Ignored while asleep: '{}'", transcript.text);
                self.rest_state();
                return;
            }
        }
        let wake_required = c.wake_word_required.load(Ordering::Relaxed);
        // (A stitched sentence is in a conversation only if a piece of it was spoken to him.)
        let in_window = in_conversation(u.in_window, self.has_carry(), joined, c.window_remaining_ms() > 0, c.turn_active.load(Ordering::Acquire));
        // Whose voice it is by Sir's print, and what the print learns from this sentence.
        let db = self.inner.config.read(|c| c.memory_db_path.clone());
        let judged = match &voice {
            Some(Ok(clip)) => crate::speaker_id::hear(&db, clip, speech_ms, !scripted),
            _ => None,
        };
        let his_voice = judged.is_some_and(|j| j.sure);
        let (jai_focused, gaming) = foreground_context().await;
        // A guest Sir has just introduced is saying the sentences his or her print is made of.
        // (Nothing that passes for Sir's own voice, by itself or in the moment of trust.)
        if !scripted && crate::kit_voice::guest_learning() && !judged.is_some_and(|j| j.his) {
            if let Some(Ok(clip)) = &voice {
                match crate::kit_voice::guest_heard(clip.clone(), speech_ms) {
                    Ok(guest) => {
                        let line = match crate::speaker_id::keep_guest(&db, &guest, true) {
                            Ok(()) => {
                                info!("[VOICE] A guest's voice is known now: {} ({} sentences)", guest.name, guest.clips);
                                format!("Thank you, {0}. I know your voice now. Sir, say \"{0} may only control the music\" if that is all {0} should do.", guest.name)
                            }
                            Err(e) => format!("I could not keep {}'s voice, Sir: {e}", guest.name),
                        };
                        self.speak_standalone(&line, true, Trigger::Input(InputSource::Voice));
                    }
                    Err(left) => {
                        info!("[VOICE] A guest's sentence is taken; {left} more wanted");
                        crate::hud_notify::record(&format!("Heard. {left} more sentence{} in that voice, please.", if left == 1 { "" } else { "s" }), false);
                    }
                }
                crate::hud_lines::speech_over();
                self.rest_state();
                return;
            }
        }
        // Dictation: what Sir says is typed into the app in front, not taken as an order. (In
        // his own voice only, when the lock holds.)
        // (Where his voice is known, only his voice is typed, lock or no lock; where it is not,
        // nothing that is plainly a song, a video or talk in the room.)
        let typed_voice = match judged {
            Some(j) => j.his,
            None => crate::ears::background_speech(&transcript.text, true, &[]).is_none(),
        };
        if crate::kit_typing::dictating() && typed_voice {
            crate::hud_lines::speech_over();
            match crate::kit_typing::dictate(&transcript.text, jai_focused).await {
                Ok(shown) if !crate::kit_typing::dictating() => {
                    self.emit(HudEvent::Transcript { text: transcript.text.clone(), language: transcript.language.clone(), source: "dictation".into(), wake_word: None, stt_ms: transcript.stt_ms, audio_ms: transcript.audio_ms });
                    self.speak_standalone(&format!("{shown}, Sir."), true, Trigger::Input(InputSource::Voice));
                    return;
                }
                Ok(shown) => {
                    crate::hud_lines::settled(&shown);
                    self.emit(HudEvent::Transcript { text: shown, language: transcript.language.clone(), source: "dictation".into(), wake_word: None, stt_ms: transcript.stt_ms, audio_ms: transcript.audio_ms });
                }
                Err(why) => {
                    warn!("[DICTATION] Not typed: {why}");
                    self.emit(HudEvent::Error { message: why });
                }
            }
            self.rest_state();
            return;
        }
        // The pronunciation coach: what Sir says is the sentence on the board, read to him.
        if crate::kit_voice::coach_active() && crate::kit_voice::coach_heard(&transcript.text) {
            crate::hud_lines::speech_over();
            crate::hud_lines::settled(&transcript.text);
            self.rest_state();
            return;
        }
        // His ear for Sir's words (`ear`): what he has learnt to take them for, what sounds
        // like a name where a name belongs, and for words spoken to him that lead nowhere, a
        // second and careful hearing of the clip.
        // (A word that only stands for his name in an accented voice, "They", counts as his
        // name in Sir's own sure voice alone.)
        let named = {
            let matcher = self.inner.wake.read().unwrap_or_else(|e| e.into_inner());
            let first = transcript.text.split_whitespace().next();
            matcher.evaluate(&transcript.text).matched || first.is_some_and(crate::wake::real_name_word) || (his_voice && first.is_some_and(crate::wake::name_word))
        };
        let spoken_to_him = homecoming || u.ptt || u.barge_in || named || in_window || jai_focused;
        let first_heard = transcript.text.clone();
        let mut reading = crate::ear::read(&transcript.text, spoken_to_him, &crate::ear::is_order);
        // (first hearing, second hearing) when a second hearing stood for the first.
        let mut heard_twice: Option<(String, String)> = None;
        // (Only for a first hearing the recogniser itself was unsure of, and never for an order
        // he knows, the dispatcher's own included: 2026-11-13 "Jay, don't listen." was heard
        // twice, and the lock came 1.7 s later for it.)
        let first_hearing = crate::stt::HeardQuality { avg_logprob: transcript.avg_logprob, language_prob: transcript.language_prob, no_speech: transcript.no_speech };
        if reading.is_none() && spoken_to_him && crate::ear::worth_hearing_again(&transcript.text, first_hearing, &crate::ear::is_order) {
            if let Some(clip) = again_audio {
                match self.inner.stt.transcribe_carefully(clip, speech_ms, crate::ear::hint()).await {
                    Ok(again) => {
                        let text = crate::wake::canon(&again.text);
                        let second = crate::ear::read(&text, true, &crate::ear::is_order);
                        let led = second.as_ref().map_or_else(|| text.clone(), |r| r.text.clone());
                        if !text.is_empty() && crate::ear::leads_somewhere(&led, &crate::ear::is_order) && !crate::ear::second_hearing_fits(&transcript.text, &led) {
                            info!("[EAR] Heard again, carefully: '{led}' does not sound like '{}' (or takes something away): the first hearing stands", transcript.text);
                        } else if !text.is_empty() && crate::ear::leads_somewhere(&led, &crate::ear::is_order) {
                            info!("[EAR] Heard again, carefully: '{}' is '{led}'", transcript.text);
                            // (Learnt once it is carried out, in Sir's voice: below.)
                            heard_twice = Some((transcript.text.clone(), led.clone()));
                            transcript.text = text;
                            transcript.avg_logprob = again.avg_logprob;
                            transcript.no_speech = again.no_speech;
                            transcript.stt_ms += again.stt_ms;
                            reading = second;
                        } else {
                            info!("[EAR] Heard again, carefully: '{text}' is no order he knows either; the first hearing stands");
                        }
                    }
                    Err(e) => tracing::debug!("[EAR] No second hearing: {e}"),
                }
            }
        }
        if let Some(r) = &reading {
            info!("[EAR] '{}' → '{}' ({})", transcript.text, r.text, r.why());
            transcript.text = r.text.clone();
        } else if heard_twice.is_none() && !scripted && !crate::intents::says_misheard(&transcript.text) {
            // Taken as it was written: "that's not what I said" after it is about this sentence.
            crate::ear::no_rewrite();
        }
        // His name: as it is written, and in Sir's own voice also as it arrives there ("They
        // open the mixer.").
        let (wake, new_name) = {
            let matcher = self.inner.wake.read().unwrap_or_else(|e| e.into_inner());
            // (Not over a game or a voice chat: there "they play on PC" is talk about others.)
            if his_voice && !gaming { matcher.evaluate_in_his_voice(&transcript.text) } else { (matcher.evaluate(&transcript.text), None) }
        };
        // Who is Sir talking to? Push-to-talk, a barge-in and the welcome phrase are always for
        // JAI; everything else goes through the four-layer addressee detector.
        let addressee = if homecoming || u.ptt || u.barge_in {
            crate::ears::Addressee::WakeWord
        } else if !wake_required && !self.inner.config.read(|c| c.addressee_detection) {
            crate::ears::Addressee::HandsFree
        } else {
            let ctx = crate::ears::AddresseeContext { wake_word: wake.matched, wake_required, conversation_open: in_window, jai_focused, gaming };
            crate::ears::classify_addressee(&transcript.text, &ctx)
        };
        let source = if u.ptt {
            "ptt"
        } else if u.barge_in {
            "barge-in"
        } else {
            addressee.label()
        };

        // Background speech: humming, sung syllables, Whisper's all-caps lines and (hands-free)
        // implausible fragments never reach the brain without the wake word.
        let forced = homecoming || u.ptt || u.barge_in || wake.matched;
        if !forced && addressee.accepted() {
            let hands_free = addressee == crate::ears::Addressee::HandsFree;
            if let Some(reason) = crate::ears::background_speech(&transcript.text, hands_free, &self.recent_turns()) {
                info!("[Background Speech Ignored] {reason}: '{}'", transcript.text);
                self.inner.heard.lock().unwrap_or_else(|e| e.into_inner()).push(&transcript.text, true);
                self.emit(HudEvent::Transcript {
                    text: transcript.text,
                    language: transcript.language,
                    source: "ignored".into(),
                    wake_word: Some(format!("background · {reason}")),
                    stt_ms: transcript.stt_ms,
                    audio_ms: transcript.audio_ms,
                });
                self.rest_state();
                return;
            }
        }
        // A song through the microphone: while a player is playing, words Whisper is unsure of and
        // that are no command are the song's, not Sir's (the player is only asked when in doubt).
        if !forced && addressee.accepted() && transcript.avg_logprob < crate::ears::UNSURE_LOGPROB {
            let playing = tokio::time::timeout(Duration::from_millis(200), crate::music::now_playing()).await.ok().flatten().is_some_and(|np| np.playing);
            if crate::ears::sung_not_said(&transcript.text, transcript.avg_logprob, playing) {
                info!("[Background Speech Ignored] sung, not said (a player is playing; log-probability {:.2}, no-speech {:.2}): '{}'", transcript.avg_logprob, transcript.no_speech, transcript.text);
                self.inner.heard.lock().unwrap_or_else(|e| e.into_inner()).push(&transcript.text, true);
                self.emit(HudEvent::Transcript {
                    text: transcript.text,
                    language: transcript.language,
                    source: "ignored".into(),
                    wake_word: Some("background · music".into()),
                    stt_ms: transcript.stt_ms,
                    audio_ms: transcript.audio_ms,
                });
                self.rest_state();
                return;
            }
        }
        if !addressee.accepted() {
            self.inner.heard.lock().unwrap_or_else(|e| e.into_inner()).push(&transcript.text, true);
            info!("[WAKE] Not addressed to JAI ({addressee:?}, wake score {:.2}): '{}'", wake.score, transcript.text);
            crate::hud_lines::speech_over();
            // An order he knows, said without his name: he does not act and does not speak,
            // the HUD asks ("Heard: … – do it?"; a click or a spoken yes carries it out).
            if addressee != crate::ears::Addressee::Chatter && !forced && crate::ears::known_command(&transcript.text) && !crate::platform::in_test_session() {
                crate::hud_lines::offer(&transcript.text);
            }
            self.emit(HudEvent::Transcript {
                text: transcript.text,
                language: transcript.language,
                source: "ignored".into(),
                wake_word: Some(if addressee == crate::ears::Addressee::Chatter { "chatter".into() } else { "no wake word".into() }),
                stt_ms: transcript.stt_ms,
                audio_ms: transcript.audio_ms,
            });
            self.rest_state();
            return;
        }
        if !wake.matched {
            // (Every sentence that becomes a turn without his name says here why it was his to
            // hear, a follow-up too: on 2026-11-13 a lyric went through as one and left no line.)
            let stitched = match joined {
                Some(true) => ", stitched to words that were his to hear",
                Some(false) => ", stitched to audio that was never judged",
                None => "",
            };
            info!("[WAKE] Addressed without the wake word ({addressee:?}{stitched}): '{}'", transcript.text);
        }
        if wake.matched {
            info!("[WAKE] '{}' (score {:.2}) → command '{}'", wake.word, wake.score, wake.command);
            // (His silent "yes?": the emblem ripples.)
            crate::hud_lines::woke();
        }
        // The voice lock: a spoken order is carried out only in Sir's own voice. (Push-to-talk
        // and the keyboard are his hands; a voice that cannot be judged is let through and
        // logged, so a fault of the lock never locks Sir out.)
        // (Whose voice it was, for "who is speaking?".)
        let guest = match (&judged, &voice) {
            (Some(j), Some(Ok(clip))) if !j.his => crate::speaker_id::guest_of(&db, clip),
            _ => None,
        };
        // (The question "who was that?" is about the sentence before it.)
        if !crate::kit_voice::asks_who(&transcript.text) {
            crate::kit_voice::note_speaker(match (&judged, &guest) {
                (Some(j), _) if j.his => "you",
                (_, Some((guest, _))) => guest.name.as_str(),
                (Some(_), None) => "a voice I do not know",
                (None, _) => "someone whose voice I could not judge",
            });
        }
        // A guest Sir introduced may speak: everything, or the music and the volume only.
        let guest_may = guest.as_ref().is_some_and(|(guest, score)| {
            let order = if wake.matched { wake.command.as_str() } else { transcript.text.as_str() };
            let may = guest.may != "music" || crate::kit_voice::music_order(order);
            info!("[VOICE LOCK] {}'s voice (likeness {score:.2}), a guest who may {}: '{order}' is {}", guest.name, if guest.may == "music" { "control the music" } else { "ask for everything" }, if may { "carried out" } else { "not carried out" });
            may
        });
        if lock_on && !guest_may {
            match (&judged, &voice) {
                (Some(j), _) if !j.his => {
                    info!("[VOICE LOCK] Not Sir's voice (likeness {:.2}, his from {:.2}): '{}' is not carried out", j.score, j.bar, transcript.text);
                    // (Orders by name that fall just short, three in a row: the bar goes back
                    // to the set-up's, so that a cold or another microphone is no lock-out.)
                    if crate::speaker_id::turned_away(&db, *j, wake.matched, !scripted) {
                        crate::hud_notify::record("Voice lock: your voice reached me just short of my bar three times. I have lowered it to where the set-up put it.", false);
                    }
                    crate::hud_lines::speech_over();
                    crate::hud_lines::turned_away();
                    self.emit(HudEvent::Transcript {
                        text: transcript.text,
                        language: transcript.language,
                        source: "ignored".into(),
                        wake_word: Some("voice lock".into()),
                        stt_ms: transcript.stt_ms,
                        audio_ms: transcript.audio_ms,
                    });
                    self.rest_state();
                    return;
                }
                (Some(j), _) => info!("[VOICE LOCK] Sir's voice (likeness {:.2}, his from {:.2}{})", j.score, j.bar, if j.sure { "" } else { "; taken on trust, moments after a sentence that was surely his" }),
                (None, Some(Err(why))) if crate::speaker_id::known(&db).is_some() => warn!("[VOICE LOCK] The voice could not be judged ({why}): the order is carried out"),
                _ => {}
            }
        }
        // His words stand under the emblem as they were understood.
        crate::hud_lines::settled(&transcript.text);
        crate::hud_lines::dismiss_offer();
        // A bare "JAI" and a breath: since a short burst ends after 160-220 ms of silence, the
        // rest of the sentence often starts a moment later. Wait for it before listening on or
        // acting on words heard earlier (the retroactive wake word).
        if wake.matched && !homecoming && stitching_enabled && crate::ears::is_retro_reference(&wake.command) {
            let deadline = Instant::now() + BARE_WAKE_GRACE;
            while Instant::now() < deadline && !superseded() && !c.in_speech.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            if superseded() || c.in_speech.load(Ordering::Acquire) {
                info!("[STITCH] '{}' — the rest of the sentence follows", transcript.text);
                self.emit(HudEvent::Transcript {
                    text: transcript.text.clone(),
                    language: transcript.language,
                    source: source.into(),
                    wake_word: Some(wake.word),
                    stt_ms: transcript.stt_ms,
                    audio_ms: transcript.audio_ms,
                });
                self.emit(HudEvent::Retracted { message_id: String::new() });
                match kept {
                    // (Said with his name: his to hear.)
                    Some(pcm) => self.keep_audio_for_stitching(pcm, speech_ms, u.barge_in, u.ptt, true),
                    None => self.keep_for_stitching(transcript.text),
                }
                return;
            }
        }
        let mut heard = if wake.matched && !homecoming { wake.command.clone() } else { transcript.text.clone() };
        // The retroactive wake word: "… turn the music down. — JAI, do that." acts on the
        // words spoken just before the name (ignored then, at most 12 s ago).
        if wake.matched && !homecoming && crate::ears::is_retro_reference(&heard) {
            if let Some(earlier) = self.inner.heard.lock().unwrap_or_else(|e| e.into_inner()).take_referenced() {
                info!("[WAKE] Retroactive: '{}' refers to '{earlier}'", transcript.text);
                heard = earlier;
            }
        }
        self.inner.heard.lock().unwrap_or_else(|e| e.into_inner()).push(&transcript.text, false);
        // The rest of an earlier, unfinished sentence: one request — unless the new words are a
        // command of their own that the held ones would only spoil (2026-11-08: "told me the
        // world", held after a slow turn, made "JAI, show me the world" 13 s later into
        // "told me the world show me the world").
        let carried = if heard.trim().is_empty() { None } else { self.take_carry() };
        let named_first = wake.matched && !homecoming && wake_leads(&transcript.text, &wake.word);
        let carried = match carried {
            Some(earlier) if supersedes(&earlier, heard.trim(), named_first, &crate::command_corrector::routes) => {
                info!("[STITCH] '{}' is a command of its own; the held '{earlier}' is dropped", heard.trim());
                None
            }
            other => other,
        };
        let command = match &carried {
            Some(earlier) => format!("{earlier} {}", heard.trim()),
            None => heard,
        };
        if carried.is_some() {
            info!("[STITCH] Joined: '{command}'");
        }
        if command.trim().is_empty() {
            // A bare "JAI" (or noise that sounded like it): the orb listens; neither a
            // "JAI" line nor a "Yes, Sir?" reply lands in the chat.
            self.wake_listening("Bare wake word");
            return;
        }
        // The request is known: whatever it only needs to read is fetched from this moment on,
        // while the turn is still being set up (and while an unfinished thought is waited for).
        crate::hyper_cortex::anticipate(self.tools(), &command);
        let stitched = carried.is_some() || joined.is_some();
        self.emit(HudEvent::Transcript {
            text: if carried.is_some() { command.clone() } else { transcript.text },
            language: transcript.language,
            source: if stitched { "stitched".into() } else { source.into() },
            wake_word: wake.matched.then_some(wake.word),
            stt_ms: transcript.stt_ms,
            audio_ms: transcript.audio_ms,
        });
        // Sir started speaking again while this was being transcribed: wait for the rest.
        if stitching_enabled && superseded() {
            info!("[STITCH] More speech followed '{command}'; waiting for it");
            // No reply to withdraw; the HUD marks Sir's line as the start of a longer sentence.
            self.emit(HudEvent::Retracted { message_id: String::new() });
            self.stitch_rest(kept, speech_ms, u.barge_in, u.ptt, carried, command);
            return;
        }
        // "Set the colour to …", "Open the …": give Sir a moment to finish the thought.
        if stitching_enabled && crate::intents::looks_incomplete(&command) {
            let wait = Duration::from_millis(self.inner.config.read(|c| c.incomplete_thought_wait_ms) as u64);
            let deadline = Instant::now() + wait;
            while Instant::now() < deadline && !superseded() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            if superseded() || self.inner.controls.in_speech.load(Ordering::Acquire) {
                info!("[STITCH] '{command}' sounded unfinished and Sir carried on");
                self.emit(HudEvent::Retracted { message_id: String::new() });
                self.stitch_rest(kept, speech_ms, u.barge_in, u.ptt, carried, command);
                return;
            }
        }
        // What this sentence teaches him, now that it is carried out: a rewrite that stood, his
        // name as it arrives in Sir's voice, and (while no print of that voice is known) the
        // voice of an order given by name.
        if !scripted {
            if let Some(r) = &reading {
                crate::ear::stood(r);
            }
            if let Some((first, second)) = heard_twice.as_ref().filter(|_| judged.is_none_or(|j| j.his)) {
                crate::ear::second_hearing_stood(first, second);
            }
            // (How often his name arrived as another word in his voice: counted for "what have
            // you learnt about my voice". It is his name in his voice only, never a word
            // anyone can call him by.)
            if let Some(word) = &new_name {
                crate::ear::name_heard(word);
            }
            if judged.is_none() && wake.matched && wake.score >= 0.95 && !u.ptt {
                if let Some(Ok(clip)) = &voice {
                    self.gather_voice(&db, clip.clone(), speech_ms);
                }
            }
            // The ear's gate follows his voice: sentences that were surely his (by his print,
            // or by his name while no print is known), spoken to the open microphone.
            if !u.ptt && judged.map_or(wake.matched && wake.score >= 0.95, |j| j.sure) {
                self.gate_follows(voice_levels);
            }
            // The clip itself, with what it was heard as and taken for: Sir's voice only (by
            // his print, or by his name while no print is known).
            if let Some(pcm) = clip_audio.filter(|_| judged.map_or(wake.matched || u.ptt, |j| j.sure)) {
                crate::voice_clips::keep(pcm, first_heard, command.clone(), crate::ear::leads_somewhere(&command, &crate::ear::is_order), transcript.avg_logprob, judged.map_or(0.0, |j| j.score));
            }
        }
        self.inner.barge_in_turn.store(u.barge_in, Ordering::Release);
        *self.inner.heard_turn.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(crate::stt::HeardQuality { avg_logprob: transcript.avg_logprob, language_prob: transcript.language_prob, no_speech: transcript.no_speech });
        if let Some(pcm) = kept {
            *self.inner.turn_audio.lock().unwrap_or_else(|e| e.into_inner()) = Some((pcm, speech_ms));
        }
        self.dispatch_turn(command, u.ended_at, transcript.stt_ms, InputSource::Voice);
    }

    /// The first half of a sentence waits for the rest: as audio (heard again together with it),
    /// with any older text still in front; as text when there is no audio. (Only for a first
    /// half that was judged and found to be his to hear.)
    fn stitch_rest(&self, kept: Option<Vec<f32>>, speech_ms: u32, barge_in: bool, ptt: bool, carried: Option<String>, command: String) {
        match kept {
            Some(pcm) => {
                if let Some(earlier) = carried {
                    self.keep_for_stitching(earlier);
                }
                self.keep_audio_for_stitching(pcm, speech_ms, barge_in, ptt, true);
            }
            None => self.keep_for_stitching(command),
        }
    }

    /// "Stop listening (for 5 minutes)" / "you can listen again": the privacy lock, confirmed in
    /// his voice.
    pub fn set_listening_lock(&self, cmd: crate::listening_lock::LockCommand, trigger: Trigger) {
        crate::listening_lock::apply(cmd);
        info!("[PRIVACY] {cmd:?}");
        self.emit(HudEvent::ConfigState(self.config_state()));
        self.speak_standalone(&crate::listening_lock::reply(cmd), false, trigger);
    }

    /// What the dispatcher itself does with a request, before any turn exists: the privacy lock
    /// (the first stage of the reflex router), also as a clause of a sentence that asks for more
    /// ("stop listening and set the theme to Arc Cyan"). The live dispatcher and the dry run both
    /// decide here.
    pub fn admit(text: &str) -> Admission {
        use crate::intent_engine::{Reflex, ReflexContext};
        // (A typed slip is the same command; the routes behind this one read it right anyway.)
        let typed = crate::typos::normalize_user_input_typos(text);
        let text = typed.as_str();
        let ctx = ReflexContext::live();
        if let Some(plan) = crate::compound_planner::plan(text, &ctx).filter(|p| p.privacy.is_some()) {
            return Admission::Compound { privacy: plan.privacy, rest: plan.rest() };
        }
        match crate::intent_engine::route(text, &ctx).map(|r| r.reflex) {
            Some(Reflex::Privacy(cmd)) => Admission::Privacy(cmd),
            _ => Admission::Turn,
        }
    }

    /// Starts a brain turn, superseding any running one.
    pub fn dispatch_turn(&self, text: String, started: Instant, stt_ms: u64, source: InputSource) {
        // His name in one spelling, typed or spoken ("J.A.I., open Spotify", "jay open spotify").
        let text = crate::wake::canon(&text);
        // The privacy lock: answered at once — before music or any other parser, and without the
        // model. ("You can listen again" while he is listening anyway is an ordinary sentence.)
        let lifts_nothing = |cmd: crate::listening_lock::LockCommand| cmd == crate::listening_lock::LockCommand::Resume && !crate::listening_lock::is_locked();
        let text = match Self::admit(&text) {
            Admission::Privacy(cmd) if !lifts_nothing(cmd) => {
                crate::intent_engine::forget_follow_up();
                self.set_listening_lock(cmd, Trigger::Input(source));
                return;
            }
            Admission::Compound { privacy, rest } => {
                crate::intent_engine::forget_follow_up();
                let privacy = privacy.filter(|cmd| !lifts_nothing(*cmd));
                info!("[COMPOUND] '{text}' → privacy {privacy:?}, then '{rest}'");
                if rest.is_empty() {
                    // Nothing for the brain: the switch is confirmed in his voice as usual.
                    if let Some(cmd) = privacy {
                        self.set_listening_lock(cmd, Trigger::Input(source));
                    }
                    return;
                }
                // The switch happens now and without a line of its own: the turn for the other
                // clauses answers for the whole sentence.
                if let Some(cmd) = privacy {
                    crate::listening_lock::apply(cmd);
                    info!("[PRIVACY] {cmd:?}");
                }
                self.emit(HudEvent::ConfigState(self.config_state()));
                rest
            }
            _ => text,
        };
        // A turn for JAI: out of the zero-VRAM standby, and the idle countdown starts over.
        crate::vram_guard::touch();
        // "Stop" / "pause" (also "stopp", "halt", "pause music", "JAI, stop"): whatever is
        // playing stops right now — every playing media session and JAI's own voice (stopped
        // just below) — before the turn even queues. The brain's silent command reports the
        // same run (`stop_all_active_media` shares one result per burst).
        if crate::intents::is_media_stop(&text) {
            tokio::spawn(async {
                crate::music::stop_all_active_media().await;
            });
        }
        *self.inner.last_source.lock().unwrap_or_else(|e| e.into_inner()) = source;
        let barge_in = self.inner.barge_in_turn.swap(false, Ordering::AcqRel) && source == InputSource::Voice;
        self.note_turn(&text);
        if lifecycle::set_sleeping(false) {
            self.emit(HudEvent::ConfigState(self.config_state()));
        }
        let generation = self.inner.tts.stop();
        let cancel = CancellationToken::new();
        let id = uuid::Uuid::new_v4().to_string();
        let voice = self.tools().modality.gate(Trigger::Input(source), &text);
        let link = Arc::new(TurnLink::default());
        let heard = self.inner.heard_turn.lock().unwrap_or_else(|e| e.into_inner()).take();
        link.set_heard(heard.filter(|_| source == InputSource::Voice));
        {
            let mut turn = self.turn_lock();
            if let Some(prev) = turn.take() {
                prev.cancel.cancel();
            }
            *turn = Some(ActiveTurn {
                id: id.clone(),
                cancel: cancel.clone(),
                generation,
                started,
                stt_ms,
                first_token_ms: None,
                first_audio_logged: false,
                // Typed questions do not open the microphone's conversation window.
                opens_window: source == InputSource::Voice,
                source: Some(source),
                request: (source == InputSource::Voice).then(|| text.clone()),
                audio: self.inner.turn_audio.lock().unwrap_or_else(|e| e.into_inner()).take().filter(|_| source == InputSource::Voice),
                link: Arc::clone(&link),
            });
        }
        self.inner.controls.turn_active.store(true, Ordering::Release);
        self.inner.controls.close_window();
        self.set_state("thinking", "Thinking…");
        let app = self.clone();
        tokio::spawn(async move {
            // The emblem's gyroscope follows this guard: it drops however the turn ends.
            let thinking = crate::thinking_guard::engage();
            let req = TurnRequest { text, message_id: id.clone(), generation, cancel, started, source, voice, link, barge_in };
            app.inner.brain.run_turn(req, &app.inner.tts, &app.inner.brain_tx).await;
            drop(thinking);
            app.settle_turn(id).await;
        });
    }

    /// The watchdog after a brain turn: if the HUD still says "thinking" long after the brain
    /// finished (no audio ever started, so no `PlaybackFinished` will come), the turn is closed and
    /// JAI rests — he never stays stuck in the thinking state.
    async fn settle_turn(&self, id: String) {
        tokio::time::sleep(Duration::from_secs(12)).await;
        let stuck = self.hud_state().state == "thinking" && !self.inner.controls.speaking_or_tail() && self.inner.stt_pending.load(Ordering::Acquire) == 0;
        if !stuck {
            return;
        }
        let ours = {
            let mut turn = self.turn_lock();
            if turn.as_ref().is_some_and(|t| t.id == id) {
                turn.take();
                true
            } else {
                turn.is_none()
            }
        };
        if ours {
            warn!("[TURN] Watchdog: still 'thinking' 12 s after the brain finished; resting");
            self.inner.controls.turn_active.store(false, Ordering::Release);
            self.rest_state();
        }
    }

    /// Remembers a request or a reply for the hands-free continuity check (the last four).
    fn note_turn(&self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let mut recent = self.inner.recent_turns.lock().unwrap_or_else(|e| e.into_inner());
        recent.push_back(text.to_string());
        while recent.len() > 4 {
            recent.pop_front();
        }
    }

    fn recent_turns(&self) -> Vec<String> {
        self.inner.recent_turns.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
    }

    /// A bare "JAI" or a double clap: JAI listens for the request — the orb's cyan
    /// listening ring and the follow-up window open, with a soft chime when his reply mode lets
    /// him make a sound — without a "Yes, Sir?" bubble in the chat (his own echo and room noise
    /// used to fill it with them, one after another).
    fn wake_listening(&self, why: &str) {
        let secs = self.inner.config.read(|c| c.active_conversation_window_secs).max(5);
        // A second clap or "JAI" while he is already listening only extends the window.
        let already_listening = self.inner.controls.window_remaining_ms() > 0;
        self.inner.controls.open_window(secs);
        let chime = !already_listening
            && self.tools().modality.gate(Trigger::Input(InputSource::Voice), "").speaks()
            && !self.inner.controls.speaking_or_tail();
        if chime {
            self.inner.tts.chime();
        }
        info!("[WAKE] {why}: listening for {secs} s{}", if chime { " (chime)" } else { "" });
        self.set_state("listening", "Listening — go ahead, Sir");
    }

    /// A fixed line outside of an LLM turn (greeting, alerts, tests), spoken when Sir's reply
    /// mode allows it for `trigger` and always shown in the chat.
    pub fn speak_standalone(&self, text: &str, opens_window: bool, trigger: Trigger) {
        let generation = self.inner.tts.stop();
        {
            let mut turn = self.turn_lock();
            if let Some(prev) = turn.take() {
                prev.cancel.cancel();
            }
            *turn = Some(ActiveTurn {
                id: uuid::Uuid::new_v4().to_string(),
                cancel: CancellationToken::new(),
                generation,
                started: Instant::now(),
                stt_ms: 0,
                first_token_ms: None,
                first_audio_logged: true,
                opens_window,
                source: None,
                request: None,
                audio: None,
                link: Arc::new(TurnLink::default()),
            });
        }
        self.inner.controls.turn_active.store(true, Ordering::Release);
        self.emit(HudEvent::AssistantDone {
            message_id: uuid::Uuid::new_v4().to_string(),
            text: text.to_string(),
            latency_ms: 0,
            model: "jai".into(),
            interrupted: false,
        });
        if self.tools().modality.gate(trigger, text).speaks() {
            self.inner.tts.speak(text, generation);
        }
        self.inner.tts.finish(generation);
    }

    /// Stops speech and cancels the running turn (UI stop button, barge-in, PTT).
    pub fn interrupt(&self, reason: &str) {
        self.inner.tts.stop();
        let had_turn = {
            let mut turn = self.turn_lock();
            match turn.take() {
                Some(t) => {
                    t.cancel.cancel();
                    true
                }
                None => false,
            }
        };
        self.inner.controls.turn_active.store(false, Ordering::Release);
        if had_turn {
            info!("[TURN] Interrupted ({reason})");
            self.set_state("interrupted", format!("Interrupted: {reason}"));
            let app = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(700)).await;
                if app.hud_state().state == "interrupted" {
                    app.rest_state();
                }
            });
        }
    }

    fn on_tts(&self, ev: TtsEvent) {
        match ev {
            TtsEvent::PlaybackStarted { generation, synth_ms } => {
                let metrics = {
                    let mut turn = self.turn_lock();
                    // Sir has heard JAI answer: the turn can no longer be withdrawn.
                    if let Some(t) = turn.as_ref().filter(|t| t.generation == generation) {
                        t.link.commit();
                    }
                    match turn.as_mut().filter(|t| t.generation == generation) {
                        Some(t) if !t.first_audio_logged => {
                            t.first_audio_logged = true;
                            Some((t.started.elapsed().as_millis() as u64, t.stt_ms, t.first_token_ms))
                        }
                        Some(_) => None,
                        None => return,
                    }
                };
                if let Some((first_audio_ms, stt_ms, first_token_ms)) = metrics {
                    let (clause_ms, missed) = crate::tts::first_sound().unwrap_or_default();
                    info!(
                        "[TTS] First audio {first_audio_ms} ms after end of speech (STT {stt_ms} ms, first token {} , synth {synth_ms} ms; first clause to sound: median {clause_ms} ms, {missed} over 250 ms)",
                        first_token_ms.map(|m| format!("{m} ms")).unwrap_or_else(|| "n/a".into())
                    );
                    self.emit(HudEvent::TurnMetrics { stt_ms, first_token_ms, first_audio_ms });
                }
                self.set_state("speaking", "Speaking");
            }
            TtsEvent::PlaybackFinished { generation } => {
                let finished = {
                    let mut turn = self.turn_lock();
                    match turn.as_ref() {
                        Some(t) if t.generation == generation => turn.take(),
                        _ => None,
                    }
                };
                if let Some(t) = finished {
                    self.inner.controls.turn_active.store(false, Ordering::Release);
                    // After a goodbye nobody is listening for a follow-up.
                    if t.opens_window && !self.inner.quitting.load(Ordering::Acquire) {
                        let secs = self.inner.config.read(|c| c.active_conversation_window_secs);
                        self.inner.controls.open_window(secs);
                        info!("[STATE] Conversation window open for {secs} s");
                    }
                    self.rest_state();
                }
            }
        }
    }

    fn on_brain(&self, ev: BrainEvent) {
        match ev {
            BrainEvent::FirstToken { message_id, latency_ms, since_speech_ms, model } => {
                if let Some(t) = self.turn_lock().as_mut().filter(|t| t.id == message_id) {
                    t.first_token_ms = Some(since_speech_ms);
                }
                self.emit(HudEvent::FirstToken { message_id, latency_ms, since_speech_ms, model });
            }
            BrainEvent::Token { message_id, token } => self.emit(HudEvent::Token { message_id, token }),
            BrainEvent::Tool(outcome) => self.emit_tool(&outcome),
            BrainEvent::MemoriesChanged => {
                self.emit(HudEvent::Memories { items: self.tools().memory.list(200).unwrap_or_default(), query: None });
            }
            BrainEvent::SkillsChanged => {
                self.emit(HudEvent::Skills { items: self.tools().skills.list().unwrap_or_default() });
            }
            BrainEvent::Done { message_id, text, latency_ms, model, interrupted } => {
                info!("[TURN] Done in {latency_ms} ms via {model}{}", if interrupted { " (interrupted)" } else { "" });
                crate::hud_lines::took(latency_ms as u64, interrupted);
                // JAI asked something (even in writing): Sir may answer without the wake word.
                // A spoken reply re-opens the window once its audio has finished.
                if !interrupted && text.trim_end().ends_with('?') && !self.inner.quitting.load(Ordering::Acquire) {
                    let secs = self.inner.config.read(|c| c.active_conversation_window_secs);
                    self.inner.controls.open_window(secs);
                }
                self.note_turn(&text);
                self.emit(HudEvent::AssistantDone { message_id, text, latency_ms, model, interrupted });
            }
            BrainEvent::Quick { label, ok } => {
                info!("[QUICK] {label}");
                self.emit(HudEvent::QuickAction { label, ok });
            }
            BrainEvent::Error { error } => self.emit(HudEvent::Error { message: error }),
            BrainEvent::ShutdownRequested => self.shutdown_after_reply(),
            BrainEvent::StateChanged => {
                // `set_listening_mode` writes the config; the microphone loop reads the controls.
                let required = self.inner.config.read(|cfg| cfg.wake_word_required);
                if self.inner.controls.wake_word_required.swap(required, Ordering::AcqRel) != required {
                    info!("[EARS] Listening mode: {}", if required { "wake word" } else { "hands-free" });
                    if required {
                        self.inner.controls.close_window();
                    }
                    self.rest_state();
                }
                self.emit(HudEvent::ConfigState(self.config_state()));
                if lifecycle::is_sleeping() {
                    self.inner.controls.close_window();
                }
            }
            // The HUD was told when the turn was withdrawn (`withdraw_unanswered_turn`).
            BrainEvent::Retracted { message_id } => tracing::debug!("[STITCH] Turn {message_id} unwound"),
            BrainEvent::PowerCard { id, action, label, status } => {
                // Thirty seconds without an answer: the card says so and JAI tells Sir.
                if status == "expired" {
                    let line = if action == "restart" { "Restart aborted, Sir." } else { "Shutdown aborted, Sir." };
                    let source = *self.inner.last_source.lock().unwrap_or_else(|e| e.into_inner());
                    self.speak_standalone(line, false, Trigger::Input(source));
                }
                self.emit(HudEvent::PowerCard { id, action, label, status });
            }
        }
    }

    /// Quits once the goodbye has been spoken ("JAI, shut down", `shutdown_jai`).
    fn shutdown_after_reply(&self) {
        if self.inner.quitting.swap(true, Ordering::AcqRel) {
            return;
        }
        info!("[APP] Shutdown requested by Sir");
        // Nothing said from here on should start another turn.
        self.inner.controls.mic_enabled.store(false, Ordering::Release);
        let app = self.clone();
        tokio::spawn(async move {
            // `turn_active` stays set until the turn's audio has finished playing.
            let deadline = Instant::now() + Duration::from_secs(10);
            while app.inner.controls.turn_active.load(Ordering::Acquire) && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            app.inner.shutdown.cancel();
        });
    }

    fn emit_tool(&self, o: &ToolOutcome) {
        self.emit(HudEvent::ToolLog {
            tool: o.tool.clone(),
            args: o.args.clone(),
            result: o.output.clone(),
            success: o.success,
            duration_ms: o.duration_ms,
            timestamp: now_hms(),
        });
    }

    async fn finish_mic_test(&self, pcm: Vec<f32>, peak: f32, rms: f32, mic: String) {
        let db = crate::dsp::to_dbfs(rms);
        let line;
        let result;
        if peak < 0.004 {
            result = format!("{mic}: near silent (peak {peak:.4}, RMS {db:.1} dBFS). Check the mute switch and input level.");
            line = "Sir, your microphone is practically silent. Please check the mute switch or choose another microphone.".to_string();
        } else {
            match self.inner.stt.transcribe(pcm, 2500).await {
                Ok(t) if !t.text.is_empty() => {
                    result = format!("{mic}: peak {peak:.3}, RMS {db:.1} dBFS, heard [{}] \"{}\" in {} ms", t.language, t.text, t.stt_ms);
                    line = format!("Microphone check passed, Sir. I heard: {}", t.text);
                }
                Ok(_) => {
                    result = format!("{mic}: signal OK (peak {peak:.3}, RMS {db:.1} dBFS) but no words recognised.");
                    line = "Your microphone works, Sir, but I couldn't make out any words.".to_string();
                }
                Err(e) => {
                    result = format!("{mic}: speech recognition failed: {e}");
                    line = "The microphone works, Sir, but speech recognition is unavailable.".to_string();
                }
            }
        }
        self.emit(HudEvent::ToolLog {
            tool: "mic_test".into(),
            args: json!({ "mic": mic, "peak": peak, "rms_db": db }),
            result,
            success: peak >= 0.004,
            duration_ms: 2500,
            timestamp: now_hms(),
        });
        self.speak_standalone(&line, true, Trigger::Input(InputSource::Voice));
    }

    /// Live VAD tuning from the UI sliders (applied on the next 32 ms frame, persisted debounced).
    fn apply_vad(&self, start: Option<f32>, release: Option<f32>, silence_ms: Option<u32>, multiplier: Option<f32>) {
        let c = &self.inner.controls;
        c.set_vad(start, release, silence_ms, multiplier);
        let (s, r, m, h) = c.vad_live();
        self.inner.config.update(|cfg| {
            cfg.vad_start_threshold = s;
            cfg.vad_release_threshold = r;
            cfg.vad_noise_multiplier = m;
            cfg.silence_hangover_ms = h;
        });
        self.emit(HudEvent::ConfigState(self.config_state()));
    }

    /// A quick-action button: `jai: <command>`, `shell: <cmd>`, `skill: <name>`, `app: <name>`
    /// or `url: <address>` (no prefix = a JAI command).
    async fn run_widget_action(&self, action: &str) {
        let (kind, value) = match action.split_once(':') {
            Some((k, v)) if matches!(k.trim().to_lowercase().as_str(), "jai" | "shell" | "skill" | "app" | "url" | "https" | "http") => {
                (k.trim().to_lowercase(), v.trim().to_string())
            }
            _ => ("jai".to_string(), action.trim().to_string()),
        };
        match kind.as_str() {
            "shell" => self.run_ui_tool("run_shell_command", json!({ "command": value })).await,
            "skill" => self.run_ui_tool("execute_skill", json!({ "name": value })).await,
            "app" => self.run_ui_tool("open_application", json!({ "app_name": value })).await,
            "url" => self.run_ui_tool("open_application", json!({ "app_name": value })).await,
            "http" | "https" => self.run_ui_tool("open_application", json!({ "app_name": action.trim() })).await,
            _ if !value.is_empty() => {
                self.emit(HudEvent::Transcript {
                    text: value.clone(),
                    language: "text".into(),
                    source: "widget".into(),
                    wake_word: None,
                    stt_ms: 0,
                    audio_ms: 0,
                });
                self.dispatch_turn(value, Instant::now(), 0, InputSource::TextGui);
            }
            _ => {}
        }
    }

    async fn run_ui_tool(&self, tool: &str, args: serde_json::Value) {
        let outcome = self.tools().execute(tool, &args).await;
        self.emit_tool(&outcome);
        if outcome.state_changed || outcome.modality_changed {
            self.emit(HudEvent::ConfigState(self.config_state()));
        }
        if outcome.memories_changed {
            self.on_brain(BrainEvent::MemoriesChanged);
            self.inner.brain.request_prime();
        }
        if outcome.skills_changed {
            self.on_brain(BrainEvent::SkillsChanged);
            self.inner.brain.request_prime();
        }
    }

    /// Handles a HUD command; the optional return value goes only to the requesting caller.
    pub async fn handle_command(&self, cmd: HudCommand) -> Option<HudEvent> {
        let c = &self.inner.controls;
        match cmd {
            HudCommand::Chat { text } => {
                let text = text.trim().to_string();
                if !text.is_empty() {
                    self.emit(HudEvent::Transcript {
                        text: text.clone(),
                        language: "text".into(),
                        source: "chat".into(),
                        wake_word: None,
                        stt_ms: 0,
                        audio_ms: 0,
                    });
                    self.dispatch_turn(text, Instant::now(), 0, InputSource::TextGui);
                }
                None
            }
            HudCommand::SetResponseMode { mode } => {
                self.run_ui_tool(
                    "set_response_modality",
                    json!({
                        "mode": mode.id(),
                        "custom_condition": match &mode { crate::modality::VoiceResponseMode::CustomCondition(c) => c.as_str(), _ => "" },
                    }),
                )
                .await;
                Some(HudEvent::Notice { message: format!("Reply mode: {}", mode.display()) })
            }
            HudCommand::WidgetToggleItem { id, index } => {
                self.tools().widgets.toggle_item(&id, index);
                None
            }
            HudCommand::WidgetTimer { id, op } => {
                self.tools().widgets.timer(&id, &op);
                None
            }
            HudCommand::WidgetRefresh { id } => {
                self.tools().widgets.refresh_now(&id);
                None
            }
            HudCommand::WidgetRemove { id } => {
                self.tools().widgets.remove(&id);
                crate::gui::refresh();
                None
            }
            HudCommand::WidgetPopOut { id } => {
                let title = self.tools().widgets.widget(&id).map(|w| w.title).unwrap_or_else(|| id.clone());
                self.run_ui_tool("spawn_native_window", json!({ "title": title, "widget_ids": [id], "width": 440, "height": 340 })).await;
                None
            }
            HudCommand::OpenTextWindow { title, text } => {
                self.run_ui_tool("spawn_native_window", json!({ "title": title, "markdown": text, "width": 520, "height": 380 })).await;
                None
            }
            HudCommand::PowerCard { id, confirm } => {
                // The card's buttons answer the open question like Sir's own "yes" / "no".
                match self.inner.brain.power_card_answer(&id, confirm) {
                    Some(answer) => {
                        self.emit(HudEvent::Transcript {
                            text: answer.to_string(),
                            language: "text".into(),
                            source: "chat".into(),
                            wake_word: None,
                            stt_ms: 0,
                            audio_ms: 0,
                        });
                        self.dispatch_turn(answer.to_string(), Instant::now(), 0, InputSource::TextGui);
                        None
                    }
                    None => Some(HudEvent::PowerCard { id, action: String::new(), label: String::new(), status: "expired".into() }),
                }
            }
            HudCommand::MessageCard { id, send } => {
                let result = crate::whatsapp_client::resolve_card(self.tools(), &id, send).await;
                Some(match result {
                    Ok(message) => HudEvent::Notice { message },
                    Err(message) => HudEvent::Error { message },
                })
            }
            HudCommand::WidgetAction { id, index } => {
                let action = self.tools().widgets.widget(&id).and_then(|w| match w.kind {
                    WidgetKind::QuickActionButtons { buttons } => buttons.get(index).map(|b| b.action.clone()),
                    _ => None,
                });
                if let Some(action) = action {
                    self.run_widget_action(&action).await;
                }
                None
            }
            HudCommand::Interrupt => {
                self.interrupt("stop button");
                None
            }
            HudCommand::SetMic { enabled } => {
                c.mic_enabled.store(enabled, Ordering::Release);
                if !enabled {
                    c.close_window();
                }
                info!("[MIC] Microphone {}", if enabled { "enabled" } else { "muted" });
                self.emit(HudEvent::ConfigState(self.config_state()));
                self.rest_state();
                None
            }
            HudCommand::Ptt { active } => {
                c.ptt_active.store(active, Ordering::Release);
                self.emit(HudEvent::ConfigState(self.config_state()));
                None
            }
            HudCommand::SetWakeWordRequired { required } => {
                c.wake_word_required.store(required, Ordering::Release);
                self.inner.config.update(|cfg| cfg.wake_word_required = required);
                self.emit(HudEvent::ConfigState(self.config_state()));
                self.rest_state();
                None
            }
            HudCommand::SetVad { start_threshold, release_threshold, silence_ms, noise_multiplier } => {
                self.apply_vad(start_threshold, release_threshold, silence_ms, noise_multiplier);
                None
            }
            HudCommand::Recalibrate => {
                c.recalibrate_requested.store(true, Ordering::Release);
                Some(HudEvent::Notice { message: "Recalibrating the noise floor — please stay quiet for a second.".into() })
            }
            HudCommand::AddMemory { key, value, category, importance } => {
                self.run_ui_tool(
                    "remember_fact",
                    json!({ "key": key, "value": value, "category": category, "importance": importance.unwrap_or(7) }),
                )
                .await;
                None
            }
            HudCommand::DeleteMemory { id_or_key } => {
                self.run_ui_tool("delete_memory", json!({ "id_or_key": id_or_key })).await;
                None
            }
            HudCommand::SearchMemories { query } => {
                let mem = &self.tools().memory;
                let items = if query.trim().is_empty() {
                    mem.list(200)
                } else {
                    mem.recall(Some(&query), None, 50)
                }
                .unwrap_or_default();
                Some(HudEvent::Memories { items, query: Some(query) })
            }
            HudCommand::SetModel { model } => {
                let model = model.trim().to_string();
                if !model.is_empty() {
                    info!("[LLM] Primary model set to '{model}'");
                    self.inner.config.update(|cfg| cfg.primary_model = model.clone());
                    self.emit(HudEvent::ConfigState(self.config_state()));
                    // Installs the model first if the built-in engine does not have it yet.
                    let app = self.clone();
                    tokio::spawn(async move {
                        match app.inner.llm.ensure_model(&model).await {
                            Ok(()) => app.inner.brain.request_prime(),
                            Err(e) => app.emit(HudEvent::Error { message: format!("Could not install '{model}': {e}") }),
                        }
                    });
                }
                None
            }
            HudCommand::ListModels => Some(HudEvent::Models {
                items: self.inner.llm.list_models().await,
                current: self.inner.config.read(|c| c.primary_model.clone()),
            }),
            HudCommand::Shutdown => {
                info!("[APP] Shutdown requested from the HUD");
                self.inner.shutdown.cancel();
                None
            }
            HudCommand::SetVoice { voice } => {
                self.inner.tts.set_voice(&voice);
                self.inner.config.update(|cfg| cfg.kokoro_voice = voice.clone());
                self.emit(HudEvent::ConfigState(self.config_state()));
                None
            }
            HudCommand::SetLoudness { volume_pct } => {
                self.inner.tts.set_volume(volume_pct);
                self.inner.config.update(|cfg| cfg.loudness_pct = volume_pct.min(200));
                self.emit(HudEvent::ConfigState(self.config_state()));
                None
            }
            HudCommand::SetMicDevice { device } => {
                c.request_mic(device.clone());
                self.inner.config.update(|cfg| cfg.preferred_mic = device.clone());
                self.emit(HudEvent::ConfigState(self.config_state()));
                None
            }
            HudCommand::SetSpeakerDevice { device } => {
                self.inner.tts.set_output_device(device.clone());
                self.inner.config.update(|cfg| cfg.preferred_speaker = device.clone());
                let app = self.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(600)).await;
                    app.emit(HudEvent::ConfigState(app.config_state()));
                });
                None
            }
            HudCommand::RefreshDevices => Some(Self::devices_event()),
            HudCommand::TestVoice => {
                let pct = self.inner.tts.volume();
                self.speak_standalone(
                    &format!("Good day, Sir. All systems are online, and my voice is set to {pct} percent."),
                    false,
                    Trigger::Forced,
                );
                None
            }
            HudCommand::TestMic => {
                c.mic_test_requested.store(true, Ordering::Release);
                self.set_state("listening", format!("Microphone test on {} — speak now (2.5 s)", c.active_mic()));
                None
            }
            HudCommand::RunSkill { name } => {
                self.run_ui_tool("execute_skill", json!({ "name": name })).await;
                None
            }
            HudCommand::DeleteSkill { name } => {
                self.run_ui_tool("delete_skill", json!({ "name": name })).await;
                None
            }
            HudCommand::ClearHistory => {
                self.inner.brain.clear_history().await;
                Some(HudEvent::Notice { message: "Conversation history cleared.".into() })
            }
        }
    }
}

/// For the addressee detector: (one of JAI's windows is in front, a game / voice chat /
/// full-screen app is in front).
#[cfg(windows)]
async fn foreground_context() -> (bool, bool) {
    crate::win32::call(|| {
        let (process, own) = crate::win32::foreground_info().unwrap_or_default();
        let gaming = !own && (crate::ears::is_game_or_voice_chat(&process) || crate::win32::foreground_is_fullscreen());
        (own, gaming)
    })
    .await
    .unwrap_or((false, false))
}

#[cfg(not(windows))]
async fn foreground_context() -> (bool, bool) {
    (false, false)
}

/// His name opens the sentence ("JAI, show me the world", "Hey JAI, …"): the words are
/// addressed afresh, not the end of something said before.
fn wake_leads(text: &str, word: &str) -> bool {
    let word = word.trim().trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    !word.is_empty()
        && text
            .split_whitespace()
            .take(3)
            .any(|t| t.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase() == word)
}

/// Whether newly heard words replace held, unfinished ones instead of completing them: they are
/// a command on their own and either open with his name (two words or more), or the joined
/// sentence routes nowhere while they do.
fn supersedes(earlier: &str, heard: &str, named_first: bool, routes: &dyn Fn(&str) -> bool) -> bool {
    let words = heard.split_whitespace().count();
    if words == 0 || earlier.trim().is_empty() || !routes(heard) {
        return false;
    }
    (named_first && words >= 2) || !routes(&format!("{} {heard}", earlier.trim()))
}

/// Lowers the system volume (the music in Sir's headphones) to a third and raises JAI's own
/// voice, so the knock alert is heard; the returned future puts both back.
async fn duck_audio(voice_volume: &Arc<std::sync::atomic::AtomicU8>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    let voice = Arc::clone(voice_volume);
    let previous_voice = voice.load(Ordering::Relaxed);
    voice.store(previous_voice.saturating_mul(2).clamp(40, 200), Ordering::Relaxed);
    #[cfg(windows)]
    {
        let previous = crate::win32::call(|| {
            let (level, muted) = crate::win32::get_volume().ok()?;
            if !muted && level > 0.05 {
                crate::win32::set_volume(level * 0.35).ok()?;
            }
            Some(level)
        })
        .await
        .ok()
        .flatten();
        Box::pin(async move {
            voice.store(previous_voice, Ordering::Relaxed);
            if let Some(level) = previous {
                let _ = crate::win32::call(move || crate::win32::set_volume(level)).await;
            }
        })
    }
    #[cfg(not(windows))]
    {
        Box::pin(async move {
            voice.store(previous_voice, Ordering::Relaxed);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stitched_audio_keeps_order_gap_and_the_newest_28_seconds() {
        let joined = join_audio(vec![1.0; 100], vec![2.0; 50]);
        assert_eq!(joined.len(), 100 + STITCH_GAP_SAMPLES + 50);
        assert_eq!((joined[0], joined[100], joined[joined.len() - 1]), (1.0, 0.0, 2.0));
        let long = join_audio(vec![1.0; STITCH_MAX_SAMPLES], vec![2.0; 16_000]);
        assert_eq!(long.len(), STITCH_MAX_SAMPLES);
        assert_eq!(long[long.len() - 1], 2.0, "the newest audio stays");
    }

    /// Repro (jai.log, 2026-11-08 23:3x): "told me the world" was held, and "JAI show me the
    /// world" became "[STITCH] Joined: 'told me the world show me the world'".
    #[test]
    fn a_fresh_command_with_his_name_drops_a_held_fragment() {
        assert!(wake_leads("JAI show me the world.", "JAI"));
        assert!(wake_leads("Hey, JAI, show me the world.", "JAI"));
        assert!(!wake_leads("Show me the world, JAI.", "JAI"));
        let routes = |text: &str| matches!(text, "show me the world" | "open the spotify" | "close it");
        assert!(supersedes("told me the world", "show me the world", true, &routes));
        // Without the name, the newer words still win when the joined sentence routes nowhere.
        assert!(supersedes("told me the world", "show me the world", false, &routes));
        // The rest of a sentence completes it: "open the" … "spotify".
        let completes = |text: &str| text == "open the spotify";
        assert!(!supersedes("open the", "spotify", true, &completes));
        // Words that are no command of their own never replace what was held.
        assert!(!supersedes("open the", "uh the music", true, &routes));
        // A one-word fragment after the name joins, as before.
        let both = |text: &str| matches!(text, "close" | "turn it down close");
        assert!(!supersedes("turn it down", "close", true, &both));
        assert!(!supersedes("", "close it", true, &routes));
    }

    /// Repro (jai.log, 2026-11-13 18:11, a song in the room): "[STITCH] Nothing clear in 1536 ms
    /// on its own; hearing it again with the rest", "[STITCH] Hearing 1536 ms + 4864 ms of
    /// speech as one sentence", and then a turn with no `[WAKE]` line before it. He was idle and
    /// the conversation window had closed a minute before: being stitched alone had made the
    /// sentence a follow-up.
    #[test]
    fn a_stitched_sentence_is_judged_like_any_other() {
        use crate::ears::{classify_addressee, Addressee, AddresseeContext};
        // The first piece: Whisper found no words in it while the song went on, so it was held
        // before anything was known of it. It began outside any conversation, without the
        // push-to-talk key, and not over his voice.
        let held = held_for_him(false, None, false, false, false);
        assert!(!held, "nothing made the held piece his to hear");
        // The sentence it was stitched into: the window closed, no turn running, no words held.
        let open = in_conversation(false, false, Some(held), false, false);
        assert!(!open, "being stitched opens no conversation");
        let idle = |conversation_open: bool, jai_focused: bool| AddresseeContext { wake_word: false, wake_required: true, conversation_open, jai_focused, gaming: false };
        // A line of that song which no layer takes for his: ignored, stitched or in one piece.
        let line = "The river keeps the count of all my wanderings too.";
        assert_eq!(classify_addressee(line, &idle(open, false)), Addressee::Unaddressed);
        // A line that opens like a question: judged by its own words, as it is unstitched, with his
        // window in front or not. (It opens with "When", which layers B and C take for a
        // question: that is theirs to judge, and no longer the stitching's to wave through.)
        let lyric = "When apart, how I passed my days just mending the nets of the morning.";
        for focused in [false, true] {
            let judged = classify_addressee(lyric, &idle(open, focused));
            assert_ne!(judged, Addressee::Conversation, "no follow-up: nobody was talking to him");
            assert_eq!(judged, classify_addressee(lyric, &idle(false, focused)), "as the same words are judged in one piece");
        }
        // What was his to hear stays his when the sentence goes on: a turn that was pulled
        // back, his name with a breath after it, a first half that was accepted.
        assert!(in_conversation(false, false, Some(held_for_him(true, None, false, false, false)), false, false));
        // A piece held unjudged that began inside a conversation, with the key held, or over
        // his voice.
        for (window, ptt, barge_in) in [(true, false, false), (false, true, false), (false, false, true)] {
            assert!(held_for_him(false, None, window, ptt, barge_in));
        }
        // Two pauses in one sentence: his to hear if an earlier part was.
        assert!(held_for_him(false, Some(true), false, false, false));
        assert!(!held_for_him(false, Some(false), false, false, false));
        // And as ever: the window, a running turn, words held from before.
        assert!(in_conversation(true, false, None, false, false));
        assert!(in_conversation(false, true, None, false, false));
        assert!(in_conversation(false, false, None, true, false));
        assert!(in_conversation(false, false, None, false, true));
        assert!(!in_conversation(false, false, None, false, false));
    }

    /// A "don't listen" with an end is the dispatcher's own, like the plain one: admitted before
    /// any turn exists, in English and German. The question about it is a turn (it is answered,
    /// and lifts nothing).
    #[test]
    fn the_dispatcher_admits_a_lock_with_an_end() {
        use crate::listening_lock::LockCommand;
        let pause = |secs: u64| Admission::Privacy(LockCommand::Pause(Duration::from_secs(secs)));
        for (said, want) in [
            ("Don't listen for ten minutes", pause(600)),
            ("JAI, stop listening for an hour.", pause(3600)),
            ("Hör zehn Minuten nicht zu", pause(600)),
            ("Mic off for 20 minutes", pause(1200)),
            ("Jay, don't listen.", Admission::Privacy(LockCommand::Lock)),
            ("You can listen again", Admission::Privacy(LockCommand::Resume)),
        ] {
            assert_eq!(Jai::admit(said), want, "{said}");
        }
        for said in ["Don't listen until eight", "Hör bis acht nicht zu", "Mic off until 8 pm"] {
            assert!(matches!(Jai::admit(said), Admission::Privacy(LockCommand::Until(_, 0))), "{said}: {:?}", Jai::admit(said));
        }
        for said in ["How long until you listen again?", "When will you listen again?", "Wann hörst du wieder zu?", "Is my mic off?", "Hör dem Lehrer nicht zu"] {
            assert_eq!(Jai::admit(said), Admission::Turn, "{said}");
        }
    }

    /// The real router: the log's newer words are one of his commands on their own.
    #[test]
    fn the_log_phrase_routes_on_its_own() {
        assert!(crate::command_corrector::routes("show me the world"));
        assert!(supersedes("told me the world", "show me the world", true, &crate::command_corrector::routes));
    }
}
