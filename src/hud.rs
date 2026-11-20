//! Messages between the backend (`app::JAI`) and the native HUD window: events the HUD
//! renders and commands it sends back. Everything stays in-process — there is no web server.

use crate::ears::AudioLevel;
use crate::memory::MemoryEntry;
use crate::modality::VoiceResponseMode;
use crate::skills::JaiSkill;
use crate::telemetry::SystemTelemetry;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ConfigState {
    pub mic_enabled: bool,
    pub ptt_active: bool,
    pub wake_word_required: bool,
    pub vad_start_threshold: f32,
    pub vad_release_threshold: f32,
    pub silence_ms: u32,
    pub noise_multiplier: f32,
    pub window_secs: u64,
    pub model: String,
    pub fallback_model: String,
    pub voice: String,
    pub voices: Vec<String>,
    pub loudness_pct: u8,
    pub requested_mic: Option<String>,
    pub active_mic: String,
    pub requested_speaker: Option<String>,
    pub active_speaker: String,
    pub stt_ready: bool,
    pub stt_model: String,
    pub tts_status: String,
    pub hardware_profile: String,
    /// Language engine status line.
    pub llm: String,
    /// When JAI answers out loud (`modality`).
    pub response_mode: VoiceResponseMode,
    /// `mute_tts`: his voice is off for this session.
    pub voice_muted: bool,
    /// Sleep mode: only "wake up" is answered.
    pub sleeping: bool,
}

#[derive(Debug, Clone)]
pub enum HudEvent {
    State { state: String, detail: String, window_ms: u64 },
    AudioLevel(AudioLevel),
    VadEvent { event: String, detail: String },
    Transcript { text: String, language: String, source: String, wake_word: Option<String>, stt_ms: u64, audio_ms: u64 },
    FirstToken { message_id: String, latency_ms: u64, since_speech_ms: u64, model: String },
    Token { message_id: String, token: String },
    AssistantDone { message_id: String, text: String, latency_ms: u64, model: String, interrupted: bool },
    /// Sir kept talking before JAI answered: the reply is withdrawn and his words are
    /// stitched onto the next transcript.
    Retracted { message_id: String },
    /// The end of the previous conversation (time, role, text), shown when the HUD opens.
    ChatHistory { lines: Vec<(String, String, String)> },
    TurnMetrics { stt_ms: u64, first_token_ms: Option<u64>, first_audio_ms: u64 },
    ToolLog { tool: String, args: Value, result: String, success: bool, duration_ms: u64, timestamp: String },
    SystemMetrics { telemetry: SystemTelemetry },
    Memories { items: Vec<MemoryEntry>, query: Option<String> },
    Skills { items: Vec<JaiSkill> },
    ConfigState(ConfigState),
    Devices { inputs: Vec<String>, outputs: Vec<String> },
    Models { items: Vec<String>, current: String },
    Notice { message: String },
    Error { message: String },
    /// The Guardian found an anomaly (the HUD's core pulses its node in crimson).
    Guardian { message: String },
    /// A background module (the personal manager's watchers, the itslearning keep-alive) was
    /// active: the core lights this path of nodes (`alert`: something changed).
    CorePulse { path: Vec<String>, alert: bool },
    /// A WhatsApp message waiting for Sir's go-ahead (or its new status: sent / cancelled).
    MessageCard { id: String, recipient: String, phone: String, text: String, status: String },
    /// The acoustic radar heard something (a knock): the HUD flashes amber.
    RoomAlert { message: String },
    /// A silent quick command ran ("pause", "mute", "volume 40"): a small badge, no speech.
    QuickAction { label: String, ok: bool },
    /// A PC shutdown / restart waiting for Sir's double-check, or its outcome (`status`: pending,
    /// confirmed, cancelled, expired): the crimson confirmation card.
    PowerCard { id: String, action: String, label: String, status: String },
    /// JAI is quitting; the HUD shows that he is going offline.
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum HudCommand {
    Chat { text: String },
    Interrupt,
    SetMic { enabled: bool },
    Ptt { active: bool },
    SetWakeWordRequired { required: bool },
    SetVad {
        start_threshold: Option<f32>,
        release_threshold: Option<f32>,
        silence_ms: Option<u32>,
        noise_multiplier: Option<f32>,
    },
    Recalibrate,
    AddMemory { key: String, value: String, category: Option<String>, importance: Option<i32> },
    DeleteMemory { id_or_key: String },
    SearchMemories { query: String },
    SetModel { model: String },
    ListModels,
    SetVoice { voice: String },
    SetLoudness { volume_pct: u8 },
    SetMicDevice { device: Option<String> },
    SetSpeakerDevice { device: Option<String> },
    RefreshDevices,
    TestVoice,
    TestMic,
    RunSkill { name: String },
    DeleteSkill { name: String },
    ClearHistory,
    /// Quit JAI (HUD power button).
    Shutdown,
    /// The HUD's reply-mode selector.
    SetResponseMode { mode: VoiceResponseMode },
    /// Widget interactions in the HUD or a JAI window.
    WidgetToggleItem { id: String, index: usize },
    WidgetTimer { id: String, op: String },
    WidgetAction { id: String, index: usize },
    WidgetRefresh { id: String },
    WidgetRemove { id: String },
    /// A chat message in its own native window.
    OpenTextWindow { title: String, text: String },
    /// `[ Send via WhatsApp ]` (true) or `[ Cancel ]` on a message card.
    MessageCard { id: String, send: bool },
    /// `[ Confirm PC Shutdown ]` (true) or `[ Cancel ]` on the power confirmation card.
    PowerCard { id: String, confirm: bool },
}
