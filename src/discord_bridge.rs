//! Jarvis in Sir's Discord call — only when Sir asks for it, and without touching Elgato Wave Link.
//!
//! * **Listen** ("listen to my Discord call", "hör im Discord mit"): Jarvis records what Discord
//!   *plays* — the output device its audio session is on (with Wave Link, the "Voice chat"
//!   channel), as a WASAPI loopback (`cpal` input stream on an output device). That is only the
//!   other people in the call: Discord never plays Sir's own voice back, and Sir's microphone is
//!   not touched. Speech is cut by a call VAD (`CallVad`), transcribed by the same Whisper worker,
//!   kept for 30 minutes in memory only (never written to disk) and given to the model as live
//!   context, so "what did they just say?" works.
//! * **Converse** ("you can talk to them", "du darfst mit ihnen reden"): a second copy of Jarvis'
//!   voice plays on an output that reaches Discord's microphone — with Wave Link a channel in the
//!   Chat Mix, else a virtual cable — while Sir keeps hearing him on his own speaker (the TTS dual
//!   sink, resampled to that device); a friend who says "Jarvis, …" gets a short spoken answer. Guests only ever get
//!   conversation: their words never run a tool (no "shut down Sir's PC" from the call).
//!   The output is `discord_call_output` in the config (the HUD's `DISCORD // AUDIO ROUTING`
//!   injection target); when it is unset Jarvis finds it once with
//!   a silent probe (an 18.5 kHz tone at −34 dBFS for 0.4 s on each Wave Link channel, listening for
//!   it on the Chat Mix) and keeps it. Jarvis never changes Wave Link's routing, levels or devices.
//! * **Off** ("stop listening to the call"): the loopback closes and his voice goes back to the
//!   normal output. Every Jarvis start begins with Off.

use crate::crash_guard::lock;
use crate::stt::SttEngine;
use crate::tools::ToolContext;
use crate::tts::TtsController;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

pub const TOOL: &str = "discord_call";
/// Lines kept for the live context and "what did they say".
const KEEP_FOR: Duration = Duration::from_secs(30 * 60);
const KEEP_LINES: usize = 60;
/// The Wave Link channel Discord itself plays to (never Jarvis' talk channel).
const DISCORD_OWN_CHANNEL: &str = "voice chat";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallMode {
    Off = 0,
    Listen = 1,
    Converse = 2,
}

impl CallMode {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Listen,
            2 => Self::Converse,
            _ => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Listen => "listening",
            Self::Converse => "in the call",
        }
    }
}

static MODE: AtomicU8 = AtomicU8::new(0);
/// The capture thread runs (it exits when the mode goes Off).
static CAPTURING: AtomicBool = AtomicBool::new(false);
/// A guest's question is being answered (one at a time).
static ANSWERING: AtomicBool = AtomicBool::new(false);
static HEARD: Mutex<VecDeque<(Instant, String, String)>> = Mutex::new(VecDeque::new());

struct Handles {
    stt: SttEngine,
    tts: TtsController,
    tools: ToolContext,
    rt: tokio::runtime::Handle,
}

static HANDLES: OnceLock<Handles> = OnceLock::new();

/// Gives the bridge Whisper, the voice and the tools (once, at start-up).
pub fn install(stt: SttEngine, tts: TtsController, tools: ToolContext) {
    let rt = tokio::runtime::Handle::current();
    let _ = HANDLES.set(Handles { stt, tts, tools, rt });
}

pub fn mode() -> CallMode {
    CallMode::from_u8(MODE.load(Ordering::Acquire))
}

/// The bottom strip's badge while the bridge is on ("CALL · LISTENING").
pub fn status_label() -> Option<&'static str> {
    match mode() {
        CallMode::Off => None,
        CallMode::Listen => Some("CALL · LISTENING"),
        CallMode::Converse => Some("CALL · TALKING"),
    }
}

pub fn is_tool(name: &str) -> bool {
    name == TOOL
}

pub fn definitions() -> Vec<Value> {
    vec![json!({"type": "function", "function": {
        "name": TOOL,
        "description": "Sir's Discord voice call. action listen: hear the others in the call (transcribed, kept 30 min in memory). converse: also speak into the call and answer friends who address you (chat only, never tools). say: speak Sir's `message` into the call (\"tell the guys in Discord …\"; turns converse on). quiet: stop speaking in the call but keep listening. off: stop. recap: what the others said recently. status. test: speak a test line into the call (and on Sir's speaker). Only when Sir asks for it.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["listen", "converse", "say", "quiet", "off", "recap", "status", "test"]},
            "message": {"type": "string", "description": "say: the words to speak into the call, in Sir's language"}
        }, "required": ["action"]}
    }})]
}

fn spoken(display: impl AsRef<str>, line: impl AsRef<str>) -> String {
    format!("{}\nSpoken: {}", display.as_ref(), line.as_ref())
}

