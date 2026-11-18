//! Persistent configuration (`jai_config.json`).
//!
//! * Every field has a default (`#[serde(default)]`), so older or partial files load cleanly.
//! * `config_version` drives one-time migrations of tuned parameters (VAD timings etc.)
//!   while user preferences (devices, voice, model, loudness) are preserved.
//! * Writes are atomic (temp file + rename) and debounced so live UI sliders don't thrash the disk.

use crate::interface::InterfaceSettings;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use tracing::{error, info, warn};

pub const CONFIG_FILE: &str = "jai_config.json";
/// Bump whenever tuned defaults change and old persisted values must be migrated.
pub const CONFIG_VERSION: u32 = 28;

/// His name as the settings list it (`wake::WakeWordMatcher` knows how it is misheard and spelt;
/// further names may be added here).
pub const DEFAULT_WAKE_WORDS: &[&str] = &["jai", "jay"];

/// The language model since v23 (`migrate`): Qwen3.5 4B.
pub const PRIMARY_MODEL: &str = "qwen3.5:4b";
/// The stock primary model of v12-v22: a Qwen2.5 7B under the tag "qwen3:30b-a3b".
const LEGACY_PRIMARY_MODEL: &str = "qwen3:30b-a3b";
/// Qwen3.5's window: the whole prompt in its template (≈ 30k), the live context and a reasoned
/// answer. Qwen2.5 (the fallback) only ever fills its own 32k positions of it.
pub const QWEN35_NUM_CTX: u32 = 40_960;

/// A Qwen3.5 model by its tag ("qwen3.5:4b"): its own sampling and the 40k window.
pub fn is_qwen35(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("qwen3.5") || m.contains("qwen3_5") || m.contains("qwen35")
}

/// Whisper's initial prompt: the name as Sir says it ("Jay") plus his own short domain words, so
/// "core" stops coming out as "car" / "call" / "for C-O-R-E". Short on purpose (the long v9
/// list made Whisper invent words on quiet audio); `no_context` + `single_segment` keep it from
/// echoing, and `stt::normalize_stt_transcript` / `typos::core_homophones` repair what still
/// slips through. It opens with a greeting as a sentence: a bare list of words made the
/// self-test mishear the name (2026-11-02 A/B, same clips).
pub const WHISPER_DOMAIN_PROMPT: &str = "Hey Jay. Show your core, hide your core, close your core. Orb, HUD, board, blocks, Sonora, Spotify, WebUntis, itslearning.";

/// A value's fingerprint (FNV-1a, 64 bit). The stock values of earlier settings versions are
/// kept as fingerprints: a settings file from before v24 is brought up to date as it always
/// was, and the name he had then is written nowhere.
pub(crate) fn fingerprint(value: &str) -> u64 {
    value.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// Earlier default prompts, whitespace collapsed (v12-v17, v18-v23, v18's first build, v9-v10,
/// v4, v5, v6-v8, v22): replaced on migration; a prompt Sir wrote himself stays.
const STOCK_PROMPTS: [u64; 8] = [
    0xf36e_a15a_d215_d828,
    0x1199_73a0_01f5_c968,
    0xfed6_77b4_7bc6_dbe4,
    0xfd42_252b_0eb3_adc3,
    0x179e_290d_0566_afbe,
    0x5fa3_00bd_7bff_9f5e,
    0x5ab7_18fe_bb4b_c004,
    0x4681_af27_8122_e3c4,
];
/// The stock wake words of v1-v23 (lower case).
const STOCK_WAKE_WORDS: [u64; 7] = [0x979e_eefc_7363_6064, 0xa760_0ad3_2f22_bd2a, 0x6dbc_2d77_f26c_4c41, 0xffbe_25f7_baf8_72c5, 0x979e_e7fc_7363_547f, 0xd9cf_874f_5ed7_7792, 0x869e_b459_8016_e517];
/// The stock paths of the memory database and the log (v1-v24), and the film voice's alias
/// (v15-v25).
const STOCK_DB_PATH: u64 = 0xac98_211b_7100_d85e;
const STOCK_LOG_FILE: u64 = 0xec79_b0f6_831d_d6f4;
const STOCK_VOICE_ALIAS: u64 = 0x43cd_6bae_b2a4_ddc1;

/// The v8 voice: replaced by the v9 film voice unless Sir chose another.
const V8_VOICE_BLEND: &str = "bm_george:0.45+bm_fable:0.35+bm_lewis:0.2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HardwareProfile {
    /// x86_64 desktop (Windows or Linux), optionally with an NVIDIA GPU.
    #[default]
    #[serde(alias = "DesktopRtx3060Ti")]
    Desktop,
    /// Raspberry Pi 5 class ARM64 edge device.
    #[serde(alias = "EdgePi5AiHat")]
    EdgePi5,
}

