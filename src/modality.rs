//! Voice or text replies. Every input is tagged with where it came from (`InputSource`), and
//! Sir's `VoiceResponseMode` (persisted in `jai_memory.db`) decides per turn whether JAI
//! answers out loud (TTS + chat) or silently in the HUD chat only.
//!
//! Sir switches modes by voice or text ("only talk when I talk to you", "sei leise und schreib
//! nur"), with the `set_response_modality` tool, or from the HUD's mode selector; every path goes
//! through [`ModalityStore::set`], whose watch channel keeps the HUD in sync.

use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::watch;
use tracing::{info, warn};

const SETTING_KEY: &str = "voice_response_mode";

/// Where a user message came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputSource {
    /// Spoken into the microphone.
    Voice,
    /// Typed into the HUD chat (or a HUD button / widget action).
    TextGui,
}

impl InputSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Voice => "Voice (Sir spoke into the microphone)",
            Self::TextGui => "TextGui (Sir typed in the HUD chat)",
        }
    }
}

/// When JAI speaks his replies out loud.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VoiceResponseMode {
    /// Spoken input gets a spoken + written reply; typed input a written reply only.
    #[default]
    MirrorInput,
    /// Every reply is spoken, typed or not.
    AlwaysSpeak,
    /// Silent mode: replies only ever appear in the chat.
    TextOnly,
    /// A rule evaluated per turn; see [`Condition`].
    CustomCondition(String),
}

impl VoiceResponseMode {
    pub fn id(&self) -> &'static str {
        match self {
            Self::MirrorInput => "mirror_input",
            Self::AlwaysSpeak => "always_speak",
            Self::TextOnly => "text_only",
            Self::CustomCondition(_) => "custom_condition",
        }
    }

    pub fn display(&self) -> String {
        match self {
            Self::MirrorInput => "Mirror Input".into(),
            Self::AlwaysSpeak => "Always Speak".into(),
            Self::TextOnly => "Silent Text-Only".into(),
            Self::CustomCondition(c) => format!("Custom: {}", c.replace('_', " ")),
        }
    }

    /// `mode` id plus an optional condition (tool arguments, HUD).
    pub fn from_parts(mode: &str, condition: Option<&str>) -> Option<Self> {
        let m = mode.trim().to_lowercase().replace([' ', '-'], "_");
        Some(match m.as_str() {
            "mirror_input" | "mirror" | "mirrorinput" | "default" | "normal" => Self::MirrorInput,
            "always_speak" | "always" | "alwaysspeak" | "speak" | "voice" => Self::AlwaysSpeak,
            "text_only" | "textonly" | "silent" | "text" | "silent_text_only" | "mute" => Self::TextOnly,
            "custom_condition" | "custom" | "customcondition" | "condition" => {
                Self::CustomCondition(Condition::parse(condition.unwrap_or("")).id().to_string())
            }
            // A condition name given directly as the mode.
            other => match Condition::parse(other) {
                Condition::Unknown => return None,
                c => Self::CustomCondition(c.id().to_string()),
            },
        })
    }
}

/// Rules for `CustomCondition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// Speak only alerts (failures, warnings, finished timers); everything else is text.
    OnlyOnAlerts,
    /// Silent from 22:00 to 07:00, mirror the input otherwise.
    QuietAtNight,
    /// Silent while a full-screen app (game, video) is in front, mirror the input otherwise.
    MuteWhenFullscreen,
    /// Speak only when Sir explicitly asks for it ("read it out", "sag es laut").
    OnlyWhenExplicitlyAsked,
    Unknown,
}