/// The words of a `say` request (None when there are none).
fn say_line(args: &Value) -> Option<String> {
    args["message"].as_str().map(str::trim).filter(|m| !m.is_empty()).map(String::from)
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let action = args["action"].as_str().unwrap_or("status").to_lowercase();
    match action.as_str() {
        "listen" | "quiet" => {
            let was = mode();
            set_mode(CallMode::Listen, tools).await?;
            let line = if was == CallMode::Converse { "I'll keep quiet in the call, Sir, but I'm still listening." } else { "I'm listening to your call, Sir. Only the others, never your microphone." };
            Ok(spoken(format!("Discord call: listening on {}", source_device().await.unwrap_or_else(|| "?".into())), line))
        }
        "converse" | "talk" => {
            let device = set_mode(CallMode::Converse, tools).await?;
            Ok(spoken(format!("Discord call: talking through {device}"), "Very good, Sir. I can hear them, and they can hear me. Anyone who says my name gets an answer."))
        }
        "say" => {
            let Some(message) = say_line(args) else {
                return Ok(spoken("Discord call: nothing to say yet", "What shall I tell them, Sir?"));
            };
            // The turn's voice goes out through the call's talk channel (and Sir's own mix).
            let device = set_mode(CallMode::Converse, tools).await?;
            Ok(spoken(format!("Discord call: saying it through {device} (now in the call)"), message))
        }
        "off" | "stop" => {
            set_mode(CallMode::Off, tools).await?;
            Ok(spoken("Discord call: off", "I've left the call, Sir. Nothing more is being heard."))
        }
        "test" => {
            let handles = HANDLES.get().ok_or("The call bridge isn't ready yet, Sir.")?;
            let device = talk_device(tools).await?;
            handles.tts.set_mirror_device(Some(device.clone()));
            let generation = handles.tts.generation();
            handles.tts.speak(TEST_LINE, generation);
            handles.tts.finish(generation);
            // Outside a conversation the call hears him for the test only.
            if mode() != CallMode::Converse {
                let tts = handles.tts.clone();
                handles.rt.spawn(async move {
                    tokio::time::sleep(Duration::from_secs(8)).await;
                    if mode() != CallMode::Converse {
                        tts.set_mirror_device(None);
                    }
                });
            }
            Ok(format!("Discord call: test line playing on {device} and your speaker"))
        }
        "recap" => {
            let lines = recent(Duration::from_secs(10 * 60));
            if lines.is_empty() {
                return Ok(spoken("Nothing heard in the call yet.", if mode() == CallMode::Off { "I'm not listening to the call, Sir. Say \"listen to my Discord call\" if you'd like me to." } else { "Nobody has said anything I could make out yet, Sir." }));
            }
            let last: Vec<String> = lines.iter().rev().take(4).rev().map(|(_, t)| t.clone()).collect();
            Ok(spoken(lines.iter().map(|(at, t)| format!("[{at}] {t}")).collect::<Vec<_>>().join("\n"), format!("Most recently: {}", last.join(" … "))))
        }
        _ => Ok(spoken(format!("Discord call: {}", mode().label()), format!("The call bridge is {}, Sir.", mode().label()))),
    }
}

/// The output that reaches the call: the injection target Sir picked, else Wave Link's routing,
/// else the silent probe (kept in the config).
async fn talk_device(tools: &ToolContext) -> Result<String, String> {
    if let Some(d) = tools.config.read(|c| c.discord_call_output.clone()).filter(|d| !d.trim().is_empty()) {
        return Ok(d);
    }
    // Wave Link's own routing table: no tone needed, nothing saved (it follows whatever Sir
    // changes in Wave Link).
    if let Some(d) = wave_link_talk_device().await {
        return Ok(d);
    }
    let mic = discord_mic().await.ok_or("I can't find Wave Link's Chat Mix, Sir — is Wave Link running?")?;
    let found = tokio::task::spawn_blocking(move || probe_chat_channel(&mic)).await.map_err(|e| e.to_string())?.ok_or(NO_TALK_CHANNEL)?;
    let keep = found.clone();
    tools.config.update(|c| c.discord_call_output = Some(keep));
    Ok(found)
}

/// The HUD's injection target: where the copy of his voice goes (None: found automatically). In
/// a conversation it switches at once.
pub fn set_injection_target(config: &crate::config::ConfigStore, device: Option<String>) {
    config.update(|c| c.discord_call_output = device.clone());
    if let (Some(handles), Some(d), CallMode::Converse) = (HANDLES.get(), device, mode()) {
        handles.tts.set_mirror_device(Some(d));
    }
}

/// The HUD's listen source: the output whose sound is the call (None: found automatically);
/// used the next time the bridge starts listening.
pub fn set_listen_source(config: &crate::config::ConfigStore, device: Option<String>) {
    config.update(|c| c.discord_listen_source = device);
}

/// The device the call hears him on right now ("—" when nothing goes in).
pub fn voice_device() -> String {
    HANDLES.get().map(|h| h.tts.mirror_device()).unwrap_or_else(|| "—".into())
}

/// The HUD's `[ TEST VOICE IN DISCORD ]` line.
pub const TEST_LINE: &str = "Testing Stark audio bridge on Discord channel.";