impl HardwareProfile {
    pub fn detect() -> Self {
        if let Ok(value) = std::env::var("JAI_PROFILE") {
            let v = value.to_lowercase();
            if v.contains("pi") || v.contains("edge") || v.contains("arm") {
                return Self::EdgePi5;
            }
            if v.contains("desktop") || v.contains("pc") {
                return Self::Desktop;
            }
        }
        if cfg!(feature = "edge-pi5") {
            return Self::EdgePi5;
        }
        if cfg!(any(target_arch = "aarch64", target_arch = "arm")) {
            let model = fs::read_to_string("/proc/device-tree/model").unwrap_or_default();
            if model.to_lowercase().contains("raspberry pi") {
                return Self::EdgePi5;
            }
        }
        Self::Desktop
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Desktop => "Desktop PC",
            Self::EdgePi5 => "Raspberry Pi 5 (edge)",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JaiConfig {
    /// Missing in pre-v4 files, which must therefore read as version 0 to trigger migration.
    #[serde(default = "legacy_config_version")]
    pub config_version: u32,
    pub hardware_profile: HardwareProfile,

    // ---- HUD window -----------------------------------------------------------------------
    /// Show the HUD window at start-up (otherwise JAI starts in the tray).
    pub open_ui_on_start: bool,
    /// Look and layout of the HUD; JAI changes it on request (`configure_interface`).
    pub interface: InterfaceSettings,
    /// Last HUD window placement on the desktop (x, y, width, height in physical pixels).
    pub hud_window: Option<[i32; 4]>,
    pub hud_maximized: bool,

    // ---- LLM ----------------------------------------------------------------------------
    /// Built-in engine shipped with JAI (a private copy of Ollama). Used whenever present.
    pub llm_runtime_dir: String,
    /// Private model store of the built-in engine.
    pub llm_models_dir: String,
    /// Preferred localhost port of the built-in engine (the next free one is used if taken).
    pub llm_port: u16,
    /// External Ollama, used only when the built-in engine is missing.
    pub ollama_url: String,
    pub primary_model: String,
    pub fallback_model: String,
    pub num_ctx: u32,
    /// Seconds to keep the model resident after a request; -1 = forever (the VRAM guard caps it).
    pub keep_alive: i64,
    /// -1 = let Ollama decide.
    pub num_gpu: i32,
    /// 0 = let Ollama decide.
    pub num_thread: u32,
    pub temperature: f32,
    pub top_p: f32,
    /// Hard cap on generated tokens per LLM round (spoken replies are short).
    pub max_reply_tokens: i32,
    /// Sliding conversation window, in user turns.
    pub history_turns: usize,
    /// Highest-importance memories injected into the (cache-stable) system prompt.
    pub core_memories_in_prompt: usize,
    /// Anti-repetition sampling for spoken/chat replies (JSON one-shots keep plain sampling).
    pub repeat_penalty: f32,
    pub repeat_last_n: i32,
    pub presence_penalty: f32,
    pub frequency_penalty: f32,

    // ---- Voice activity detection (512-sample / 32 ms frames @ 16 kHz) ------------------
    pub vad_start_threshold: f32,
    pub vad_release_threshold: f32,
    /// Speech start also requires RMS > noise_floor * multiplier.
    pub vad_noise_multiplier: f32,
    /// Consecutive voiced frames needed to open an utterance (2 = 64 ms).
    pub vad_start_frames: u32,
    /// Trailing silence that closes a normal sentence (450 ms; the HUD's "silence before
    /// end-of-speech" slider). Utterance stitching and the unfinished-thought wait keep longer
    /// thinking pauses together.
    pub silence_hangover_ms: u32,
    /// Trailing silence after which the words so far go to Whisper for a head start: 220 ms
    /// (160 ms behind a one-word reflex, `reflex_hangover_ms`). The utterance itself only ends
    /// after `short_command_commit_ms` / `silence_hangover_ms`.
    pub short_command_hangover_ms: u32,
    /// Trailing silence that closes a short burst ("stop", "pause", "JAI, next page"): 280 ms,
    /// long enough for the breath between "JAI" and the command.
    pub short_command_commit_ms: u32,
    /// Up to this much voiced audio an utterance counts as a short command burst.
    pub short_command_max_ms: u32,
    /// The head start behind a one-word reflex burst ("mute", "stop", "weiter"): after 160 ms of
    /// silence the word reaches Whisper's small reflex window, while the utterance stays open
    /// until `short_command_commit_ms` in case more follows ("JAI … open Discord").
    pub reflex_hangover_ms: u32,
    /// A reflex burst has at least this much voiced audio …
    pub reflex_min_ms: u32,
    /// … and at most this much.
    pub reflex_max_ms: u32,
    /// Extra wait after a fragment that sounds unfinished ("… und", "open the", < 4 words).
    pub incomplete_thought_wait_ms: u32,
    /// Speech that resumes before JAI has answered is joined onto the previous transcript.
    pub utterance_stitching: bool,
    pub pre_roll_ms: u32,
    /// Utterances with less voiced audio than this are discarded as noise.
    pub min_speech_ms: u32,
    pub max_utterance_secs: u32,
    pub noise_calibration_ms: u32,
    pub barge_in_enabled: bool,
    pub barge_in_min_ms: u32,
    /// Added to the start threshold while JAI is talking (speaker-echo guard).
    pub barge_in_threshold_boost: f32,
    /// After JAI answers (or asks something), Sir may talk on without the wake word.
    pub active_conversation_window_secs: u64,

    // ---- Wake word ----------------------------------------------------------------------
    pub wake_words: Vec<String>,
    pub wake_word_similarity_threshold: f64,
    pub wake_word_required: bool,
    /// The addressee detector: speech meant for JAI is answered without the wake word (an
    /// open conversation, his window in front, a direct command or question), while gamer
    /// chatter over a game or Discord is ignored even in hands-free mode.
    pub addressee_detection: bool,

    // ---- Models -------------------------------------------------------------------------
    pub auto_download_models: bool,
    pub silero_vad_model_path: String,
    pub whisper_model_path: String,
    pub whisper_fallback_model_path: String,
    /// 0 = auto (physical cores).
    pub whisper_threads: i32,
    pub whisper_initial_prompt: String,
    /// Beam-search width (≤ 1 = greedy). 2 recovers mumbled words at a small latency cost.
    pub whisper_beam_size: i32,
    /// Language auto-detection is constrained to these codes. Empty = unconstrained.
    pub whisper_languages: Vec<String>,
    /// Shrink the Whisper encoder window to the utterance length (large CPU speed-up).
    pub whisper_dynamic_audio_ctx: bool,
    /// Smallest encoder window (frames, 50 per second) used with the dynamic window.
    pub whisper_min_audio_ctx: i32,
    pub whisper_flash_attn: bool,
    /// The whole process runs above normal priority for the half second a decode lasts
    /// (`platform::urgent`), so a busy CPU does not slow the transcript. Off: Whisper shares
    /// the cores evenly with whatever else runs.
    pub whisper_priority_boost: bool,
    /// One encoder pass per clip: greedy in the last clip's language, the language read from the
    /// same pass, the beam search only for an unsure sentence (`stt::one_pass`). False = the v22
    /// decoding (separate language detection, beam search for every sentence).
    pub whisper_fast_decode: bool,
    /// Words spoken to him that lead nowhere get a second, careful hearing of their clip (the
    /// beam search, a wide window, the names of Sir's applications): `stt::transcribe_carefully`.
    pub whisper_second_hearing: bool,
    /// The last clips of Sir's own orders are kept on this PC (`voice_clips`: his hearing is
    /// tested on them; never sent anywhere). Sir, 2026-11-13: "Keep them".
    pub keep_voice_clips: bool,
    pub kokoro_model_path: String,
    /// `voices.bin` (npz) from the kokoro-onnx v0.19 release; voices are extracted on demand.
    pub kokoro_voices_archive_path: String,
    pub kokoro_voice: String,
    pub kokoro_speed: f32,
    /// How much the voice's pitch movement is flattened around each clause's median (0 = off,
    /// 0.35 = the film voice's level intonation; `fish_zero_shot_tts::flatten_f0`).
    pub voice_f0_flatten: f32,
    /// 0 = auto.
    pub tts_threads: usize,
    /// Run Kokoro on the GPU via DirectML (Windows). Falls back to CPU if unavailable.
    pub tts_gpu: bool,
    /// eSpeak-NG install directory or library path override.
    pub espeak_path: Option<String>,

    // ---- Audio devices & output ---------------------------------------------------------
    /// JAI voice loudness (0-200 %), independent of the system volume.
    pub loudness_pct: u8,
    pub preferred_mic: Option<String>,
    pub preferred_speaker: Option<String>,
    /// The output JAI speaks on while he may talk in Sir's Discord call: a channel that reaches
    /// Discord's microphone (with Wave Link, one in the Chat Mix). None: found once by a silent
    /// probe (`discord_bridge`), then kept here. JAI never changes Wave Link itself.
    pub discord_call_output: Option<String>,
    /// The output whose sound is the call, for listening (None: Discord's own channel or session,
    /// found automatically). Set from the HUD's `DISCORD // AUDIO ROUTING` block.
    pub discord_listen_source: Option<String>,
    /// The Stream Deck bridge's secret (`streamdeck_bridge`; generated on first start).
    pub streamdeck_token: String,
    pub startup_greeting: bool,
    /// What his name stands for, as Sir put it (kept in the settings file, not in the code:
    /// it may hold Sir's own name). Empty: he is J.A.I. and says no more.
    #[serde(default)]
    pub name_meaning: String,
    /// JAI warns on his own about overheating, full memory or disks (`watchdog`).
    pub proactive_alerts: bool,
    /// RSS feed for the headlines of the daily briefing ("" = no headlines).
    pub briefing_news_feed: String,
    /// The parts Sir left out of his briefing ("news", "weather" …: `briefing::PARTS`).
    pub briefing_skip: Vec<String>,

    // ---- Stark systems --------------------------------------------------------------------
    /// WhatsApp: desktop URI handler, notification listener and the optional local bridge.
    pub whatsapp: WhatsAppSettings,
    /// JAI's own Discord account (a bot Sir created; its token lives in the vault).
    pub discord_bot: DiscordBotSettings,
    /// Away Mode: the answering machine, alerts to Sir's phone and the digest when he's back.
    pub away: AwaySettings,
    /// The mobile web satellite (phone / iPad HUD with live GPS).
    pub satellite: SatelliteSettings,
    /// Home / school coordinates, IP fallback, the home geofence and the commute advisor.
    pub location: LocationSettings,
    /// Double-clap wake gesture and the door-knock radar.
    pub acoustic: AcousticSettings,
    /// The field systems: Instant Replay, projects, network, deals, Minecraft, alarm.
    pub features: FeatureSettings,
    /// Zero-VRAM gaming standby: idle/game eviction of every model, wake hotkeys.
    pub vram_guard: crate::vram_guard::VramGuardConfig,
    /// live layers (C1): the rain note before rain arrives and the ISS pass heads-up.
    pub live_layers: crate::live_layers::LiveLayerSettings,

    // ---- Storage ------------------------------------------------------------------------
    pub memory_db_path: String,
    pub skills_dir: String,
    pub log_file: String,
}

fn legacy_config_version() -> u32 {
    0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WhatsAppSettings {
    /// Reads WhatsApp Desktop's toasts from the Windows notification database.
    pub notification_listener: bool,
    /// Local bridge (whatsapp-web.js, Baileys, WAHA …) base URL; empty = no bridge.
    pub bridge_url: String,
    /// "generic" (`GET /messages`, `POST /send`) or "waha" (`POST /api/sendText`).
    pub bridge_kind: String,
    /// WAHA session name.
    pub bridge_session: String,
    /// Without a bridge, "send it now" presses Enter in WhatsApp Desktop once the chat is open.
    pub desktop_auto_send: bool,
    /// Country code for numbers written without one ("0151…" → "+49151…").
    pub default_country_code: String,
    /// How often the listener and the bridge are read.
    pub poll_seconds: u64,
}

impl Default for WhatsAppSettings {
    fn default() -> Self {
        Self {
            notification_listener: true,
            bridge_url: String::new(),
            bridge_kind: "generic".into(),
            bridge_session: "default".into(),
            desktop_auto_send: true,
            default_country_code: "+49".into(),
            poll_seconds: 15,
        }
    }
}

/// JAI's own Discord account (`discord_bot`). The token is in the vault, never here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordBotSettings {
    pub enabled: bool,
    /// Sir's Discord user id (set by pairing): his DMs are orders, everyone else only chats.
    pub owner_id: String,
    /// Friends who DM or mention JAI get a short answer in character.
    pub answer_others: bool,
    /// Away Mode, reminders and alerts reach Sir as DMs.
    pub notify_owner: bool,
    /// JAI joins whatever voice channel Sir joins (and leaves with him).
    pub follow_owner_voice: bool,
    /// "Tell me when Max comes online" (the Presence Intent must be on in the Developer Portal).
    pub presence_intent: bool,
    /// JAI says once per channel, in its text chat, that he transcribes the call.
    pub announce_listening: bool,
    /// "Watching …" under his name.
    pub status_text: String,
}

impl Default for DiscordBotSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            owner_id: String::new(),
            answer_others: true,
            notify_owner: true,
            follow_owner_voice: false,
            presence_intent: false,
            announce_listening: true,
            status_text: "over Sir".into(),
        }
    }
}