impl Condition {
    pub fn parse(raw: &str) -> Self {
        let s = raw.trim().to_lowercase().replace([' ', '-'], "_");
        let has = |words: &[&str]| words.iter().any(|w| s.contains(w));
        if s.is_empty() {
            Self::Unknown
        } else if has(&["alert", "urgent", "emergency", "warning", "notfall", "alarm", "warnung", "wichtig", "dringend"]) {
            Self::OnlyOnAlerts
        } else if has(&["night", "nacht", "quiet_hours", "sleep_hours", "ruhezeit"]) {
            Self::QuietAtNight
        } else if has(&["fullscreen", "full_screen", "vollbild", "gaming", "game", "spiel", "movie", "film"]) {
            Self::MuteWhenFullscreen
        } else if has(&["explicit", "asked", "ask", "request", "when_told", "ausdrücklich", "gefragt"]) {
            Self::OnlyWhenExplicitlyAsked
        } else {
            Self::Unknown
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::OnlyOnAlerts => "only_on_alerts",
            Self::QuietAtNight => "quiet_at_night",
            Self::MuteWhenFullscreen => "mute_when_fullscreen_app_active",
            Self::OnlyWhenExplicitlyAsked => "only_when_explicitly_asked",
            Self::Unknown => "unknown",
        }
    }
}

/// What a reply is answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Input(InputSource),
    /// JAI speaking on his own (greeting, wake acknowledgement).
    System,
    /// Something Sir must hear about (finished timer, failure).
    Alert,
    /// Always audible (the HUD's "Test voice" button).
    Forced,
}

/// Per-turn decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceGate {
    Speak,
    Silent,
    /// `only_on_alerts`: stay silent unless the turn turns out to be an alert.
    SpeakIfAlert,
}

impl VoiceGate {
    pub fn speaks(self) -> bool {
        self == Self::Speak
    }
}

/// Facts the custom conditions look at.
#[derive(Debug, Clone, Copy)]
pub struct GateContext<'a> {
    pub text: &'a str,
    pub hour: u32,
    pub fullscreen_app: bool,
    pub voice_muted: bool,
}

pub fn gate(mode: &VoiceResponseMode, trigger: Trigger, ctx: &GateContext<'_>) -> VoiceGate {
    use VoiceGate::{Silent, Speak, SpeakIfAlert};
    if trigger == Trigger::Forced {
        return Speak;
    }
    if ctx.voice_muted {
        return Silent;
    }
    let mirror = |source: Option<InputSource>| match source {
        Some(InputSource::TextGui) => Silent,
        _ => Speak,
    };
    let source = match trigger {
        Trigger::Input(s) => Some(s),
        _ => None,
    };
    match mode {
        VoiceResponseMode::TextOnly => Silent,
        VoiceResponseMode::AlwaysSpeak => Speak,
        VoiceResponseMode::MirrorInput => mirror(source),
        VoiceResponseMode::CustomCondition(c) => {
            if trigger == Trigger::Alert {
                return Speak;
            }
            let night = ctx.hour >= 22 || ctx.hour < 7;
            match Condition::parse(c) {
                Condition::OnlyOnAlerts => SpeakIfAlert,
                Condition::QuietAtNight if night => Silent,
                Condition::MuteWhenFullscreen if ctx.fullscreen_app => Silent,
                Condition::OnlyWhenExplicitlyAsked => {
                    if source.is_some() && explicitly_asked(ctx.text) {
                        Speak
                    } else {
                        Silent
                    }
                }
                _ if trigger == Trigger::System => Speak,
                _ => mirror(source),
            }
        }
    }
}

/// "Read it out", "say it out loud", "sag es laut", "lies vor".
pub fn explicitly_asked(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "out loud", "aloud", "say it", "speak it", "read it out", "read it to me", "read out", "read that out", "tell me out loud",
        "answer out loud", "speak up", "laut vor", "vorlesen", "lies vor", "lies es vor", "sag es", "sag mir laut", "sprich es",
        "antworte laut", "laut antworten", "laut sagen",
    ]
    .iter()
    .any(|p| t.contains(p))
}

/// Replies that count as alerts for `only_on_alerts`.
pub fn sounds_like_alert(text: &str) -> bool {
    let t = text.to_lowercase();
    ["warning", "urgent", "alert", "critical", "danger", "immediately", "emergency", "failed", "overheat", "warnung", "achtung", "dringend"]
        .iter()
        .any(|w| t.contains(w))
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid modality regex")
}