/// Switches the bridge; returns the talk device in Converse.
async fn set_mode(next: CallMode, tools: &ToolContext) -> Result<String, String> {
    let handles = HANDLES.get().ok_or("The call bridge isn't ready yet, Sir.")?;
    let mut talk = String::new();
    match next {
        CallMode::Converse => {
            talk = talk_device(tools).await?;
            // A second sink: the call hears him and Sir's own speaker keeps playing.
            handles.tts.set_mirror_device(Some(talk.clone()));
            info!("[DISCORD] Talking in the call through '{talk}'");
        }
        _ => {
            if mode() == CallMode::Converse {
                handles.tts.set_mirror_device(None);
            }
        }
    }
    MODE.store(next as u8, Ordering::Release);
    if next == CallMode::Off {
        lock(&HEARD).clear();
        return Ok(talk);
    }
    if !CAPTURING.swap(true, Ordering::AcqRel) {
        let device = match tools.config.read(|c| c.discord_listen_source.clone()).filter(|d| !d.trim().is_empty()) {
            Some(d) => Some(d),
            None => source_device().await,
        };
        std::thread::Builder::new()
            .name("jarvis-discord".into())
            .spawn(move || {
                crate::crash_guard::supervise("jarvis-discord", || capture_loop(device.clone()));
                CAPTURING.store(false, Ordering::Release);
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(talk)
}

/// The microphone Discord records from: its capture session while it is in a call, else
/// Wave Link's Chat Mix (what Discord is normally set to with Wave Link).
pub async fn discord_mic() -> Option<String> {
    let live = tokio::time::timeout(Duration::from_secs(4), crate::audio_mixer::app_input_device("Discord")).await.ok().flatten();
    live.or_else(chat_mix_name)
}

fn chat_mix_name() -> Option<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    cpal::default_host().input_devices().ok()?.find_map(|d| d.name().ok().filter(|n| n.to_lowercase().contains("chat mix")))
}

/// The output that reaches the call according to Wave Link's routing (read-only): a channel in
/// the Chat Mix that carries no other app. Logs channels whose apps reach the call as well.
async fn wave_link_talk_device() -> Option<String> {
    let channels = crate::wave_link::channels().await?;
    for c in crate::wave_link::leaking(&channels) {
        warn!("[DISCORD] Wave Link also sends '{}' ({}) into the Chat Mix", c.name, c.apps.join(", "));
    }
    let c = crate::wave_link::talk_channel(&channels)?;
    let device = crate::wave_link::device_of(&c.name, &crate::tts::list_output_devices())?;
    info!("[DISCORD] Wave Link routes '{}' into the Chat Mix", c.name);
    Some(device)
}

/// What to tell Sir when no channel reaches the call.
const NO_TALK_CHANNEL: &str = "Nothing I play reaches your Discord microphone yet, Sir: no Wave Link channel is turned up in the Chat Mix. Add a spare channel in Wave Link (SFX or Aux, say), turn it up in the Chat Mix and in your Personal Mix, then ask me again. I won't touch Wave Link myself.";

/// The output device Discord plays to: Wave Link's channel for the Discord app, else Discord's
/// audio session, else a "Voice chat" device. Wave Link comes first because Discord also keeps a
/// session on the default output (live 2026-11-03: the session lookup picked "System").
async fn source_device() -> Option<String> {
    if let Some(channels) = crate::wave_link::channels().await {
        if let Some(c) = crate::wave_link::channel_of_app(&channels, "Discord") {
            if let Some(d) = crate::wave_link::device_of(&c.name, &crate::tts::list_output_devices()) {
                return Some(d);
            }
        }
    }
    if let Some(d) = crate::audio_mixer::app_output_device("Discord").await {
        return Some(d);
    }
    crate::tts::list_output_devices().into_iter().find(|n| n.to_lowercase().contains(DISCORD_OWN_CHANNEL))
}

/// Lines heard within `window`: ("HH:MM:SS", text).
pub fn recent(window: Duration) -> Vec<(String, String)> {
    let now = Instant::now();
    lock(&HEARD).iter().filter(|(at, _, _)| now.duration_since(*at) <= window).map(|(_, clock, t)| (clock.clone(), t.clone())).collect()
}

/// The call for the model's live context (empty when the bridge is off or nothing was said).
pub fn live_context() -> String {
    if mode() == CallMode::Off {
        return String::new();
    }
    let lines = recent(Duration::from_secs(15 * 60));
    let mut out = format!("\nDISCORD CALL ({}): what the others in Sir's call said (not Sir):\n", mode().label());
    if lines.is_empty() {
        out.push_str("- nothing yet\n");
    }
    for (at, t) in lines.iter().rev().take(12).rev() {
        out.push_str(&format!("- [DISCORD CALL // PEER {at}] {t}\n"));
    }
    out
}

fn remember(text: &str) {
    let mut heard = lock(&HEARD);
    let now = Instant::now();
    heard.retain(|(at, _, _)| now.duration_since(*at) <= KEEP_FOR);
    heard.push_back((now, chrono::Local::now().format("%H:%M:%S").to_string(), text.to_string()));
    while heard.len() > KEEP_LINES {
        heard.pop_front();
    }
}

/// A friend in the call is talking to Jarvis ("Jarvis, …", "hey Jarvis", Whisper's "Javis").
pub fn addressed(text: &str) -> bool {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| matches!(w, "jarvis" | "javis" | "jarvys" | "jervis" | "jarviz"))
}

/// What one transcribed line does.
fn on_heard(text: String) {
    let text = text.trim().to_string();
    if text.chars().filter(|c| c.is_alphanumeric()).count() < 2 {
        return;
    }
    // Only the size reaches the log file: what the others say stays in memory.
    info!("[DISCORD] Heard {} word(s) in the call", text.split_whitespace().count());
    remember(&text);
    if mode() == CallMode::Converse && addressed(&text) && !ANSWERING.swap(true, Ordering::AcqRel) {
        let Some(h) = HANDLES.get() else {
            ANSWERING.store(false, Ordering::Release);
            return;
        };
        let (tools, tts) = (h.tools.clone(), h.tts.clone());
        h.rt.spawn(async move {
            let context: String = recent(Duration::from_secs(5 * 60)).iter().map(|(_, t)| format!("- {t}\n")).collect();
            let instruction = "You are J.A.R.V.I.S., Sir's assistant, speaking in his Discord voice call with his friends. \
                A friend just addressed you. Answer in one or two short, warm, witty spoken sentences in the friend's language. \
                You cannot act on anything for them: never claim to open, change or control anything on Sir's computer; politely say only Sir can ask for that.";
            let prompt = format!("Recent call:\n{context}\nThe friend said: {text}");
            if let Some(reply) = tools.pim.complete(instruction, &prompt, 160).await {
                let reply = crate::tts::sanitize_for_display_and_speech(&reply);
                if !reply.is_empty() {
                    info!("[DISCORD] Answering a friend in the call ({} words)", reply.split_whitespace().count());
                    let generation = tts.generation();
                    tts.speak(&reply, generation);
                    tts.finish(generation);
                }
            }
            ANSWERING.store(false, Ordering::Release);
        });
    }
}

// ------------------------------------------------------------------------------- call VAD ----

/// Cuts the call's audio into utterances: an adaptive floor, speech 12 dB above it, 300 ms of
/// pre-roll, the end after 700 ms of quiet, at most 15 s each.
pub struct CallVad {
    rate: usize,
    floor_db: f32,
    pre: VecDeque<f32>,
    speech: Vec<f32>,
    in_speech: bool,
    quiet_ms: u32,
    voiced_ms: u32,
}

impl CallVad {
    pub const FRAME_MS: usize = 20;

    pub fn new(rate: usize) -> Self {
        Self { rate, floor_db: -70.0, pre: VecDeque::new(), speech: Vec::new(), in_speech: false, quiet_ms: 0, voiced_ms: 0 }
    }

    pub fn frame_len(&self) -> usize {
        self.rate * Self::FRAME_MS / 1000
    }

    /// One frame (mono, `frame_len` samples); returns a finished utterance and its voiced ms.
    pub fn push_frame(&mut self, frame: &[f32]) -> Option<(Vec<f32>, u32)> {
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32).sqrt();
        let db = if rms > 1e-7 { 20.0 * rms.log10() } else { -120.0 };
        let ms = Self::FRAME_MS as u32;
        if !self.in_speech {
            // The floor follows quiet quickly and noise slowly.
            self.floor_db = if db < self.floor_db { db.max(-90.0) } else { (self.floor_db + 0.05).min(-35.0) };
            self.pre.extend(frame.iter().copied());
            let keep = self.rate * 300 / 1000;
            while self.pre.len() > keep {
                self.pre.pop_front();
            }
            if db > self.floor_db + 12.0 && db > -55.0 {
                self.in_speech = true;
                self.speech = self.pre.drain(..).collect();
                self.voiced_ms = ms;
                self.quiet_ms = 0;
            }
            return None;
        }
        self.speech.extend_from_slice(frame);
        if db > self.floor_db + 8.0 {
            self.voiced_ms += ms;
            self.quiet_ms = 0;
        } else {
            self.quiet_ms += ms;
        }
        let long = self.speech.len() >= self.rate * 15;
        if self.quiet_ms >= 700 || long {
            self.in_speech = false;
            let clip = std::mem::take(&mut self.speech);
            let voiced = std::mem::replace(&mut self.voiced_ms, 0);
            return (voiced >= 300).then_some((clip, voiced));
        }
        None
    }
}