impl DiscordBotSettings {
    pub fn owner(&self) -> Option<u64> {
        self.owner_id.trim().parse().ok().filter(|id| *id > 0)
    }
}

/// Away Mode (`away_mode`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AwaySettings {
    /// Away Mode turns on by itself once the PC has been locked this long (0 = only by voice).
    pub auto_after_locked_mins: u64,
    /// A DM to Sir when someone unlocks the PC while he's away.
    pub alert_on_unlock: bool,
    /// A DM to Sir when a USB drive is plugged in while he's away.
    pub alert_on_usb: bool,
    /// Friends' messages are answered and collected for the digest.
    pub answering_machine: bool,
}

impl Default for AwaySettings {
    fn default() -> Self {
        Self { auto_after_locked_mins: 10, alert_on_unlock: true, alert_on_usb: true, answering_machine: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FeatureSettings {
    /// Instant Replay: a rolling buffer of screen frames (JPEG) kept in memory.
    pub replay_enabled: bool,
    pub replay_seconds: u32,
    /// Frames per second of the buffer (1-15). 5 keeps the background capture light; the
    /// capture never includes the cursor or layered windows (no cursor flicker).
    pub replay_fps: f32,
    /// Width the frames are scaled to (pixels).
    pub replay_width: u32,
    /// Where new coding projects go ("" = D:\Code when it exists, else ~\Projects).
    pub projects_dir: String,
    /// Extra hosts the Network Guardian pings (game servers: "euw.leagueoflegends.com", "1.2.3.4").
    pub game_servers: Vec<String>,
    /// How often the Deal Sniper checks the marketplaces (minutes).
    pub deal_interval_min: u32,
    /// The Minecraft folder ("" = %APPDATA%\.minecraft and the launchers' instances).
    pub minecraft_dir: String,
    /// The Minecraft server ("host:25565") for status pings.
    pub minecraft_server: String,
    /// RCON ("host:25575") and its password, for server commands.
    pub rcon_address: String,
    pub rcon_password: String,
    /// `user@host` for SSH commands to the server.
    pub ssh_target: String,
    /// Smart Alarm: fade-in time and the volume it fades to (percent).
    pub alarm_fade_secs: u32,
    pub alarm_volume: u32,
}

impl Default for FeatureSettings {
    fn default() -> Self {
        Self {
            replay_enabled: true,
            replay_seconds: 60,
            replay_fps: 5.0,
            replay_width: 1280,
            projects_dir: String::new(),
            game_servers: Vec::new(),
            deal_interval_min: 30,
            minecraft_dir: String::new(),
            minecraft_server: String::new(),
            rcon_address: String::new(),
            rcon_password: String::new(),
            ssh_target: String::new(),
            alarm_fade_secs: 120,
            alarm_volume: 60,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SatelliteSettings {
    pub enabled: bool,
    pub port: u16,
    /// Browsers only hand out GPS to secure pages, so HTTPS (self-signed) is the default.
    pub https: bool,
    /// Pairing token every request must carry (`?k=` / `X-JAI-Key`); generated once.
    pub token: String,
    /// Public HTTPS tunnel. Disable to retain local-network access only.
    pub worldwide_enabled: bool,
    /// auto/cloudflare, tailscale, or ssh. SSH requires an HTTPS reverse proxy on the VPS.
    pub tunnel_provider: String,
    /// The permanent address (`https://jai.your-domain`): with a Cloudflare tunnel token in
    /// the vault it is the named tunnel's own hostname, the same after every restart; with
    /// SSH it is the reverse proxy's. Empty with Cloudflare: a quick tunnel, whose
    /// trycloudflare.com address is new on every start.
    pub tunnel_public_url: String,
    /// The loopback port the tunnel forwards to. A permanent Cloudflare address names it in
    /// the tunnel's own settings (service `http://127.0.0.1:8444`), so it must not change.
    pub tunnel_origin_port: u16,
    pub tunnel_ssh_target: String,
    pub tunnel_ssh_key: String,
    pub tunnel_ssh_remote_port: u16,
    /// Optional LAN machine to wake; this PC cannot serve requests while asleep.
    pub wake_mac: String,
    pub wake_broadcast: String,
    /// Independent always-on relay URL for waking this PC while its satellite is offline.
    pub wake_relay_url: String,
}

impl Default for SatelliteSettings {
    fn default() -> Self {
        Self {
            enabled: true, port: 8443, https: true, token: String::new(),
            worldwide_enabled: true, tunnel_provider: "auto".into(),
            tunnel_public_url: String::new(), tunnel_origin_port: 8444, tunnel_ssh_target: String::new(),
            tunnel_ssh_key: String::new(), tunnel_ssh_remote_port: 18443,
            wake_mac: String::new(), wake_broadcast: "255.255.255.255".into(),
            wake_relay_url: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocationSettings {
    pub home_lat: Option<f64>,
    pub home_lon: Option<f64>,
    pub home_label: String,
    pub school_lat: Option<f64>,
    pub school_lon: Option<f64>,
    pub school_label: String,
    /// Without a phone fix or home coordinates, locate the PC by its IP (ip-api.com).
    pub ip_fallback: bool,
    pub geofence_enabled: bool,
    /// Leaving: farther than this from home. Arriving: back within `geofence_arrive_m`.
    pub geofence_leave_m: f64,
    pub geofence_arrive_m: f64,
    pub arrival_wake_hud: bool,
    pub arrival_greeting: bool,
    /// Playlist started on arrival ("" = none).
    pub arrival_playlist: String,
    /// Stark protocol engaged on arrival ("" = none).
    pub arrival_protocol: String,
    /// Minutes added to every commute for parking, helmet and the walk in.
    pub commute_buffer_min: u32,
    /// motorcycle, car, bike, walk or transit.
    pub default_travel_mode: String,
}

impl Default for LocationSettings {
    fn default() -> Self {
        Self {
            home_lat: None,
            home_lon: None,
            home_label: String::new(),
            school_lat: None,
            school_lon: None,
            school_label: String::new(),
            ip_fallback: true,
            geofence_enabled: true,
            geofence_leave_m: 500.0,
            geofence_arrive_m: 150.0,
            arrival_wake_hud: true,
            arrival_greeting: true,
            arrival_playlist: String::new(),
            arrival_protocol: String::new(),
            commute_buffer_min: 10,
            default_travel_mode: "motorcycle".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AcousticSettings {
    /// Two sharp claps 150-450 ms apart.
    pub double_clap: bool,
    /// "wake" (JAI listens) or "toggle_music" (play / pause).
    pub clap_action: String,
    /// Rhythmic low knocks (100-900 Hz) duck the music and raise an amber alert.
    pub knock_alert: bool,
    /// Knock alerts only while music is playing (Sir wears headphones), not all the time.
    pub knock_only_with_music: bool,
    /// 0.5 (fewer false alarms) … 2.0 (quieter claps and knocks count).
    pub sensitivity: f32,
}

impl Default for AcousticSettings {
    fn default() -> Self {
        Self { double_clap: true, clap_action: "wake".into(), knock_alert: true, knock_only_with_music: true, sensitivity: 1.0 }
    }
}

impl Default for JaiConfig {
    fn default() -> Self {
        let profile = HardwareProfile::detect();
        let env = |key: &str, fallback: &str| std::env::var(key).unwrap_or_else(|_| fallback.to_string());

        Self {
            config_version: CONFIG_VERSION,
            hardware_profile: profile,

            open_ui_on_start: true,
            interface: InterfaceSettings::default(),
            hud_window: None,
            hud_maximized: false,

            llm_runtime_dir: "runtime/ollama".to_string(),
            llm_models_dir: "models/llm".to_string(),
            llm_port: 11435,
            ollama_url: env("OLLAMA_URL", "http://127.0.0.1:11434"),
            primary_model: env("JAI_PRIMARY_MODEL", PRIMARY_MODEL),
            fallback_model: env(
                "JAI_FALLBACK_MODEL",
                match profile {
                    HardwareProfile::Desktop => "qwen2.5:7b-instruct",
                    HardwareProfile::EdgePi5 => "qwen2.5:3b",
                },
            ),
            // System prompt and tool schemas are ≈ 26k tokens (≈ 30k in Qwen3.5's template); its
            // hybrid attention keeps the 40k window at 3.9 GB in all (2026-11-09 lab).
            num_ctx: QWEN35_NUM_CTX,
            keep_alive: crate::vram_guard::RESIDENT_SECS as i64,
            num_gpu: -1,
            num_thread: 0,
            temperature: 0.55,
            top_p: 0.9,
            max_reply_tokens: 384,
            history_turns: 20,
            core_memories_in_prompt: 40,
            repeat_penalty: 1.18,
            repeat_last_n: 256,
            presence_penalty: 0.5,
            frequency_penalty: 0.4,

            vad_start_threshold: 0.50,
            vad_release_threshold: 0.35,
            vad_noise_multiplier: 2.2,
            vad_start_frames: 2,
            silence_hangover_ms: 450,
            short_command_hangover_ms: 220,
            short_command_commit_ms: 280,
            short_command_max_ms: 1100,
            reflex_hangover_ms: 160,
            reflex_min_ms: 150,
            reflex_max_ms: 850,
            incomplete_thought_wait_ms: 400,
            utterance_stitching: true,
            pre_roll_ms: 500,
            min_speech_ms: 200,
            max_utterance_secs: 20,
            noise_calibration_ms: 1000,
            barge_in_enabled: true,
            barge_in_min_ms: 160,
            barge_in_threshold_boost: 0.15,
            active_conversation_window_secs: 5,

            wake_words: DEFAULT_WAKE_WORDS.iter().map(|s| s.to_string()).collect(),
            wake_word_similarity_threshold: 0.80,
            wake_word_required: true,
            addressee_detection: true,

            auto_download_models: true,
            silero_vad_model_path: env("SILERO_VAD_MODEL", "models/silero_vad.onnx"),
            // The full-precision small model hears "core" / accented English best; base (or the
            // q8 builds) only when it is missing (`stt::WhisperEngine::load` walks the list).
            whisper_model_path: env("WHISPER_MODEL", "models/ggml-small.bin"),
            whisper_fallback_model_path: "models/ggml-base.bin".to_string(),
            whisper_threads: 0,
            whisper_initial_prompt: WHISPER_DOMAIN_PROMPT.to_string(),
            // Beam search for sentences; one-word commands stay greedy (`stt::run_full`).
            whisper_beam_size: 2,
            whisper_languages: vec!["de".to_string(), "en".to_string()],
            whisper_dynamic_audio_ctx: true,
            // The encoder window follows the clip ((secs / 30 × 1500) + 64 frames); 128 is the floor.
            whisper_min_audio_ctx: 128,
            whisper_flash_attn: false,
            whisper_priority_boost: true,
            whisper_fast_decode: true,
            whisper_second_hearing: true,
            keep_voice_clips: true,
            kokoro_model_path: env("KOKORO_MODEL", crate::models::KOKORO_V1_MODEL),
            kokoro_voices_archive_path: crate::models::KOKORO_V1_VOICES.to_string(),
            kokoro_voice: env("KOKORO_VOICE", crate::tts::JAI_VOICE),
            kokoro_speed: crate::tts::JAI_VOICE_SPEED,
            voice_f0_flatten: crate::fish_zero_shot_tts::DEFAULT_F0_FLATTEN,
            tts_threads: 0,
            tts_gpu: false,
            espeak_path: None,

            loudness_pct: 100,
            preferred_mic: None,
            preferred_speaker: None,
            discord_call_output: None,
            discord_listen_source: None,
            streamdeck_token: String::new(),
            startup_greeting: true,
            name_meaning: String::new(),
            proactive_alerts: true,
            briefing_news_feed: crate::briefing::DEFAULT_FEED.to_string(),
            briefing_skip: Vec::new(),

            whatsapp: WhatsAppSettings::default(),
            discord_bot: DiscordBotSettings::default(),
            away: AwaySettings::default(),
            satellite: SatelliteSettings::default(),
            location: LocationSettings::default(),
            acoustic: AcousticSettings::default(),
            features: FeatureSettings::default(),
            vram_guard: crate::vram_guard::VramGuardConfig::default(),
            live_layers: crate::live_layers::LiveLayerSettings::default(), // live layers (C1)

            memory_db_path: env("JAI_MEMORY_DB", "jai_memory.db"),
            skills_dir: "skills".to_string(),
            log_file: "jai.log".to_string(),
        }
    }
}

impl JaiConfig {
    /// Loads `jai_config.json` (or defaults), migrates old versions and clamps ranges.
    /// The (possibly migrated) result is written back so the file always reflects reality.
    pub fn load() -> Self {
        let path = Path::new(CONFIG_FILE);
        let mut cfg = match fs::read_to_string(path) {
            Ok(raw) => match serde_json::from_str::<JaiConfig>(&raw) {
                Ok(cfg) => cfg,
                Err(e) => {
                    let backup = format!("{CONFIG_FILE}.broken");
                    error!("Config file is invalid ({e}); keeping a copy at {backup} and using defaults.");
                    let _ = fs::copy(path, &backup);
                    Self::default()
                }
            },
            Err(_) => {
                info!("No {CONFIG_FILE} found, creating one with defaults.");
                Self::default()
            }
        };

        // Before an update changes the settings, the file as Sir left it is kept beside it
        // (`jai_config.json.v26`): one copy per version, so there is a way back.
        if !cfg!(test) && path.is_file() && cfg.config_version < CONFIG_VERSION {
            let kept = format!("{CONFIG_FILE}.v{}", cfg.config_version);
            if !Path::new(&kept).exists() && fs::copy(path, &kept).is_ok() {
                info!("The settings as they were before this update are kept in {kept}.");
            }
        }
        cfg.migrate();
        cfg.sanitize();
        if let Err(e) = cfg.save() {
            warn!("Could not write {CONFIG_FILE}: {e}");
        }
        cfg
    }

    fn migrate(&mut self) {
        if self.config_version >= CONFIG_VERSION {
            return;
        }
        let defaults = Self::default();
        info!(
            "Migrating {CONFIG_FILE} v{} -> v{CONFIG_VERSION} (tuning updated to new defaults; devices, voice, loudness and a model Sir chose kept).",
            self.config_version
        );
        if self.config_version < 4 {
            // v<4: the old VAD stack used a single 0.38 threshold and forced 260 ms silence; the
            // new dual-threshold state machine has its own calibrated defaults.
            self.vad_start_threshold = defaults.vad_start_threshold;
            self.vad_release_threshold = defaults.vad_release_threshold;
            self.vad_noise_multiplier = defaults.vad_noise_multiplier;
            self.vad_start_frames = defaults.vad_start_frames;
            self.pre_roll_ms = defaults.pre_roll_ms;
            self.max_utterance_secs = defaults.max_utterance_secs;
            self.barge_in_min_ms = defaults.barge_in_min_ms;
            self.barge_in_threshold_boost = defaults.barge_in_threshold_boost;
            self.wake_word_similarity_threshold = defaults.wake_word_similarity_threshold;
            self.whisper_threads = 0;
            self.whisper_initial_prompt = defaults.whisper_initial_prompt.clone();
        }
        if self.config_version < 5 {
            // v5: 350 ms of silence cut Sir off whenever he paused to breathe or think; 750 ms
            // (24 frames) plus utterance stitching lets him finish. Beam search, the live context
            // block and the larger tool set need more context.
            self.silence_hangover_ms = self.silence_hangover_ms.max(defaults.silence_hangover_ms);
            self.whisper_beam_size = defaults.whisper_beam_size;
            self.history_turns = self.history_turns.max(defaults.history_turns);
            self.num_ctx = self.num_ctx.max(defaults.num_ctx);
        }
        if self.config_version < 7 {
            // v7: WhatsApp, location, hivemind and Simulation Lab tools plus the working-memory
            // block need room; 12k tokens would truncate the system prompt.
            self.num_ctx = self.num_ctx.max(defaults.num_ctx);
        }
        if self.config_version < 8 {
            // v8: a 45 s conversation window (Sir never repeats "JAI" mid-dialogue), and the
            // film voice: the British blend at 1.06 instead of a single stock voice at 1.0.
            self.active_conversation_window_secs = self.active_conversation_window_secs.max(defaults.active_conversation_window_secs);
            // The old stock default only; a voice Sir picked himself stays.
            if matches!(self.kokoro_voice.trim(), "" | "bm_george") {
                self.kokoro_voice = defaults.kokoro_voice.clone();
            }
            if (self.kokoro_speed - 1.0).abs() < 0.01 {
                self.kokoro_speed = defaults.kokoro_speed;
            }
        }
        if self.config_version < 9 {
            // v9: the Fable/George film voice at 1.05, the frameless Stark window with its own
            // title bar, and the vocabulary prompt with the quick commands and Sonora/JaysRock.
            if self.kokoro_voice.trim() == V8_VOICE_BLEND {
                self.kokoro_voice = defaults.kokoro_voice.clone();
            }
            if (self.kokoro_speed - 1.06).abs() < 0.005 {
                self.kokoro_speed = defaults.kokoro_speed;
            }
            self.interface.frameless = true;
            // The native recorder makes real video: the old one-frame-a-second buffer becomes 12 fps.
            if self.features.replay_fps <= 1.0 {
                self.features.replay_fps = defaults.features.replay_fps;
            }
        }
        if self.config_version < 10 {
            // v10: Kokoro v1.0 with its voice pack (the only one with Fable), once both files are
            // installed; a model Sir configured himself stays.
            self.upgrade_kokoro(Path::new(crate::models::KOKORO_V1_MODEL).exists() && Path::new(crate::models::KOKORO_V1_VOICES).exists());
        }
        if self.config_version < 11 {
            // v11, latency: adaptive end of speech (220 ms after a short command, 450 ms after a
            // sentence), greedy Whisper with the encoder window scaled to the clip, and a 5 s
            // follow-up window after JAI has finished (45 s picked up unrelated room speech).
            self.silence_hangover_ms = defaults.silence_hangover_ms;
            self.short_command_hangover_ms = defaults.short_command_hangover_ms;
            self.short_command_max_ms = defaults.short_command_max_ms;
            self.active_conversation_window_secs = defaults.active_conversation_window_secs;
            self.whisper_beam_size = defaults.whisper_beam_size;
            self.whisper_min_audio_ctx = defaults.whisper_min_audio_ctx;
            self.whisper_dynamic_audio_ctx = true;
            // The light background capture (no cursor flicker); a lower rate Sir chose stays.
            self.features.replay_fps = self.features.replay_fps.min(defaults.features.replay_fps);
            // The clean two-zone studio layout (orb + conversation) on deep obsidian glass.
            self.interface.zen_mode = true;
            if matches!(self.interface.background.to_lowercase().as_str(), "#030509" | "#03070e") {
                self.interface.background = defaults.interface.background.clone();
            }
        }
        if self.config_version < 12 {
            // v12, voice input: the name prompt (no prompt made "JAI" unrecognisable) and the
            // beam search for sentences (greedy decoding looped and misheard names).
            if self.whisper_initial_prompt.trim().is_empty() {
                self.whisper_initial_prompt = defaults.whisper_initial_prompt.clone();
            }
            self.whisper_beam_size = self.whisper_beam_size.max(defaults.whisper_beam_size);
        }
        if self.config_version < 13 {
            // v13, zero-VRAM standby: the model is never pinned in VRAM forever; 90 s after the
            // last request Ollama lets go of it (a shorter keep-alive Sir set stays).
            if self.keep_alive < 0 || self.keep_alive > 90 {
                self.keep_alive = defaults.keep_alive;
            }
        }
        if self.config_version < 14 {
            // v14, film cadence: the stock 1.05× voice speed becomes 1.06× (Sir's own speed stays).
            if (self.kokoro_speed - 1.05).abs() < 1e-3 {
                self.kokoro_speed = 1.06;
            }
        }
        if self.config_version < 15 {
            // v15, the film voice: the 62/28/10 Fable-George-Lewis blend (`bm_jai_movie_1to1`)
            // through the 48 kHz mastering chain at 1.04× (the chain's pitch lock stretches 2 %, so
            // the synthesis itself runs faster). Only the stock voices and speeds move.
            if [crate::tts::LEGACY_VOICE_BLEND, crate::tts::JAI_VOICE_BLEND, crate::tts::V15_VOICE_BLEND, V8_VOICE_BLEND, ""].contains(&self.kokoro_voice.trim()) {
                self.kokoro_voice = defaults.kokoro_voice.clone();
            }
            if [1.0f32, 1.05, 1.06].iter().any(|v| (self.kokoro_speed - v).abs() < 1e-3) {
                self.kokoro_speed = defaults.kokoro_speed;
            }
        }
        if self.config_version < 16 {
            // v16: the HUD board is a fullscreen canvas (VIEW → FULLSCREEN or a double-click on the
            // canvas leaves it, and that choice is kept).
            self.interface.fullscreen = true;
        }
        if self.config_version < 17 {
            // v17, the reference-matched film voice: 75 % Fable / 25 % George at an unhurried 0.98×
            // (−0.55 semitones, dry studio chain). Only the stock voices and the stock tempo move.
            if [crate::tts::V15_VOICE_BLEND, crate::tts::LEGACY_VOICE_BLEND, ""].contains(&self.kokoro_voice.trim()) {
                self.kokoro_voice = crate::tts::JAI_VOICE.to_string();
            }
            if (self.kokoro_speed - crate::tts::V15_VOICE_SPEED).abs() < 1e-3 {
                self.kokoro_speed = crate::tts::JAI_VOICE_SPEED;
            }
        }
        if self.config_version < 18 {
            // v18, "core" heard as "car": the stock quantised small model gives way to the
            // full-precision one when it is on disk (pre-emphasis is gone for good).
            if self.whisper_model_path.replace('\\', "/") == "models/ggml-small-q8_0.bin" && Path::new("models/ggml-small.bin").exists() {
                self.whisper_model_path = "models/ggml-small.bin".to_string();
            }
        }
        if self.config_version < 20 {
            // v20: the prompt (≈ 21k tokens) had outgrown the 20k window and Ollama cut its start
            // off; the model's full 32k window (8-bit KV cache, same VRAM).
            self.num_ctx = self.num_ctx.max(defaults.num_ctx);
        }
        if self.config_version < 21 {
            // v21: the stock 90 s keep-alive and idle standby sent nearly every sentence to a cold
            // model; it now stays resident for 30 minutes (a game or sleep still evicts it), and
            // Batman: Arkham Knight counts as a game.
            let stock = crate::vram_guard::VramGuardConfig::default();
            if self.keep_alive == 90 {
                self.keep_alive = defaults.keep_alive;
            }
            let guard = &mut self.vram_guard;
            for secs in [&mut guard.idle_secs, &mut guard.keep_alive_secs] {
                if *secs == 90 {
                    *secs = crate::vram_guard::RESIDENT_SECS;
                }
            }
            for game in stock.game_processes {
                if !guard.game_processes.iter().any(|g| g.eq_ignore_ascii_case(&game)) {
                    guard.game_processes.push(game);
                }
            }
        }
        if self.config_version < 23 {
            // v23 (2026-11-09 lab, the same 21 requests on both): Qwen3.5 4B called the right tool
            // where the old 7B wrote tool names as text, got the sums right, and answers at
            // ≈ 65 tok/s in 3.9 GB instead of ≈ 45 tok/s in 5.5 GB. The stock "qwen3:30b-a3b" tag
            // (really Qwen2.5 7B) gives way to it and stays as the fallback's weights; a model Sir
            // chose himself stays.
            if self.primary_model == LEGACY_PRIMARY_MODEL {
                self.primary_model = defaults.primary_model.clone();
            }
            if is_qwen35(&self.primary_model) {
                self.num_ctx = self.num_ctx.max(QWEN35_NUM_CTX);
            }
        }
        if self.config_version < 24 {
            // v24 (2026-11-11): his name is J.A.I. ("Jay"). The stock wake words of before leave
            // (names Sir added himself stay), the stock window title follows, and the stock look
            // (or the yellow accent the 2026-11-09 helmet mishap left behind) becomes the new
            // amber default.
            self.wake_words.retain(|w| !STOCK_WAKE_WORDS.contains(&fingerprint(&w.trim().to_lowercase())));
            for name in DEFAULT_WAKE_WORDS.iter().rev() {
                if !self.wake_words.iter().any(|w| w.eq_ignore_ascii_case(name)) {
                    self.wake_words.insert(0, name.to_string());
                }
            }
            self.interface.renamed_v24();
        }
        if self.config_version < 25 {
            // v25 (2026-11-11): his files carry his name too. The stock paths follow; a path Sir
            // chose himself stays.
            if fingerprint(&self.memory_db_path) == STOCK_DB_PATH {
                self.memory_db_path = defaults.memory_db_path.clone();
            }
            if fingerprint(&self.log_file) == STOCK_LOG_FILE {
                self.log_file = defaults.log_file.clone();
            }
        }
        if self.config_version < 26 && fingerprint(self.kokoro_voice.trim()) == STOCK_VOICE_ALIAS {
            // v26: the film voice's alias carries his name of today.
            self.kokoro_voice = crate::tts::JAI_VOICE.to_string();
        }
        if self.config_version < 27 {
            // v27 (Sir, 2026-11-13: "he only answers to me except if I tell him otherwise"): the
            // voice lock is on. It holds as soon as his voice is known, and the bar for it is
            // read anew from the print (`speaker_id`).
            self.interface.voice_lock = true;
        }
        if self.config_version < 28 {
            // v28 (Sir, 2026-11-17): the whole HUD in the Stark look.
            self.interface.stark_v28();
        }
        if self.config_version < 22 && self.pre_roll_ms == 320 {
            // v22: 500 ms of audio before the onset, so "JAI" / "Move" keep their first
            // syllable.
            self.pre_roll_ms = defaults.pre_roll_ms;
        }
        // A stock prompt (v4-v21) becomes today's domain prompt; a prompt Sir wrote himself stays.
        let prompt = self.whisper_initial_prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        let stock = STOCK_PROMPTS.contains(&fingerprint(&prompt));
        if stock {
            self.whisper_initial_prompt = defaults.whisper_initial_prompt;
        }
        self.config_version = CONFIG_VERSION;
    }

    /// Moves the stock v0.19 model and voice pack to v1.0 (`available`: both v1.0 files exist).
    fn upgrade_kokoro(&mut self, available: bool) {
        let stock = self.kokoro_model_path.replace('\\', "/") == crate::models::KOKORO_V019_MODEL;
        if available && stock {
            self.kokoro_model_path = crate::models::KOKORO_V1_MODEL.to_string();
            self.kokoro_voices_archive_path = crate::models::KOKORO_V1_VOICES.to_string();
            info!("Kokoro TTS upgraded to v1.0 ({}, {})", self.kokoro_model_path, self.kokoro_voices_archive_path);
        } else if !available && !Path::new(&self.kokoro_model_path).exists() && Path::new(crate::models::KOKORO_V019_MODEL).exists() {
            // A fresh config on a machine that only has the v0.19 files keeps using them.
            self.kokoro_model_path = crate::models::KOKORO_V019_MODEL.to_string();
            self.kokoro_voices_archive_path = crate::models::KOKORO_V019_VOICES.to_string();
        }
    }

    fn sanitize(&mut self) {
        self.vad_start_threshold = self.vad_start_threshold.clamp(0.05, 0.98);
        self.vad_release_threshold = self
            .vad_release_threshold
            .clamp(0.02, self.vad_start_threshold);
        self.vad_noise_multiplier = self.vad_noise_multiplier.clamp(1.0, 10.0);
        self.vad_start_frames = self.vad_start_frames.clamp(1, 10);
        self.silence_hangover_ms = self.silence_hangover_ms.clamp(96, 3000);
        self.short_command_hangover_ms = self.short_command_hangover_ms.clamp(96, self.silence_hangover_ms);
        // A short burst never ends before its head start, nor later than a sentence would.
        self.short_command_commit_ms = self.short_command_commit_ms.clamp(self.short_command_hangover_ms, self.silence_hangover_ms);
        self.short_command_max_ms = self.short_command_max_ms.min(3000);
        self.reflex_hangover_ms = self.reflex_hangover_ms.clamp(96, self.short_command_hangover_ms);
        self.reflex_max_ms = self.reflex_max_ms.min(self.short_command_max_ms);
        self.reflex_min_ms = self.reflex_min_ms.min(self.reflex_max_ms);
        self.incomplete_thought_wait_ms = self.incomplete_thought_wait_ms.min(2000);
        self.whisper_beam_size = self.whisper_beam_size.clamp(0, 8);
        self.pre_roll_ms = self.pre_roll_ms.clamp(0, 1000);
        self.min_speech_ms = self.min_speech_ms.clamp(32, 2000);
        self.max_utterance_secs = self.max_utterance_secs.clamp(3, 60);
        self.noise_calibration_ms = self.noise_calibration_ms.clamp(256, 5000);
        self.barge_in_min_ms = self.barge_in_min_ms.clamp(64, 2000);
        self.barge_in_threshold_boost = self.barge_in_threshold_boost.clamp(0.0, 0.5);
        self.active_conversation_window_secs = self.active_conversation_window_secs.clamp(0, 120);
        self.wake_word_similarity_threshold = self.wake_word_similarity_threshold.clamp(0.5, 0.99);
        if self.wake_words.is_empty() {
            self.wake_words = DEFAULT_WAKE_WORDS.iter().map(|s| s.to_string()).collect();
        }
        self.loudness_pct = self.loudness_pct.min(200);
        self.kokoro_speed = self.kokoro_speed.clamp(0.5, 2.0);
        self.history_turns = self.history_turns.clamp(1, 50);
        self.num_ctx = self.num_ctx.clamp(2048, 131_072);
        self.max_reply_tokens = self.max_reply_tokens.clamp(32, 4096);
        if self.llm_port == 0 {
            self.llm_port = 11435;
        }
        self.whisper_languages.retain(|l| !l.trim().is_empty());
        self.repeat_penalty = self.repeat_penalty.clamp(1.0, 2.0);
        self.repeat_last_n = self.repeat_last_n.clamp(-1, 4096);
        self.presence_penalty = self.presence_penalty.clamp(0.0, 2.0);
        self.frequency_penalty = self.frequency_penalty.clamp(0.0, 2.0);
        if self.satellite.token.trim().len() < 16 {
            self.satellite.token = uuid::Uuid::new_v4().simple().to_string()[..24].to_string();
        }
        if self.satellite.port == 0 {
            self.satellite.port = 8443;
        }
        self.whatsapp.poll_seconds = self.whatsapp.poll_seconds.clamp(5, 600);
        if !matches!(self.whatsapp.bridge_kind.as_str(), "generic" | "waha") {
            self.whatsapp.bridge_kind = "generic".into();
        }
        self.location.geofence_arrive_m = self.location.geofence_arrive_m.clamp(30.0, 2000.0);
        self.location.geofence_leave_m = self.location.geofence_leave_m.clamp(self.location.geofence_arrive_m + 50.0, 20_000.0);
        self.location.commute_buffer_min = self.location.commute_buffer_min.min(120);
        self.acoustic.sensitivity = self.acoustic.sensitivity.clamp(0.5, 2.0);
        if !matches!(self.acoustic.clap_action.as_str(), "wake" | "toggle_music") {
            self.acoustic.clap_action = "wake".into();
        }
        self.interface.sanitize();
        for opt in [&mut self.preferred_mic, &mut self.preferred_speaker] {
            if opt
                .as_deref()
                .map(|s| s.trim().is_empty() || s.eq_ignore_ascii_case("auto"))
                .unwrap_or(false)
            {
                *opt = None;
            }
        }
    }

    /// Atomic write: temp file + rename, so a crash never leaves a half-written config. In a
    /// read-only run (`set_read_only`) nothing is written, and the unit tests never write Sir's
    /// settings file at all.
    pub fn save(&self) -> std::io::Result<()> {
        if READ_ONLY.load(std::sync::atomic::Ordering::Acquire) || cfg!(test) {
            return Ok(());
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = format!("{CONFIG_FILE}.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, CONFIG_FILE)
    }

    /// Whisper's threads: Sir's number, else 4-8 (with the encoder window scaled to a short
    /// clip there is too little work per step for more threads to pay off).
    pub fn whisper_thread_count(&self) -> i32 {
        if self.whisper_threads > 0 {
            self.whisper_threads
        } else {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(4, 8) as i32
        }
    }

    pub fn tts_thread_count(&self) -> usize {
        if self.tts_threads > 0 {
            self.tts_threads
        } else {
            inference_threads()
        }
    }
}

/// Measured on an 8-core/16-thread desktop: ¾ of the logical cores (12) beat both the physical
/// core count (8) and all logical cores (16, SMT contention ~3× slower). Without SMT (e.g. a
/// Raspberry Pi 5) every core is used.
pub fn inference_threads() -> usize {
    let logical = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    if logical >= 8 {
        (logical * 3 / 4).min(12)
    } else {
        logical
    }
}

/// The settings file is not to be touched in this run (the live self-test and the exhaustive
/// verifier change settings in memory only: silent, and whatever a probe switches).
static READ_ONLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Makes every save of the settings a no-op for the rest of this process.
pub fn set_read_only(on: bool) {
    READ_ONLY.store(on, std::sync::atomic::Ordering::Release);
}

/// Shared, thread-safe config with debounced persistence.
#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<RwLock<JaiConfig>>,
    dirty: Arc<tokio::sync::Notify>,
}

impl ConfigStore {
    pub fn new(cfg: JaiConfig) -> Self {
        Self {
            inner: Arc::new(RwLock::new(cfg)),
            dirty: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub fn snapshot(&self) -> JaiConfig {
        self.read(|c| c.clone())
    }

    pub fn read<R>(&self, f: impl FnOnce(&JaiConfig) -> R) -> R {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    /// Mutates the config and schedules a debounced save.
    pub fn update(&self, f: impl FnOnce(&mut JaiConfig)) {
        {
            let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
            f(&mut guard);
            guard.sanitize();
        }
        self.dirty.notify_one();
    }

    pub fn save_now(&self) {
        if let Err(e) = self.snapshot().save() {
            warn!("Could not save {CONFIG_FILE}: {e}");
        }
    }

    /// Background task: coalesces bursts of updates (e.g. slider drags) into one write.
    pub fn spawn_autosave(&self) {
        let store = self.clone();
        tokio::spawn(async move {
            loop {
                store.dirty.notified().await;
                tokio::time::sleep(Duration::from_millis(600)).await;
                store.save_now();
            }
        });
    }
}

static ORT_ENV: OnceLock<ort::environment::Environment> = OnceLock::new();

/// Process-wide ONNX Runtime environment shared by Silero VAD and Kokoro TTS.
pub fn ort_env() -> ort::Result<&'static ort::environment::Environment> {
    if let Some(env) = ORT_ENV.get() {
        return Ok(env);
    }
    let env = ort::init().with_name("jai").build()?;
    Ok(ORT_ENV.get_or_init(|| env))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_config_migrates_tuning_but_keeps_preferences() {
        let legacy = r#"{
            "hardware_profile": "DesktopRtx3060Ti",
            "primary_model": "qwen3:30b-a3b",
            "num_ctx": 2048,
            "vad_threshold": 0.38,
            "silence_end_of_speech_ms": 260,
            "max_utterance_secs": 18,
            "wake_word_similarity_threshold": 0.72,
            "kokoro_voice": "bm_lewis",
            "loudness_pct": 10,
            "preferred_mic": "Audio Mix (Elgato Virtual Audio)"
        }"#;
        let mut cfg: JaiConfig = serde_json::from_str(legacy).expect("legacy config parses");
        assert_eq!(cfg.config_version, 0, "a file without config_version must migrate");
        cfg.migrate();
        cfg.sanitize();
        assert_eq!(cfg.hardware_profile, HardwareProfile::Desktop);
        assert_eq!(cfg.vad_start_threshold, 0.50);
        assert_eq!(cfg.vad_release_threshold, 0.35);
        assert_eq!((cfg.silence_hangover_ms, cfg.short_command_hangover_ms, cfg.short_command_max_ms), (450, 220, 1100));
        assert_eq!(cfg.max_utterance_secs, 20);
        assert_eq!((cfg.primary_model.as_str(), cfg.num_ctx), (PRIMARY_MODEL, QWEN35_NUM_CTX), "the stock model gives way to Qwen3.5");
        assert_eq!(cfg.whisper_initial_prompt, WHISPER_DOMAIN_PROMPT, "the name and the short domain words");
        assert_eq!(cfg.whisper_beam_size, 2, "beam search for sentences");
        assert_eq!(cfg.active_conversation_window_secs, 5);
        assert_eq!(cfg.history_turns, 20);
        assert_eq!(cfg.kokoro_voice, "bm_lewis");
        assert_eq!(cfg.loudness_pct, 10);
        assert_eq!(cfg.preferred_mic.as_deref(), Some("Audio Mix (Elgato Virtual Audio)"));
        assert_eq!(cfg.config_version, CONFIG_VERSION);
    }

    #[test]
    fn v19_configs_get_the_full_32k_window() {
        let mut cfg: JaiConfig = serde_json::from_str(r#"{ "config_version": 19, "num_ctx": 20480, "keep_alive": 90 }"#).unwrap();
        cfg.migrate();
        cfg.sanitize();
        assert_eq!((cfg.num_ctx, cfg.config_version), (QWEN35_NUM_CTX, CONFIG_VERSION));
    }

    /// v23: the stock model becomes Qwen3.5 4B with its 40k window; Sir's own choice stays.
    #[test]
    fn v22_configs_move_to_qwen35_unless_sir_chose_a_model() {
        let stock = r#"{ "config_version": 22, "primary_model": "qwen3:30b-a3b", "fallback_model": "qwen2.5:7b-instruct", "num_ctx": 32768 }"#;
        let mut cfg: JaiConfig = serde_json::from_str(stock).unwrap();
        cfg.migrate();
        cfg.sanitize();
        assert_eq!((cfg.primary_model.as_str(), cfg.fallback_model.as_str(), cfg.num_ctx), (PRIMARY_MODEL, "qwen2.5:7b-instruct", QWEN35_NUM_CTX));
        let own = r#"{ "config_version": 22, "primary_model": "llama3.1:8b", "num_ctx": 32768 }"#;
        let mut cfg: JaiConfig = serde_json::from_str(own).unwrap();
        cfg.migrate();
        cfg.sanitize();
        assert_eq!((cfg.primary_model.as_str(), cfg.num_ctx), ("llama3.1:8b", 32_768));
    }

    #[test]
    fn v20_configs_keep_the_model_resident_and_know_batman() {
        let mut cfg: JaiConfig = serde_json::from_str(
            r#"{ "config_version": 20, "keep_alive": 90, "vram_guard": { "idle_secs": 90, "keep_alive_secs": 90, "game_processes": ["GTA5.exe", "MyGame.exe"] } }"#,
        )
        .unwrap();
        cfg.migrate();
        cfg.sanitize();
        let g = &cfg.vram_guard;
        assert_eq!((cfg.keep_alive, g.idle_secs, g.keep_alive_secs), (1800, 1800, 1800));
        assert!(g.game_processes.iter().any(|p| p == "BatmanAK.exe") && g.game_processes.iter().any(|p| p == "MyGame.exe"));
        // Sir's own shorter values stay.
        let mut own: JaiConfig = serde_json::from_str(r#"{ "config_version": 20, "keep_alive": 45, "vram_guard": { "idle_secs": 600 } }"#).unwrap();
        own.migrate();
        assert_eq!((own.keep_alive, own.vram_guard.idle_secs), (45, 600));
    }

    /// The stock values of earlier settings are known by fingerprint, so the name he had then
    /// is written nowhere. (The numbers were made from those values with the same sum.)
    #[test]
    fn earlier_stock_values_are_known_by_their_fingerprints() {
        assert_eq!(fingerprint("Hey Jay."), 0xa202_2afb_e1f5_133d, "FNV-1a, 64 bit");
        assert_eq!(fingerprint(""), 0xcbf2_9ce4_8422_2325);
        // Today's defaults are no earlier stock.
        let now = JaiConfig::default();
        assert!(!STOCK_PROMPTS.contains(&fingerprint(&now.whisper_initial_prompt)));
        assert!(now.wake_words.iter().all(|w| !STOCK_WAKE_WORDS.contains(&fingerprint(w))));
        assert!(fingerprint(&now.memory_db_path) != STOCK_DB_PATH && fingerprint(&now.log_file) != STOCK_LOG_FILE && fingerprint(&now.kokoro_voice) != STOCK_VOICE_ALIAS);
        // What Sir set himself comes through every migration untouched.
        let own = r##"{ "config_version": 23, "wake_words": ["friday"], "whisper_initial_prompt": "Sir's own words.", "memory_db_path": "D:/data/mine.db", "log_file": "mine.log", "kokoro_voice": "bm_george",
            "interface": { "theme": "crimson_mk42", "accent": "#ff2a45", "background": "#0f0507", "title": "Workshop", "hud_palette": "rose_crimson" } }"##;
        let mut cfg: JaiConfig = serde_json::from_str(own).unwrap();
        cfg.migrate();
        assert_eq!(cfg.wake_words, ["jai", "jay", "friday"], "his name in front, a name Sir added stays");
        assert_eq!((cfg.whisper_initial_prompt.as_str(), cfg.memory_db_path.as_str(), cfg.log_file.as_str(), cfg.kokoro_voice.as_str()), ("Sir's own words.", "D:/data/mine.db", "mine.log", "bm_george"));
        assert_eq!((cfg.interface.theme.as_str(), cfg.interface.hud_palette.as_str(), cfg.interface.title.as_str()), ("crimson_mk42", "rose_crimson", "Workshop"));
        assert_eq!(cfg.config_version, CONFIG_VERSION);
        // The yellow accent the 2026-11-09 helmet mishap left on the gold palette becomes the
        // default look.
        let mishap = r##"{ "config_version": 23, "interface": { "theme": "custom", "accent": "#ffe14d", "background": "#0f0507", "hud_palette": "stark_gold" } }"##;
        let mut cfg: JaiConfig = serde_json::from_str(mishap).unwrap();
        cfg.migrate();
        assert_eq!((cfg.interface.theme.as_str(), cfg.interface.hud_palette.as_str(), cfg.interface.accent.as_str(), cfg.interface.title.as_str()), ("jai", "stark_gold", "#ffd400", "J.A.I."));
        // A fresh install is his from the start.
        let fresh = JaiConfig::default();
        assert_eq!((fresh.wake_words.as_slice(), fresh.interface.hud_palette.as_str()), (["jai".to_string(), "jay".to_string()].as_slice(), "stark_gold"));
        assert!(fresh.name_meaning.is_empty(), "what the name stands for is Sir's to set, not the code's");
        assert_eq!(CONFIG_FILE, "jai_config.json");
    }

    #[test]
    fn the_stark_design_keeps_his_yellow_and_cuts_the_stock_corners_sharp() {
        // The look v24-v27 left on his PC (custom theme, the stock yellow, the stock palette).
        let stock = r##"{ "config_version": 27, "interface": { "theme": "custom", "accent": "#ffd400", "background": "#070b12", "hud_palette": "jai_gold", "ui_paradigm": "modern_oled_minimal", "corner_rounding": 18.0 } }"##;
        let mut cfg: JaiConfig = serde_json::from_str(stock).unwrap();
        cfg.migrate();
        let i = &cfg.interface;
        assert_eq!((i.theme.as_str(), i.accent.as_str(), i.background.as_str(), i.hud_palette.as_str()), ("custom", "#ffd400", "#070b12", "jai_gold"));
        assert_eq!(i.corner_rounding, 2.0, "the stock round corners are cut sharp");
        assert_eq!(i.ui_paradigm, crate::oled_minimal_ui::UiDesignParadigm::ModernOledMinimal, "his design stays");
        // A colour he picked himself after v24 is his.
        for (palette, accent) in [("jai_gold", "#00e5ff"), ("rose_crimson", "#ffd400"), ("custom:#FFD400", "#ffd400")] {
            let mine = format!(r##"{{ "config_version": 27, "interface": {{ "accent": "{accent}", "hud_palette": "{palette}", "corner_rounding": 9.0 }} }}"##);
            let mut cfg: JaiConfig = serde_json::from_str(&mine).unwrap();
            cfg.migrate();
            assert_eq!((cfg.interface.hud_palette.as_str(), cfg.interface.accent.as_str(), cfg.interface.corner_rounding), (palette, accent, 9.0));
        }
        // Arc Cyan was stored as "stark": the word names the Stark Red theme now, so it moves.
        let cyan = r##"{ "config_version": 27, "interface": { "theme": "stark", "accent": "#00e5ff", "hud_palette": "arc_cyan" } }"##;
        let mut cfg: JaiConfig = serde_json::from_str(cyan).unwrap();
        cfg.migrate();
        assert_eq!((cfg.interface.theme.as_str(), cfg.interface.hud_palette.as_str()), ("arc_cyan", "arc_cyan"));
        assert_eq!(CONFIG_VERSION, 28);
    }

    #[test]
    fn old_configs_get_the_v11_latency_tuning_but_keep_a_custom_prompt() {
        let v4 = r#"{ "config_version": 4, "silence_hangover_ms": 350, "history_turns": 15, "num_ctx": 8192,
                      "whisper_initial_prompt": "JAI, Sir's own words." }"#;
        let mut cfg: JaiConfig = serde_json::from_str(v4).expect("v4 config parses");
        cfg.migrate();
        assert_eq!(cfg.silence_hangover_ms, 450);
        assert_eq!(cfg.history_turns, 20);
        assert_eq!(cfg.whisper_initial_prompt, "JAI, Sir's own words.");
        for version in [4, 5, 8, 10] {
            let raw = format!(r#"{{ "config_version": {version}, "whisper_beam_size": 3, "silence_hangover_ms": 750, "whisper_min_audio_ctx": 256 }}"#);
            let mut cfg: JaiConfig = serde_json::from_str(&raw).unwrap();
            cfg.migrate();
            assert_eq!((cfg.whisper_beam_size, cfg.whisper_min_audio_ctx, cfg.silence_hangover_ms), (2, 128, 450), "v{version}");
        }
        // v11 files (no prompt, greedy) get the name prompt and the beam back.
        let v11 = r#"{ "config_version": 11, "whisper_initial_prompt": "", "whisper_beam_size": 1 }"#;
        let mut cfg: JaiConfig = serde_json::from_str(v11).unwrap();
        cfg.migrate();
        assert_eq!((cfg.whisper_initial_prompt.as_str(), cfg.whisper_beam_size), (WHISPER_DOMAIN_PROMPT, 2));
        let v10 = r##"{ "config_version": 10, "active_conversation_window_secs": 45, "features": { "replay_fps": 12.0 },
                       "interface": { "zen_mode": false, "background": "#030509" } }"##;
        let mut cfg: JaiConfig = serde_json::from_str(v10).unwrap();
        cfg.migrate();
        cfg.sanitize();
        assert_eq!(cfg.active_conversation_window_secs, 5);
        assert_eq!(cfg.features.replay_fps, 5.0);
        assert!(cfg.interface.zen_mode, "the two-zone studio layout");
        assert_eq!(cfg.interface.background, crate::interface::InterfaceSettings::default().background);
        assert!(cfg.short_command_hangover_ms <= cfg.silence_hangover_ms);
    }

    #[test]
    fn v7_config_gets_the_follow_up_window_and_the_film_voice() {
        let v7 = r#"{ "config_version": 7, "active_conversation_window_secs": 8, "kokoro_voice": "bm_george", "kokoro_speed": 1.0, "wake_word_required": false }"#;
        let mut cfg: JaiConfig = serde_json::from_str(v7).unwrap();
        cfg.migrate();
        cfg.sanitize();
        assert_eq!(cfg.active_conversation_window_secs, 5);
        assert_eq!(cfg.kokoro_voice, crate::tts::JAI_VOICE);
        assert!((cfg.kokoro_speed - crate::tts::JAI_VOICE_SPEED).abs() < 1e-6);
        assert!(cfg.addressee_detection, "on by default for files that predate it");
        assert!(!cfg.wake_word_required, "Sir's own choice stays");
        let custom = r#"{ "config_version": 7, "active_conversation_window_secs": 90, "kokoro_voice": "bf_emma", "kokoro_speed": 1.2 }"#;
        let mut cfg: JaiConfig = serde_json::from_str(custom).unwrap();
        cfg.migrate();
        assert_eq!((cfg.active_conversation_window_secs, cfg.kokoro_voice.as_str()), (5, "bf_emma"));
        assert!((cfg.kokoro_speed - 1.2).abs() < 1e-6);
    }

    #[test]
    fn v14_config_moves_to_the_movie_voice_and_its_tempo() {
        let v14 = r#"{ "config_version": 14, "kokoro_voice": "bm_fable:0.45+bm_george:0.4+bm_lewis:0.15", "kokoro_speed": 1.06 }"#;
        let mut cfg: JaiConfig = serde_json::from_str(v14).unwrap();
        cfg.migrate();
        assert_eq!(cfg.kokoro_voice, crate::tts::JAI_VOICE);
        assert!((cfg.kokoro_speed - crate::tts::JAI_VOICE_SPEED).abs() < 1e-6, "0.98× since v17");
        assert!(cfg.interface.fullscreen, "v16: the HUD board starts fullscreen");
        // v16 → v17: the stock 1.04× film tempo becomes the unhurried 0.98×, the old blend the alias.
        let v16 = r#"{ "config_version": 16, "kokoro_voice": "bm_fable:0.62+bm_george:0.28+bm_lewis:0.10", "kokoro_speed": 1.04 }"#;
        let mut cfg: JaiConfig = serde_json::from_str(v16).unwrap();
        cfg.migrate();
        assert_eq!(cfg.kokoro_voice, crate::tts::JAI_VOICE);
        assert!((cfg.kokoro_speed - 0.98).abs() < 1e-6);
        assert_eq!(cfg.config_version, CONFIG_VERSION);
        // Leaving fullscreen afterwards sticks.
        let windowed = r#"{ "config_version": 16, "interface": { "fullscreen": false } }"#;
        let mut cfg: JaiConfig = serde_json::from_str(windowed).unwrap();
        cfg.migrate();
        assert!(!cfg.interface.fullscreen);
        let custom = r#"{ "config_version": 14, "kokoro_voice": "bf_emma", "kokoro_speed": 1.2 }"#;
        let mut cfg: JaiConfig = serde_json::from_str(custom).unwrap();
        cfg.migrate();
        assert_eq!(cfg.kokoro_voice, "bf_emma", "Sir's own voice stays");
        assert!((cfg.kokoro_speed - 1.2).abs() < 1e-6);
    }

    #[test]
    fn v9_config_moves_to_kokoro_v1_only_when_installed() {
        let v9 = r#"{ "config_version": 9, "kokoro_model_path": "models/kokoro-v0_19.onnx", "kokoro_voices_archive_path": "models/voices_npz.bin" }"#;
        let mut cfg: JaiConfig = serde_json::from_str(v9).unwrap();
        cfg.upgrade_kokoro(false);
        assert_eq!(cfg.kokoro_model_path, "models/kokoro-v0_19.onnx", "nothing to upgrade to");
        cfg.upgrade_kokoro(true);
        assert_eq!((cfg.kokoro_model_path.as_str(), cfg.kokoro_voices_archive_path.as_str()), (crate::models::KOKORO_V1_MODEL, crate::models::KOKORO_V1_VOICES));
        let custom = r#"{ "config_version": 9, "kokoro_model_path": "D:/my/kokoro-custom.onnx", "kokoro_voices_archive_path": "D:/my/voices.bin" }"#;
        let mut cfg: JaiConfig = serde_json::from_str(custom).unwrap();
        cfg.upgrade_kokoro(true);
        assert_eq!(cfg.kokoro_model_path, "D:/my/kokoro-custom.onnx", "Sir's own model stays");
    }

    #[test]
    fn v8_config_gets_the_v9_voice_and_the_stark_window() {
        let v8 = r#"{ "config_version": 8, "kokoro_voice": "bm_george:0.45+bm_fable:0.35+bm_lewis:0.2", "kokoro_speed": 1.06, "interface": { "frameless": false } }"#;
        let mut cfg: JaiConfig = serde_json::from_str(v8).unwrap();
        cfg.migrate();
        assert_eq!(cfg.kokoro_voice, crate::tts::JAI_VOICE);
        assert!((cfg.kokoro_speed - crate::tts::JAI_VOICE_SPEED).abs() < 1e-6);
        assert!(cfg.interface.frameless, "the custom title bar");
        let custom = r#"{ "config_version": 8, "kokoro_voice": "bf_emma", "kokoro_speed": 1.2 }"#;
        let mut cfg: JaiConfig = serde_json::from_str(custom).unwrap();
        cfg.migrate();
        assert_eq!(cfg.kokoro_voice, "bf_emma");
        assert!((cfg.kokoro_speed - 1.2).abs() < 1e-6);
    }

    #[test]
    fn whisper_runs_with_the_domain_prompt_and_the_short_command_cutoff() {
        let cfg = JaiConfig::default();
        assert_eq!(cfg.whisper_initial_prompt, WHISPER_DOMAIN_PROMPT);
        // Short: a long vocabulary list makes Whisper invent words on quiet audio.
        assert!(cfg.whisper_initial_prompt.split_whitespace().count() <= 20);
        assert!(cfg.whisper_initial_prompt.contains("core"), "\"core\" is primed");
        assert_eq!(cfg.whisper_model_path, "models/ggml-small.bin");
        assert!(cfg.short_command_hangover_ms < cfg.silence_hangover_ms);
        assert!((4..=8).contains(&cfg.whisper_thread_count()) || cfg.whisper_threads > 0);
    }
}