static ALERTS_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:only|just)\s+(?:speak|talk|answer|reply|respond)?\s*(?:out loud\s+|aloud\s+)?(?:if|when|for|on|with)\s+(?:it\s*is\s+|its\s+|it s\s+|there\s+is\s+|there s\s+|theres\s+)?(?:an?\s+)?(?:alerts?|urgent|emergenc|important|warnings?|critical)|\bnur\s+(?:noch\s+)?(?:bei|für|fuer)\s+(?:alarm|warnung|notf|wichtig|dringend)|\b(?:sprich|rede|antworte)\s+nur\s+(?:laut\s+)?(?:bei|wenn es)\s+(?:alarm|warnung|notf|wichtig|dringend)")
});
static QUIET_NIGHT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:be\s+)?(?:quiet|silent)\s+(?:at|during the|in the)\s+night\b|\bnachts\s+(?:leise|still|ruhig|stumm)|\bquiet at night\b")
});
static QUIET_FULLSCREEN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:mute|quiet|silent)\b.*\b(?:full ?screen|gaming|playing|a game|watching)\b|\b(?:leise|still|stumm)\b.*\b(?:vollbild|spiele|zocke|zocken)\b")
});
static EXPLICIT_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\bonly\s+(?:speak|talk|answer out loud|read)\b.*\bwhen\s+i\s+(?:explicitly\s+)?(?:ask|tell)\b|\bonly when (?:explicitly )?asked\b|\bnur\s+(?:sprechen|reden)\s+wenn\s+ich\s+(?:dich\s+)?(?:frage|darum bitte|es sage)")
});
static ALWAYS_SPEAK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\balways\s+(?:answer|talk|speak|reply|respond)(?:\s+(?:out loud|aloud|by voice|to me))?\b|\b(?:speak|talk|answer|reply)\b.*\beven (?:when|if) i (?:type|write)\b|\bspeak (?:all|every) (?:answers|responses|replies)\b|\b(?:antworte|sprich|rede)\s+immer(?:\s+(?:laut|sprechend|mit stimme))?\b|\bimmer\s+(?:laut\s+)?(?:antworten|sprechen|reden)\b|\bauch wenn ich (?:schreibe|tippe)\b")
});
static TEXT_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:only|just)\s+(?:write|type|text)(?:\s+(?:to me|me|back|in text|in the chat|in chat|from now on|instead|answers|replies))*$|\btext[ -]only\b|\b(?:answer|reply|respond|talk)\s+(?:only\s+)?(?:in|by|via)\s+(?:text|writing|chat)(?:\s+only)?\b|\bsilent mode\b|\bsilence mode\b|\b(?:be\s+)?(?:quiet|silent)\s+and\s+(?:only\s+)?(?:write|type|text)\b|\b(?:dont|don t|do not|stop)\s+(?:talk|talking|speak|speaking)\s+(?:anymore|at all|out loud)\b|\b(?:stop talking|stop speaking)\b.+\b(?:write|text|chat)\b|\bsei\s+(?:leise|still|ruhig)\s+und\s+schreib|\bschreib\w*\s+nur\b|\bnur\s+(?:noch\s+)?(?:schreiben|text|im chat|schriftlich)\b|\bstumm(?:modus|schaltung)\b|\bnicht mehr (?:sprechen|reden)\b")
});
static MIRROR: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\bonly\s+(?:talk|speak|answer|reply|respond)\w*(?:\s+\w+){0,3}?\s+when\s+i\s+(?:talk|speak)\b|\bwhen i (?:write|type)\b.*\b(?:dont|don t|do not|no|not)\b.*\b(?:talk|speak)\b|\bmirror (?:my )?(?:input|mode)\b|\b(?:antworte|sprich|rede)\s+nur\s+(?:sprechend|laut|mit stimme)?\s*wenn\s+ich\s+(?:rede|spreche)\b|\bnur\s+(?:sprechen|reden|laut antworten)\s+wenn\s+ich\s+(?:rede|spreche)\b|\bwenn ich schreibe\b.*\bnicht\b.*\b(?:sprechen|reden)\b")
});