// ------------------------------------------------------------------------------- capture ----

/// Jarvis never hears himself through the call: while he speaks into it, and for a 250 ms
/// acoustic tail after, the call's audio is dropped (a friend's open mic would echo him back).
#[derive(Default)]
pub struct AntiLoopbackGate {
    last_speech: Option<Instant>,
}

impl AntiLoopbackGate {
    pub const TAIL: Duration = Duration::from_millis(250);

    /// Whether call audio at `now` may reach Whisper (`speaking`: his voice is playing).
    pub fn admit(&mut self, speaking: bool, now: Instant) -> bool {
        if speaking {
            self.last_speech = Some(now);
            return false;
        }
        self.last_speech.is_none_or(|at| now.saturating_duration_since(at) >= Self::TAIL)
    }
}

fn to_mono(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    data.chunks(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect()
}

/// Loopback-records `device` until the mode goes Off; each utterance goes to Whisper.
fn capture_loop(device: Option<String>) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let Some(name) = device else {
        warn!("[DISCORD] Discord has no audio output right now; I'll retry while the bridge is on");
        std::thread::sleep(Duration::from_secs(3));
        return;
    };
    let host = cpal::default_host();
    let Some(dev) = host.output_devices().ok().and_then(|mut d| d.find(|x| x.name().is_ok_and(|n| n == name))) else {
        warn!("[DISCORD] Output device '{name}' not found");
        return;
    };
    let Ok(config) = dev.default_output_config() else { return };
    let (rate, channels) = (config.sample_rate().0 as usize, config.channels() as usize);
    let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
    let err = |e: cpal::StreamError| warn!("[DISCORD] Capture: {e}");
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => dev.build_input_stream(&config.config(), move |d: &[f32], _| { let _ = tx.send(to_mono(d, channels)); }, err, None),
        cpal::SampleFormat::I16 => dev.build_input_stream(&config.config(), move |d: &[i16], _| {
            let f: Vec<f32> = d.iter().map(|s| *s as f32 / 32768.0).collect();
            let _ = tx.send(to_mono(&f, channels));
        }, err, None),
        other => {
            warn!("[DISCORD] Unsupported sample format {other:?}");
            return;
        }
    };
    let Ok(stream) = stream else {
        warn!("[DISCORD] Could not open a loopback on '{name}'");
        return;
    };
    if stream.play().is_err() {
        return;
    }
    info!("[DISCORD] Listening to the call on '{name}' ({rate} Hz, {channels} ch, loopback)");
    let mut vad = CallVad::new(rate);
    let frame = vad.frame_len();
    let mut pending: Vec<f32> = Vec::new();
    let mut gate = AntiLoopbackGate::default();
    while mode() != CallMode::Off {
        let Ok(chunk) = rx.recv_timeout(Duration::from_millis(250)) else { continue };
        // Only in converse does his voice reach the call; listening, nothing of the others is lost.
        let speaking = mode() == CallMode::Converse && HANDLES.get().is_some_and(|h| h.tts.is_speaking());
        if !gate.admit(speaking, Instant::now()) {
            pending.clear();
            vad = CallVad::new(rate);
            continue;
        }
        pending.extend(chunk);
        while pending.len() >= frame {
            let f: Vec<f32> = pending.drain(..frame).collect();
            if let Some((clip, voiced)) = vad.push_frame(&f) {
                let pcm = crate::dsp::resample(&clip, rate as u32, 16_000);
                if let Some(h) = HANDLES.get() {
                    let stt = h.stt.clone();
                    h.rt.spawn(async move {
                        if let Ok(t) = stt.transcribe(pcm, voiced).await {
                            on_heard(t.text);
                        }
                    });
                }
            }
        }
    }
    drop(stream);
    info!("[DISCORD] Stopped listening to the call");
}