/// A natural request to change the reply mode (English, German, Denglisch).
pub fn parse_command(text: &str) -> Option<VoiceResponseMode> {
    let s: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if s.is_empty() {
        return None;
    }
    let custom = |c: Condition| Some(VoiceResponseMode::CustomCondition(c.id().to_string()));
    if ALERTS_ONLY.is_match(&s) {
        return custom(Condition::OnlyOnAlerts);
    }
    if QUIET_NIGHT.is_match(&s) {
        return custom(Condition::QuietAtNight);
    }
    if QUIET_FULLSCREEN.is_match(&s) {
        return custom(Condition::MuteWhenFullscreen);
    }
    if EXPLICIT_ONLY.is_match(&s) {
        return custom(Condition::OnlyWhenExplicitlyAsked);
    }
    if ALWAYS_SPEAK.is_match(&s) {
        return Some(VoiceResponseMode::AlwaysSpeak);
    }
    if MIRROR.is_match(&s) {
        return Some(VoiceResponseMode::MirrorInput);
    }
    if TEXT_ONLY.is_match(&s) {
        return Some(VoiceResponseMode::TextOnly);
    }
    None
}

/// In-character confirmation of a mode change.
pub fn acknowledgement(mode: &VoiceResponseMode) -> String {
    match mode {
        VoiceResponseMode::MirrorInput => {
            "Understood, Sir. I'll answer out loud when you speak, and in writing when you type.".into()
        }
        VoiceResponseMode::AlwaysSpeak => "Very good, Sir. I'll answer out loud, even when you type.".into(),
        VoiceResponseMode::TextOnly => "Very well, Sir. I'll keep to the chat and stay silent.".into(),
        VoiceResponseMode::CustomCondition(c) => match Condition::parse(c) {
            Condition::OnlyOnAlerts => "Understood, Sir. I'll only speak up for alerts; everything else goes to the chat.".into(),
            Condition::QuietAtNight => "Of course, Sir. I'll stay silent at night and answer in writing.".into(),
            Condition::MuteWhenFullscreen => "Noted, Sir. I'll keep quiet while a full-screen application is running.".into(),
            Condition::OnlyWhenExplicitlyAsked => "Very well, Sir. I'll only speak when you ask me to.".into(),
            Condition::Unknown => "Noted, Sir. I've stored that rule; otherwise I'll mirror how you talk to me.".into(),
        },
    }
}

/// The persisted mode plus the session-only "mute my voice" switch.
#[derive(Clone)]
pub struct ModalityStore {
    conn: Option<Arc<Mutex<Connection>>>,
    mode: Arc<RwLock<VoiceResponseMode>>,
    voice_muted: Arc<AtomicBool>,
    changes: watch::Sender<VoiceResponseMode>,
}