// ------------------------------------------------------------------- the talk-channel probe ----

const PROBE_HZ: f32 = 18_500.0;

/// Goertzel power of `freq` in `x` at `rate`.
pub fn tone_power(x: &[f32], freq: f32, rate: f32) -> f32 {
    let w = 2.0 * std::f32::consts::PI * freq / rate;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for v in x {
        let s0 = v + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2) / (x.len().max(1) as f32).powi(2)
}

/// The probe tone reached the capture: 18.5 kHz stands 10 dB above a neighbouring frequency.
pub fn probe_heard(captured: &[f32], rate: f32) -> bool {
    let on = tone_power(captured, PROBE_HZ, rate);
    let off = tone_power(captured, PROBE_HZ - 2_500.0, rate).max(1e-14);
    on > 1e-10 && on / off > 10.0
}

/// How far (dB) the probe tone stands above a neighbouring frequency in `captured`.
pub fn probe_margin_db(captured: &[f32], rate: f32) -> f32 {
    let on = tone_power(captured, PROBE_HZ, rate).max(1e-14);
    let off = tone_power(captured, PROBE_HZ - 2_500.0, rate).max(1e-14);
    10.0 * (on / off).log10()
}

/// Which Wave Link outputs to try for Jarvis' call voice, best first (never Discord's own
/// channel): a channel named for Jarvis or a spare one (SFX, Aux) carries nothing else, then
/// Music and Browser before System, which usually carries everything else; the mic's own
/// endpoint last.
pub fn probe_candidates(outputs: &[String], discord_output: Option<&str>) -> Vec<String> {
    let rank = |n: &str| {
        let l = n.to_lowercase();
        let spare = ["jarvis", "sfx", "aux"].iter().any(|w| l.contains(w));
        if spare { 0 } else if l.contains("music") { 1 } else if l.contains("browser") { 2 } else if l.contains("system") { 3 } else if l.contains("microphone") { 5 } else { 4 }
    };
    let mut out: Vec<String> = outputs
        .iter()
        .filter(|n| n.to_lowercase().contains("elgato virtual audio"))
        .filter(|n| !n.to_lowercase().contains(DISCORD_OWN_CHANNEL) && Some(n.as_str()) != discord_output)
        .cloned()
        .collect();
    out.sort_by_key(|n| rank(n));
    out
}

/// Finds the Wave Link channel that reaches Discord's microphone `mic` (normally the Chat Mix):
/// a short, inaudible 18.5 kHz tone on each candidate while `mic` is recorded. Read-only for
/// Wave Link: nothing is re-routed.
fn probe_chat_channel(mic: &str) -> Option<String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let chat = host.input_devices().ok()?.find(|d| d.name().is_ok_and(|n| n == mic))?;
    let in_cfg = chat.default_input_config().ok()?;
    if in_cfg.sample_format() != cpal::SampleFormat::F32 {
        return None;
    }
    // One recording for every candidate, warmed up first: the first capture of a Wave Link mix
    // in a process can start late (live 2026-11-03: the tone reached the Chat Mix, a fresh
    // recording in Jarvis saw nothing), so a tone played right at the start would be missed.
    let captured = Arc::new(Mutex::new(Vec::<f32>::new()));
    let sink = Arc::clone(&captured);
    let in_ch = in_cfg.channels() as usize;
    let rec = chat.build_input_stream(&in_cfg.config(), move |d: &[f32], _| lock(&sink).extend(to_mono(d, in_ch)), |_| {}, None).ok()?;
    rec.play().ok()?;
    let warm = Instant::now();
    while lock(&captured).is_empty() && warm.elapsed() < Duration::from_millis(1500) {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(250));
    let in_rate = in_cfg.sample_rate().0 as f32;
    let outputs: Vec<String> = crate::tts::list_output_devices();
    for name in probe_candidates(&outputs, None) {
        let Some(out_dev) = host.output_devices().ok().and_then(|mut d| d.find(|x| x.name().is_ok_and(|n| n == name))) else { continue };
        let Ok(out_cfg) = out_dev.default_output_config() else { continue };
        if out_cfg.sample_format() != cpal::SampleFormat::F32 {
            continue;
        }
        let (rate, out_ch) = (out_cfg.sample_rate().0 as f32, out_cfg.channels() as usize);
        let mut k = 0usize;
        let Ok(play) = out_dev.build_output_stream(&out_cfg.config(), move |d: &mut [f32], _| {
            for frame in d.chunks_mut(out_ch) {
                let v = 0.02 * (2.0 * std::f32::consts::PI * PROBE_HZ * k as f32 / rate).sin();
                frame.iter_mut().for_each(|s| *s = v);
                k += 1;
            }
        }, |_| {}, None) else { continue };
        let from = lock(&captured).len();
        if play.play().is_err() {
            continue;
        }
        std::thread::sleep(Duration::from_millis(400));
        drop(play);
        std::thread::sleep(Duration::from_millis(150));
        let (heard, margin, n) = {
            let got = lock(&captured);
            let part = &got[from.min(got.len())..];
            (probe_heard(part, in_rate), probe_margin_db(part, in_rate), part.len())
        };
        info!("[DISCORD] Probe on '{name}': {} (tone {margin:+.1} dB over its neighbour, {} ms recorded)", if heard { "reaches the Chat Mix" } else { "not in the Chat Mix" }, n as f32 / in_rate * 1000.0);
        if heard {
            return Some(name);
        }
    }
    None
}