impl ModalityStore {
    /// Opens (or creates) the `app_settings` table in JAI's memory database.
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(db_path.as_ref())?;
        conn.busy_timeout(Duration::from_secs(3))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS app_settings (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL,
                 updated_at TEXT NOT NULL DEFAULT (datetime('now'))
             );",
        )?;
        let stored: Option<String> = conn
            .query_row("SELECT value FROM app_settings WHERE key = ?1", params![SETTING_KEY], |r| r.get(0))
            .optional()?;
        let mode = stored
            .and_then(|raw| serde_json::from_str::<VoiceResponseMode>(&raw).ok())
            .unwrap_or_default();
        info!("[MODE] Voice response mode: {}", mode.display());
        let (changes, _) = watch::channel(mode.clone());
        Ok(Self {
            conn: Some(Arc::new(Mutex::new(conn))),
            mode: Arc::new(RwLock::new(mode)),
            voice_muted: Arc::new(AtomicBool::new(false)),
            changes,
        })
    }

    /// Not persisted (tests, self-test).
    pub fn in_memory() -> Self {
        let (changes, _) = watch::channel(VoiceResponseMode::default());
        Self {
            conn: None,
            mode: Arc::new(RwLock::new(VoiceResponseMode::default())),
            voice_muted: Arc::new(AtomicBool::new(false)),
            changes,
        }
    }

    pub fn get(&self) -> VoiceResponseMode {
        self.mode.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Stores and broadcasts a new mode. Returns false when nothing changed.
    pub fn set(&self, mode: VoiceResponseMode) -> bool {
        {
            let mut current = self.mode.write().unwrap_or_else(|e| e.into_inner());
            if *current == mode {
                return false;
            }
            *current = mode.clone();
        }
        info!("[MODE] Voice response mode → {}", mode.display());
        if let Some(conn) = &self.conn {
            let raw = serde_json::to_string(&mode).unwrap_or_default();
            if let Err(e) = conn.lock().unwrap_or_else(|e| e.into_inner()).execute(
                "INSERT INTO app_settings (key, value, updated_at) VALUES (?1, ?2, datetime('now'))
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![SETTING_KEY, raw],
            ) {
                warn!("[MODE] Could not persist the voice response mode: {e}");
            }
        }
        self.changes.send_replace(mode);
        true
    }

    pub fn subscribe(&self) -> watch::Receiver<VoiceResponseMode> {
        self.changes.subscribe()
    }

    pub fn voice_muted(&self) -> bool {
        self.voice_muted.load(Ordering::Acquire)
    }

    /// `mute_tts` / `unmute_tts`: session-only, on top of the mode.
    pub fn set_voice_muted(&self, muted: bool) {
        self.voice_muted.store(muted, Ordering::Release);
        // Re-broadcast so the HUD shows the muted state immediately.
        self.changes.send_replace(self.get());
    }

    /// The decision for a turn triggered by `trigger` with Sir's words `text`.
    pub fn gate(&self, trigger: Trigger, text: &str) -> VoiceGate {
        use chrono::Timelike;
        let mode = self.get();
        let needs_fullscreen = matches!(&mode, VoiceResponseMode::CustomCondition(c) if Condition::parse(c) == Condition::MuteWhenFullscreen);
        let ctx = GateContext {
            text,
            hour: chrono::Local::now().hour(),
            fullscreen_app: needs_fullscreen && fullscreen_app_active(),
            voice_muted: self.voice_muted(),
        };
        gate(&mode, trigger, &ctx)
    }
}

#[cfg(windows)]
fn fullscreen_app_active() -> bool {
    crate::win32::foreground_is_fullscreen()
}

#[cfg(not(windows))]
fn fullscreen_app_active() -> bool {
    false
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "set_response_modality",
        "description": "Choose when YOU answer out loud. mirror_input (default): speak when Sir speaks, text only when he types. always_speak: always out loud. text_only: never speak, chat only. custom_condition: a rule such as only_on_alerts, quiet_at_night, mute_when_fullscreen_app_active or only_when_explicitly_asked. Call it whenever Sir asks you to talk, stop talking, be silent or only write.",
        "parameters": {"type": "object", "properties": {
            "mode": {"type": "string", "enum": ["mirror_input", "always_speak", "text_only", "custom_condition"]},
            "custom_condition": {"type": "string", "description": "For custom_condition: only_on_alerts | quiet_at_night | mute_when_fullscreen_app_active | only_when_explicitly_asked"}
        }, "required": ["mode"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(text: &str) -> GateContext<'_> {
        GateContext { text, hour: 14, fullscreen_app: false, voice_muted: false }
    }

    #[test]
    fn mirror_input_speaks_only_to_voice() {
        let m = VoiceResponseMode::MirrorInput;
        assert_eq!(gate(&m, Trigger::Input(InputSource::Voice), &ctx("hi")), VoiceGate::Speak);
        assert_eq!(gate(&m, Trigger::Input(InputSource::TextGui), &ctx("hi")), VoiceGate::Silent);
        assert_eq!(gate(&m, Trigger::System, &ctx("")), VoiceGate::Speak);
        assert_eq!(VoiceResponseMode::default(), VoiceResponseMode::MirrorInput);
    }

    #[test]
    fn always_text_only_and_mute() {
        let typed = Trigger::Input(InputSource::TextGui);
        let spoken = Trigger::Input(InputSource::Voice);
        assert_eq!(gate(&VoiceResponseMode::AlwaysSpeak, typed, &ctx("x")), VoiceGate::Speak);
        assert_eq!(gate(&VoiceResponseMode::TextOnly, spoken, &ctx("x")), VoiceGate::Silent);
        assert_eq!(gate(&VoiceResponseMode::TextOnly, Trigger::Alert, &ctx("x")), VoiceGate::Silent);
        assert_eq!(gate(&VoiceResponseMode::TextOnly, Trigger::Forced, &ctx("x")), VoiceGate::Speak);
        let muted = GateContext { voice_muted: true, ..ctx("x") };
        assert_eq!(gate(&VoiceResponseMode::AlwaysSpeak, spoken, &muted), VoiceGate::Silent);
    }

    #[test]
    fn custom_conditions() {
        let spoken = Trigger::Input(InputSource::Voice);
        let alerts = VoiceResponseMode::CustomCondition("only_on_alerts".into());
        assert_eq!(gate(&alerts, spoken, &ctx("x")), VoiceGate::SpeakIfAlert);
        assert_eq!(gate(&alerts, Trigger::Alert, &ctx("x")), VoiceGate::Speak);
        let night = VoiceResponseMode::CustomCondition("quiet_at_night".into());
        assert_eq!(gate(&night, spoken, &GateContext { hour: 23, ..ctx("x") }), VoiceGate::Silent);
        assert_eq!(gate(&night, spoken, &GateContext { hour: 9, ..ctx("x") }), VoiceGate::Speak);
        let fs = VoiceResponseMode::CustomCondition("mute_when_fullscreen_app_active".into());
        assert_eq!(gate(&fs, spoken, &GateContext { fullscreen_app: true, ..ctx("x") }), VoiceGate::Silent);
        let explicit = VoiceResponseMode::CustomCondition("only_when_explicitly_asked".into());
        assert_eq!(gate(&explicit, spoken, &ctx("what's the time")), VoiceGate::Silent);
        assert_eq!(gate(&explicit, spoken, &ctx("read it out to me")), VoiceGate::Speak);
    }

    #[test]
    fn natural_mode_commands() {
        use VoiceResponseMode::*;
        let p = parse_command;
        assert_eq!(p("Only talk when I talk to you"), Some(MirrorInput));
        assert_eq!(p("Antworte nur sprechend wenn ich rede"), Some(MirrorInput));
        assert_eq!(
            p("JAI, can you please only answer me talking when I talk to you, so when I write to you, you don't talk back."),
            Some(MirrorInput)
        );
        assert_eq!(p("Stop talking, only write in text"), Some(TextOnly));
        assert_eq!(p("Sei leise und schreib nur"), Some(TextOnly));
        assert_eq!(p("Answer in text only"), Some(TextOnly));
        assert_eq!(p("Always answer out loud, even when I type"), Some(AlwaysSpeak));
        assert_eq!(p("Antworte immer laut"), Some(AlwaysSpeak));
        assert_eq!(p("Only speak out loud if it's an alert or urgent"), Some(CustomCondition("only_on_alerts".into())));
        assert_eq!(p("Be quiet at night"), Some(CustomCondition("quiet_at_night".into())));
        assert_eq!(p("Mute yourself when I'm gaming in fullscreen"), Some(CustomCondition("mute_when_fullscreen_app_active".into())));
        assert_eq!(p("Open Spotify"), None);
        assert_eq!(p("Stop talking"), None, "a bare stop only silences the current reply");
        assert_eq!(p("Can you only write the summary into a widget"), None);
        assert_eq!(p("What do you think about text editors?"), None);
    }

    #[test]
    fn modes_round_trip_through_json_and_tool_args() {
        let m = VoiceResponseMode::CustomCondition("quiet_at_night".into());
        let raw = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<VoiceResponseMode>(&raw).unwrap(), m);
        assert_eq!(VoiceResponseMode::from_parts("text_only", None), Some(VoiceResponseMode::TextOnly));
        assert_eq!(
            VoiceResponseMode::from_parts("custom_condition", Some("only on alerts")),
            Some(VoiceResponseMode::CustomCondition("only_on_alerts".into()))
        );
        assert_eq!(VoiceResponseMode::from_parts("quiet_at_night", None), Some(m));
        assert_eq!(VoiceResponseMode::from_parts("banana", None), None);
    }
}