// ---------------------------------------------------------------------------- call check ----

/// 20 ms RMS envelope of `x` at `rate`.
pub fn envelope(x: &[f32], rate: usize) -> Vec<f32> {
    let hop = (rate / 50).max(1);
    x.chunks(hop).map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt()).collect()
}

/// The best Pearson correlation of `played` against `heard` (envelopes) for lags 0..`max_lag`
/// frames: how surely the captured audio is the played voice (and not Sir talking over it).
pub fn best_correlation(played: &[f32], heard: &[f32], max_lag: usize) -> f32 {
    let mut best = -1.0f32;
    for lag in 0..=max_lag {
        let n = played.len().min(heard.len().saturating_sub(lag));
        if n < 10 {
            break;
        }
        let (a, b) = (&played[..n], &heard[lag..lag + n]);
        let (ma, mb) = (a.iter().sum::<f32>() / n as f32, b.iter().sum::<f32>() / n as f32);
        let (mut cov, mut va, mut vb) = (0.0f32, 0.0f32, 0.0f32);
        for (x, y) in a.iter().zip(b) {
            cov += (x - ma) * (y - mb);
            va += (x - ma) * (x - ma);
            vb += (y - mb) * (y - mb);
        }
        if va > 1e-12 && vb > 1e-12 {
            best = best.max(cov / (va.sqrt() * vb.sqrt()));
        }
    }
    best
}

/// RMS level (dBFS) of one second of the input `name`.
fn room_level(name: &str) -> Option<f32> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let dev = host.input_devices().ok()?.find(|d| d.name().is_ok_and(|n| n == name))?;
    let cfg = dev.default_input_config().ok()?;
    if cfg.sample_format() != cpal::SampleFormat::F32 {
        return None;
    }
    let got = Arc::new(Mutex::new(Vec::<f32>::new()));
    let sink = Arc::clone(&got);
    let ch = cfg.channels() as usize;
    let rec = dev.build_input_stream(&cfg.config(), move |d: &[f32], _| lock(&sink).extend(to_mono(d, ch)), |_| {}, None).ok()?;
    rec.play().ok()?;
    std::thread::sleep(Duration::from_secs(1));
    drop(rec);
    let x = lock(&got);
    let rms = (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt().max(1e-7);
    Some(20.0 * rms.log10())
}

/// `JARVIS.exe --call-check`: where Discord plays, which Wave Link channel reaches the Chat Mix
/// (the silent probe), and whether Jarvis' voice really arrives there — a short sentence played on
/// that channel while the Chat Mix is recorded. Nothing in Wave Link is changed. The sentence is
/// audible (headset, and the call if one is running).
pub async fn call_check(cfg: &crate::config::JarvisConfig) -> std::process::ExitCode {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::process::ExitCode;
    let discord = tokio::time::timeout(Duration::from_secs(6), source_device()).await.ok().flatten();
    info!("[CALL CHECK] Discord plays to: {}", discord.as_deref().unwrap_or("no audio session (Discord closed or silent since start)"));
    let host = cpal::default_host();
    let live_mic = tokio::time::timeout(Duration::from_secs(4), crate::audio_mixer::app_input_device("Discord")).await.ok().flatten();
    let Some(chat_name) = live_mic.clone().or_else(chat_mix_name) else {
        info!("[CALL CHECK] No 'Chat Mix' input: Wave Link's chat mix is what Discord's microphone should be.");
        return ExitCode::FAILURE;
    };
    match &live_mic {
        Some(m) => info!("[CALL CHECK] Discord records from: {m}"),
        None => info!("[CALL CHECK] Discord records from: no capture session (not in a call); assuming {chat_name}"),
    }
    {
        let level = tokio::task::spawn_blocking({
            let chat_name = chat_name.clone();
            move || room_level(&chat_name)
        })
        .await
        .ok()
        .flatten();
        match level {
            Some(db) => info!("[CALL CHECK] Chat Mix right now: {db:.1} dBFS{}", if db < -90.0 { " (digital silence: muted, gated, or nothing routed)" } else { "" }),
            None => info!("[CALL CHECK] Chat Mix could not be recorded"),
        }
    }
    let wave = wave_link_talk_device().await;
    let channel = match (cfg.discord_call_output.clone().filter(|d| !d.trim().is_empty()), wave) {
        (Some(d), _) => {
            info!("[CALL CHECK] Configured talk channel: {d}");
            d
        }
        (None, Some(d)) => {
            info!("[CALL CHECK] Wave Link's routing: '{d}' reaches the Chat Mix");
            d
        }
        (None, None) => match tokio::task::spawn_blocking({
            let mic = chat_name.clone();
            move || probe_chat_channel(&mic)
        })
        .await
        .ok()
        .flatten()
        {
            Some(d) => {
                info!("[CALL CHECK] Probe: '{d}' reaches the Chat Mix");
                d
            }
            None => {
                // The tone may be filtered rather than unrouted: the voice itself decides.
                let Some(first) = probe_candidates(&crate::tts::list_output_devices(), None).into_iter().next() else {
                    info!("[CALL CHECK] No Wave Link output channels found.");
                    return ExitCode::FAILURE;
                };
                info!("[CALL CHECK] Probe: no channel's tone reached '{chat_name}'; trying the voice itself on '{first}'");
                first
            }
        },
    };
    // The voice: one short sentence through Kokoro.
    let phonemizer = crate::phonemizer::Phonemizer::new(cfg.espeak_path.as_deref());
    let mut kokoro = match crate::tts::Kokoro::load(cfg) {
        Ok(k) => k,
        Err(e) => {
            info!("[CALL CHECK] Voice unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let ipa = phonemizer.phonemize(&crate::tts::normalize_for_speech("Discord voice check, Sir."));
    let speed = crate::jarvis_voice_dsp::synthesis_speed(cfg.kokoro_speed);
    let Ok(voice24) = kokoro.synthesize(&ipa, &cfg.kokoro_voice, speed) else {
        info!("[CALL CHECK] Synthesis failed");
        return ExitCode::FAILURE;
    };
    let result = tokio::task::spawn_blocking(move || -> Result<(f32, f32, f32), String> {
        let chat = host.input_devices().map_err(|e| e.to_string())?.find(|d| d.name().is_ok_and(|n| n == chat_name)).ok_or("Chat Mix vanished")?;
        let out = host.output_devices().map_err(|e| e.to_string())?.find(|d| d.name().is_ok_and(|n| n == channel)).ok_or("talk channel not found")?;
        let (in_cfg, out_cfg) = (chat.default_input_config().map_err(|e| e.to_string())?, out.default_output_config().map_err(|e| e.to_string())?);
        if in_cfg.sample_format() != cpal::SampleFormat::F32 || out_cfg.sample_format() != cpal::SampleFormat::F32 {
            return Err("only f32 devices are checked".into());
        }
        let (in_rate, out_rate) = (in_cfg.sample_rate().0, out_cfg.sample_rate().0);
        let voice: Vec<f32> = crate::dsp::resample(&voice24, crate::tts::KOKORO_RATE, out_rate).into_iter().map(|s| s * 0.35).collect();
        let played = voice.clone();
        let captured = Arc::new(Mutex::new(Vec::<f32>::new()));
        let sink = Arc::clone(&captured);
        let in_ch = in_cfg.channels() as usize;
        let rec = chat.build_input_stream(&in_cfg.config(), move |d: &[f32], _| lock(&sink).extend(to_mono(d, in_ch)), |_| {}, None).map_err(|e| e.to_string())?;
        rec.play().map_err(|e| e.to_string())?;
        // 300 ms of the room first, then the sentence.
        std::thread::sleep(Duration::from_millis(300));
        let baseline_len = lock(&captured).len();
        let out_ch = out_cfg.channels() as usize;
        let mut k = 0usize;
        let play = out.build_output_stream(&out_cfg.config(), move |d: &mut [f32], _| {
            for frame in d.chunks_mut(out_ch) {
                let v = voice.get(k).copied().unwrap_or(0.0);
                frame.iter_mut().for_each(|s| *s = v);
                k += 1;
            }
        }, |_| {}, None).map_err(|e| e.to_string())?;
        play.play().map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_secs_f32(played.len() as f32 / out_rate as f32 + 0.4));
        drop(play);
        drop(rec);
        let all = lock(&captured).clone();
        let (room, during) = all.split_at(baseline_len.min(all.len()));
        let rms = |x: &[f32]| (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt().max(1e-7);
        let r = best_correlation(&envelope(&played, out_rate as usize), &envelope(during, in_rate as usize), 20);
        Ok((20.0 * rms(during).log10(), 20.0 * (rms(during) / rms(room)).log10(), r))
    })
    .await;
    match result {
        Ok(Ok((level, above, r))) => {
            let ok = above > 6.0 && r > 0.5;
            info!("[CALL CHECK] Jarvis' voice in the Chat Mix: {level:.1} dBFS, {above:.1} dB above the room, match {r:.2} -> {}", if ok { "the call hears him" } else { "NOT reaching the call" });
            if !ok {
                info!("[CALL CHECK] {NO_TALK_CHANNEL}");
            }
            if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        Ok(Err(e)) => {
            info!("[CALL CHECK] Voice test failed: {e}");
            ExitCode::FAILURE
        }
        Err(e) => {
            info!("[CALL CHECK] Voice test crashed: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: usize, secs: f32, freq: f32, amp: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize).map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin()).collect()
    }

    #[test]
    fn the_call_vad_cuts_one_utterance_with_pre_roll() {
        let rate = 48_000;
        let mut vad = CallVad::new(rate);
        let frame = vad.frame_len();
        assert_eq!(frame, 960);
        // 1 s of digital silence (Discord between talkers), 1.2 s of speech, 1 s of silence.
        let mut audio = vec![0.0f32; rate];
        audio.extend(tone(rate, 1.2, 220.0, 0.2));
        audio.extend(vec![0.0f32; rate]);
        let mut clips = Vec::new();
        for f in audio.chunks(frame) {
            if f.len() == frame {
                if let Some(c) = vad.push_frame(f) {
                    clips.push(c);
                }
            }
        }
        assert_eq!(clips.len(), 1);
        let (clip, voiced) = &clips[0];
        assert!((1100..=1300).contains(voiced), "voiced {voiced} ms");
        // Speech + 300 ms pre-roll + the 700 ms tail.
        let secs = clip.len() as f32 / rate as f32;
        assert!(secs > 1.8 && secs < 2.4, "{secs} s");
        // A click is no utterance.
        let mut vad = CallVad::new(rate);
        let mut click = vec![0.0f32; rate];
        click.extend(tone(rate, 0.1, 1000.0, 0.3));
        click.extend(vec![0.0f32; rate]);
        assert!(click.chunks(frame).filter(|f| f.len() == frame).all(|f| vad.push_frame(f).is_none()));
    }

    #[test]
    fn the_voice_check_recognises_its_own_sentence() {
        // A syllable rhythm, heard 60 ms later and quieter: a match; unrelated noise: none.
        let rate = 16_000;
        let speech: Vec<f32> = (0..rate * 2).map(|i| {
            let t = i as f32 / rate as f32;
            (t * 4.0 * std::f32::consts::TAU).sin().max(0.0) * (t * 180.0 * std::f32::consts::TAU).sin() * 0.3
        }).collect();
        let mut heard = vec![0.0f32; rate * 6 / 100];
        heard.extend(speech.iter().map(|s| s * 0.4));
        let r = best_correlation(&envelope(&speech, rate), &envelope(&heard, rate), 20);
        assert!(r > 0.9, "{r}");
        let mut seed = 0x2545_F491u32;
        let noise: Vec<f32> = (0..rate * 2).map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32 - 0.5) * 0.1
        }).collect();
        assert!(best_correlation(&envelope(&speech, rate), &envelope(&noise, rate), 20) < 0.5);
    }

    #[test]
    fn friends_address_jarvis_by_name() {
        assert!(addressed("Jarvis, what's the weather like?"));
        assert!(addressed("hey javis du bist witzig"));
        assert!(!addressed("Let's play another round"));
        assert!(!addressed("jarvisfan2000 joined"), "a word, not a part of one");
    }

    #[test]
    fn the_probe_hears_its_tone_and_picks_wave_link_channels() {
        let rate = 48_000.0;
        let with_tone: Vec<f32> = tone(48_000, 0.4, PROBE_HZ, 0.02).iter().zip(tone(48_000, 0.4, 300.0, 0.1)).map(|(a, b)| a + b).collect();
        assert!(probe_heard(&with_tone, rate));
        assert!(!probe_heard(&tone(48_000, 0.4, 300.0, 0.1), rate), "speech alone is no probe");
        assert!(!probe_heard(&vec![0.0; 19_200], rate));
        let outputs: Vec<String> = ["Speakers (Logitech G733 Gaming Headset)", "Microphone (fifine Microphone) (Elgato Virtual Audio)", "System (Elgato Virtual Audio)", "Voice chat (Elgato Virtual Audio)", "Music (Elgato Virtual Audio)", "Browser (Elgato Virtual Audio)", "SFX (Elgato Virtual Audio)"].iter().map(|s| s.to_string()).collect();
        assert_eq!(probe_candidates(&outputs, None), ["SFX (Elgato Virtual Audio)", "Music (Elgato Virtual Audio)", "Browser (Elgato Virtual Audio)", "System (Elgato Virtual Audio)", "Microphone (fifine Microphone) (Elgato Virtual Audio)"]);
        assert!(probe_candidates(&outputs, Some("Music (Elgato Virtual Audio)")).iter().all(|n| !n.contains("Music")));
    }

    #[test]
    fn heard_lines_feed_the_live_context_only_while_on() {
        lock(&HEARD).clear();
        remember("Bist du heute Abend dabei?");
        assert_eq!(recent(Duration::from_secs(60)).len(), 1);
        MODE.store(CallMode::Off as u8, Ordering::Release);
        assert!(live_context().is_empty(), "off: nothing leaves the bridge");
        MODE.store(CallMode::Listen as u8, Ordering::Release);
        assert!(live_context().contains("Bist du heute Abend dabei?") && live_context().contains("not Sir"));
        assert!(live_context().contains("[DISCORD CALL // PEER"), "peers are tagged apart from Sir's microphone");
        assert_eq!(status_label(), Some("CALL · LISTENING"));
        MODE.store(CallMode::Off as u8, Ordering::Release);
        lock(&HEARD).clear();
    }

    #[test]
    fn the_call_is_deaf_while_jarvis_speaks_and_250_ms_after() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut gate = AntiLoopbackGate::default();
        assert!(gate.admit(false, ms(0)), "open before Jarvis has spoken");
        assert!(!gate.admit(true, ms(10)), "closed while he speaks");
        assert!(!gate.admit(true, ms(900)));
        assert!(!gate.admit(false, ms(1000)), "the acoustic tail is still gated");
        assert!(!gate.admit(false, ms(1140)));
        assert!(gate.admit(false, ms(1160)), "open 250 ms after his last sample");
        assert!(!gate.admit(true, ms(2000)), "and closed again for the next sentence");
    }

    #[test]
    fn say_speaks_sirs_words_and_only_those() {
        assert_eq!(say_line(&json!({"action": "say"})), None);
        assert_eq!(say_line(&json!({"action": "say", "message": "  Back in five, lads.  "})).as_deref(), Some("Back in five, lads."));
        assert!(definitions()[0]["function"]["parameters"]["properties"]["action"]["enum"].as_array().unwrap().iter().any(|a| a == "say"));
    }
}
