//! Instant-command fast path (< 1 ms, no LLM round-trip) for obvious requests in English,
//! German or Denglisch: stop, quitting Jarvis, mute/volume, media keys, restyling the HUD,
//! open/close apps, window actions, time/date and system status. Everything else goes to the
//! LLM with native tool calling.

use crate::interface::{self, HudLayout, HudPanel, WindowCommand};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// "Stop", "Sei still", "Halt" – silence Jarvis, nothing else.
    Stop,
    Time,
    Date,
    /// A deterministic tool call with a canned confirmation.
    Tool { name: &'static str, args: Value },
    /// An answer computed on the spot (arithmetic, unit conversion).
    Answer(String),
    /// A short operational command carried out without a word: no speech, no language model,
    /// only a small badge in the HUD ("pause", "mute", "volume 40", "orb mode").
    Silent(SilentAction),
}

/// What a silent quick command does.
#[derive(Debug, Clone, PartialEq)]
pub struct SilentAction {
    pub tool: &'static str,
    pub args: Value,
    /// The HUD badge ("Paused").
    pub label: String,
}

/// Lower-case words without punctuation, the wake word and polite fillers removed.
fn quick_words(text: &str) -> String {
    let words = raw_words(text);
    let kept: Vec<&str> = words
        .split_whitespace()
        .filter(|w| !matches!(*w, "jarvis" | "please" | "bitte" | "mal" | "now" | "jetzt" | "sir" | "the" | "die" | "den" | "das"))
        .collect();
    kept.join(" ")
}

/// "Pause", "mute", "louder", "volume 40", "lautstärke 30", "next", "orb mode" …: carried out in
/// milliseconds without speech or the language model. Only whole, short utterances qualify.
pub fn silent_quick_command(text: &str) -> Option<SilentAction> {
    let s = quick_words(text);
    if s.is_empty() || s.split_whitespace().count() > 4 {
        return None;
    }
    let act = |tool: &'static str, args: Value, label: &str| Some(SilentAction { tool, args, label: label.to_string() });
    let music = |action: &str, label: &str| act("control_music", json!({ "action": action }), label);
    let mixer = |args: Value, label: &str| act("control_audio_mixer", args, label);
    static VOLUME: LazyLock<Regex> =
        LazyLock::new(|| re(r"^(?:set\s+)?(?:volume|lautstärke|lautstaerke|sound|ton)\s+(?:to\s+|auf\s+|at\s+)?(\d{1,3})\s*(?:%|percent|prozent)?$|^(\d{1,3})\s*(?:%|percent|prozent)$"));
    if let Some(c) = VOLUME.captures(&s) {
        let level: u64 = c.get(1).or(c.get(2))?.as_str().parse().ok()?;
        let level = level.min(100);
        return mixer(json!({ "action": "set_master", "level": level }), &format!("{level} %"));
    }
    match s.as_str() {
        // Whatever is playing — every media session at once — and Jarvis' own voice.
        "pause" | "pause music" | "pause music please" | "pausieren" | "pausiere" | "musik pause" | "musik pausieren" | "pause it" | "pause playback"
        | "pause everything" | "pause all" | "paus" | "pos" => music("stop_all", "Paused"),
        "stop" | "stopp" | "halt" | "stop music" | "stop it" | "stoppe" | "musik stopp" | "musik stoppen" | "stopp musik" | "musik aus" | "stop playing"
        | "stop playback" | "stop everything" | "stop all" | "stop that" => music("stop_all", "Stopped"),
        "play" | "resume" | "continue" | "weiter" | "play music" | "fortsetzen" | "spiel weiter" | "musik weiter" | "unpause" | "resume music" | "weitermachen"
        | "mach weiter" | "weiter machen" | "weiterspielen" => {
            music("resume", "Playing")
        }
        "next" | "skip" | "next song" | "next track" | "skip song" | "skip it" | "nächster" | "nächstes" | "nächster song" | "nächstes lied" | "naechster" => {
            music("next_track", "Next")
        }
        "previous" | "back" | "previous song" | "previous track" | "last song" | "zurück" | "vorheriges" | "vorheriges lied" | "go back" => {
            music("previous_track", "Previous")
        }
        "mute" | "stumm" | "stummschalten" | "ton aus" | "mute all" | "mute sound" | "lautlos" | "silence" | "sound off" | "mute pc" | "mute the pc" => {
            mixer(json!({ "action": "mute_master" }), "Muted")
        }
        "unmute" | "ton an" | "ton wieder an" | "laut schalten" | "unmute sound" | "sound on" | "unmute pc" | "unmute the pc" => {
            mixer(json!({ "action": "unmute_master" }), "Unmuted")
        }
        "louder" | "lauter" | "volume up" | "turn it up" | "mach lauter" | "a bit louder" => mixer(json!({ "action": "change_master", "delta": 10 }), "+10 %"),
        "quieter" | "leiser" | "volume down" | "turn it down" | "mach leiser" | "a bit quieter" | "softer" => {
            mixer(json!({ "action": "change_master", "delta": -10 }), "−10 %")
        }
        // A maximised board block goes back to the board first.
        "minimize" | "minimise" | "minimieren" if crate::hud_board::spotlight_active() => {
            act("hud_board", json!({ "action": "minimize" }), "Back to the board")
        }
        "minimize" | "minimise" | "minimieren" => act("window_management", json!({ "action": "minimize" }), "Minimised"),
        "close" | "schließen" | "schliessen" => act("configure_interface", json!({ "window": "hide" }), "To the tray"),
        "orb mode" | "orb" | "orb only" | "only orb" | "orbmodus" | "orb modus" | "pure orb" => act("control_orb_mode", json!({ "enabled": true }), "Orb mode"),
        "normal mode" | "full mode" | "show chat" | "full view" | "normaler modus" | "normalmodus" | "exit orb mode" => {
            act("control_orb_mode", json!({ "enabled": false }), "Full HUD")
        }
        _ => None,
    }
}

/// "Stop", "pause", "stopp", "halt", "stop music", "Jarvis, pause" (and Whisper's "post" / "paws",
/// already corrected to "pause"): stop everything that is playing, at once and silently.
pub fn is_media_stop(text: &str) -> bool {
    silent_quick_command(text).is_some_and(|a| a.tool == "control_music" && a.args["action"] == "stop_all")
}

/// "What's up? Can you change the interface to red?": several sentences of which exactly one is
/// a command and the rest small talk (at most four words each, no "don't" / "wait"). Runs when
/// the whole text is no command.
pub fn parse_sentences(text: &str) -> Option<Intent> {
    let sentences: Vec<&str> = text.split_inclusive(['.', '?', '!']).map(str::trim).filter(|s| !s.is_empty()).collect();
    if sentences.len() < 2 {
        return None;
    }
    let mut found: Option<(usize, Intent)> = None;
    for (i, sentence) in sentences.iter().enumerate() {
        if let Some(intent @ (Intent::Tool { .. } | Intent::Silent(_))) = parse(sentence).or_else(|| silent_quick_command(sentence).map(Intent::Silent)) {
            if found.is_some() {
                return None;
            }
            found = Some((i, intent));
        }
    }
    let (index, intent) = found?;
    let small_talk = sentences.iter().enumerate().filter(|(i, _)| *i != index).all(|(_, s)| {
        let w = raw_words(s);
        w.split_whitespace().count() <= 4 && !["dont", "don t", "do not", "wait", "nicht", "never", "no ", "nein", "stop"].iter().any(|n| w.contains(n))
    });
    small_talk.then_some(intent)
}

/// The tools a garbled sentence may still reach (`parse_misheard`): Jarvis' own HUD, board, views
/// and media — never an app launch or close, which a misheard sentence must not trigger.
const MISHEARD_SAFE_TOOLS: &[&str] = &[
    "configure_interface", "control_jarvis_interface", "hud_board", "open_jarvis_view", "control_orb_mode", "media_and_volume_control", "control_music", "set_listening_mode",
];

/// A third-person verb at the start made imperative ("opens your settings" → "open …").
fn third_person_to_imperative(clause: &str) -> String {
    const VERBS: &[(&str, &str)] = &[
        ("opens", "open"), ("closes", "close"), ("shows", "show"), ("hides", "hide"), ("changes", "change"), ("makes", "make"), ("turns", "turn"),
        ("switches", "switch"), ("sets", "set"), ("puts", "put"), ("brings", "bring"), ("plays", "play"), ("pauses", "pause"), ("mutes", "mute"),
    ];
    let mut words = clause.split_whitespace();
    let Some(first) = words.next() else { return String::new() };
    let lower = first.to_lowercase();
    let head = VERBS.iter().find(|(v, _)| lower == *v).map(|(_, base)| base.to_string()).unwrap_or_else(|| first.to_string());
    std::iter::once(head).chain(words.map(String::from)).collect::<Vec<_>>().join(" ")
}

/// Jarvis' own UI commands behind a garbled opening — the wake word misheard as "now this",
/// "now the scan you fit": "Now this opens your settings.", "Now the scan you fit, change the
/// interface color to red." Tries the clause after the first comma, then the words after one or
/// two leading noise words, each with a third-person verb made imperative. Only the HUD, board,
/// view and media tools count (`MISHEARD_SAFE_TOOLS`).
pub fn parse_misheard(text: &str) -> Option<Intent> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 3 || words.len() > 14 {
        return None;
    }
    let mut candidates: Vec<String> = Vec::new();
    if let Some((_, rest)) = text.split_once(',') {
        candidates.push(rest.trim().to_string());
    }
    for skip in 1..=2 {
        if words.len() > skip + 1 {
            candidates.push(words[skip..].join(" "));
        }
    }
    candidates.into_iter().map(|c| third_person_to_imperative(&c)).filter(|c| c.split_whitespace().count() >= 2).find_map(|c| match parse(&c) {
        Some(intent @ Intent::Tool { name, .. }) if MISHEARD_SAFE_TOOLS.contains(&name) => Some(intent),
        Some(intent @ Intent::Silent(_)) => Some(intent),
        _ => None,
    })
}

/// "What's on my screen right now?", "what do you see", "what's shown", "was ist auf meinem
/// Bildschirm": a look at the screen, never the calendar.
pub fn is_screen_question(text: &str) -> bool {
    let s = raw_words(text);
    let screen = ["screen", "monitor", "bildschirm"].iter().any(|w| s.split_whitespace().any(|x| x == *w)) || s.contains("my display") || s.contains("the display");
    let seeing = ["what do you see", "can you see", "what can you see", "looking at", "what s shown", "whats shown", "what is shown", "what is it showing", "whats it showing", "what s it showing", "what s showing", "what is showing", "was siehst du", "was wird angezeigt"]
        .iter()
        .any(|p| s.contains(p));
    let asks = ["what", "whats", "was", "describe", "tell", "explain", "read", "look", "see", "show", "schau", "beschreib", "lies", "check", "zeig"].iter().any(|w| s.split_whitespace().any(|x| x == *w));
    (screen && asks) || seeing
}

/// "Switch to hands-free mode", "Freisprechmodus aktivieren", "only listen when I say Jarvis":
/// `set_listening_mode` arguments.
pub fn listening_mode_command(text: &str) -> Option<Value> {
    let s = raw_words(text);
    let hands_free = ["hands free mode", "handsfree mode", "hands free", "handsfree", "freisprechmodus", "freisprech modus", "listen to everything", "always listen", "hör immer zu", "immer zuhören"];
    let wake = ["wake word mode", "wakeword mode", "wake word", "only listen when i say jarvis", "only when i say jarvis", "only respond when i say jarvis", "only listen to jarvis", "aktivierungswort", "nur wenn ich jarvis sage", "weckwort"];
    let switching = ["switch", "turn on", "activate", "enable", "go", "use", "set", "change", "aktivier", "aktiviere", "schalte", "schalt", "wechsel", "wechsle", "only", "nur", "mode", "modus", "listen", "zuhören", "hör"];
    if !switching.iter().any(|w| s.contains(w)) {
        return None;
    }
    if wake.iter().any(|p| s.contains(p)) {
        return Some(json!({ "mode": "wake_word" }));
    }
    if hands_free.iter().any(|p| s.contains(p)) {
        return Some(json!({ "mode": "hands_free" }));
    }
    None
}

/// "Switch to orb mode", "only show the orb", "go minimal orb", "make the orb 250 pixels",
/// "move the orb to the top right of monitor 2", "keep the orb in the foreground": arguments
/// for `control_orb_mode`.
pub fn orb_mode_command(text: &str) -> Option<Value> {
    let s = raw_words(text);
    if !s.contains("orb") && !s.contains("kugel") {
        return None;
    }
    let mut args = serde_json::Map::new();
    if ["orb mode", "orb modus", "only show the orb", "only the orb", "just the orb", "minimal orb", "pure orb", "orb only", "nur den orb", "nur noch den orb", "nur die kugel"]
        .iter()
        .any(|p| s.contains(p))
        && !["exit orb", "leave orb", "orb mode off", "turn off orb", "beende"].iter().any(|p| s.contains(p))
    {
        args.insert("enabled".into(), json!(true));
    }
    if ["exit orb", "leave orb", "orb mode off", "turn off orb mode", "disable orb mode", "no more orb"].iter().any(|p| s.contains(p)) {
        args.insert("enabled".into(), json!(false));
    }
    static PIXELS: LazyLock<Regex> = LazyLock::new(|| re(r"(\d{2,4})\s*(?:px|pixels?|pixel)"));
    if let Some(c) = PIXELS.captures(&s) {
        if let Ok(px) = c[1].parse::<f64>() {
            args.insert("orb_size_px".into(), json!(px));
        }
    }
    for (phrase, anchor) in [
        ("top right", "top-right"), ("top left", "top-left"), ("bottom right", "bottom-right"), ("bottom left", "bottom-left"),
        ("oben rechts", "top-right"), ("oben links", "top-left"), ("unten rechts", "bottom-right"), ("unten links", "bottom-left"),
        ("center", "center"), ("centre", "center"), ("middle", "center"), ("mitte", "center"),
    ] {
        if s.contains(phrase) {
            args.insert("screen_position".into(), json!(anchor));
            break;
        }
    }
    static MONITOR: LazyLock<Regex> = LazyLock::new(|| re(r"(?:monitor|screen|display|bildschirm)\s+(\d)|(first|second|third|left|right|primary|main)\s+(?:monitor|screen|display)"));
    if let Some(c) = MONITOR.captures(&s) {
        let monitor = c.get(1).map(|m| m.as_str().to_string()).or_else(|| c.get(2).map(|m| m.as_str().to_string()));
        if let Some(m) = monitor {
            args.insert("monitor".into(), json!(m));
        }
    }
    if ["in the foreground", "on top", "in front", "im vordergrund"].iter().any(|p| s.contains(p)) {
        args.insert("always_on_top".into(), json!(true));
    }
    if ["in the background", "not on top", "behind", "im hintergrund"].iter().any(|p| s.contains(p)) {
        args.insert("always_on_top".into(), json!(false));
    }
    static OPACITY: LazyLock<Regex> = LazyLock::new(|| re(r"(?:opacity|transparency|transparent|deckkraft)\s+(?:to\s+|auf\s+|at\s+)?(\d{1,3})\s*(?:%|percent|prozent)?"));
    if let Some(c) = OPACITY.captures(&s) {
        if let Ok(pct) = c[1].parse::<f64>() {
            let value = if s.contains("transparen") { 1.0 - pct / 100.0 } else { pct / 100.0 };
            args.insert("opacity".into(), json!(value.clamp(0.15, 1.0)));
        }
    } else if s.contains("more transparent") || s.contains("transparenter") {
        args.insert("opacity".into(), json!("-0.15"));
    } else if s.contains("less transparent") || s.contains("more opaque") {
        args.insert("opacity".into(), json!("+0.15"));
    }
    // Size words only with the orb in orb mode ("make the orb bigger" in the HUD is the layout's).
    if args.is_empty() {
        return None;
    }
    // A place alone moves the emblem on the board ("move the orb to the top left"); with a
    // monitor, a size or orb-mode words it is the floating orb.
    if args.len() == 1 && args.contains_key("screen_position") {
        return None;
    }
    // A place for the orb means the floating orb; a size alone resizes whichever orb is shown.
    if !args.contains_key("enabled") && (args.contains_key("screen_position") || args.contains_key("monitor")) {
        args.insert("enabled".into(), json!(true));
    }
    Some(Value::Object(args))
}

/// "Set Sonora to 30 % and Discord to 100 %", "mach Spotify auf 20 Prozent", "mute Chrome",
/// "lower the volume by 5 %", "set it back to 100 %": `control_audio_mixer` arguments (several
/// apps in one sentence become `apps`).
pub fn mixer_command(text: &str) -> Option<Value> {
    // Lower-case words with the per cent sign kept ("30%").
    let s = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '%' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // "Which apps are playing audio?", "show the mixer": read the real mixer (never guess).
    static LIST: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:jarvis )?(?:(?:which|what) (?:apps?|programs?|applications?) (?:are|is) (?:playing|making|using) (?:any )?(?:audio|sound|noise)(?: right now| now| at the moment)?|what s playing audio|whats playing audio|what is playing audio|(?:show|open|read|check)(?: me)? (?:the |my )?(?:volume |audio )?mixer|(?:the |my )?(?:volume |audio )mixer|(?:what are )?(?:the |my )?app volumes|welche apps (?:spielen|machen|nutzen) (?:gerade )?(?:audio|ton|sound)|zeig (?:mir )?den (?:lautstärke )?mixer)$")
    });
    if LIST.is_match(&s) {
        return Some(json!({ "action": "list" }));
    }
    static BY: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(lower|reduce|decrease|drop|raise|increase|turn up|turn down)\s+(?:the\s+)?(?:system\s+|master\s+)?(?:volume|sound|lautstärke)\s+by\s+(\d{1,3})(?:\s*(?:%|percent|prozent))?|(?:volume|sound|lautstärke)\s+(up|down)\s+by\s+(\d{1,3})(?:\s*(?:%|percent|prozent))?)$")
    });
    if let Some(c) = BY.captures(&s) {
        let n: i64 = c.get(2).or(c.get(4))?.as_str().parse().ok()?;
        let down = c.get(1).map(|m| matches!(m.as_str(), "lower" | "reduce" | "decrease" | "drop" | "turn down")).unwrap_or_else(|| c.get(3).is_some_and(|m| m.as_str() == "down"));
        return Some(json!({ "action": "change_master", "delta": if down { -n } else { n } }));
    }
    static BACK: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:set|put|turn)\s+(?:it|the volume|the sound|volume)\s+(?:back\s+)?(?:to|on)\s+(?:a\s+)?(\d{1,3})\s*(?:%|percent|prozent)?$"));
    if let Some(c) = BACK.captures(&s) {
        let n: u64 = c[1].parse().ok()?;
        return Some(json!({ "action": "set_master", "level": n.min(100) }));
    }
    // Per-application: "<app> to N %" pairs, or "mute / unmute <app>".
    static PAIR: LazyLock<Regex> = LazyLock::new(|| re(r"([a-z][a-z0-9 ]{1,24}?)\s+(?:to|auf|at)\s+(\d{1,3})\s*(?:%|percent|prozent)"));
    let apps: Vec<Value> = PAIR
        .captures_iter(&s)
        .filter_map(|c| {
            let raw = c[1].trim();
            // "Turn Sonora down to 20 %", "mach Discord leiser auf 30 Prozent": the direction is no name.
            let name = raw
                .rsplit(" and ")
                .next()
                .unwrap_or(raw)
                .split_whitespace()
                .filter(|w| {
                    !matches!(
                        *w,
                        "set" | "make" | "mach" | "mache" | "stell" | "stelle" | "put" | "turn" | "bring" | "the" | "and" | "und" | "volume" | "of" | "lautstärke" | "von"
                            | "down" | "up" | "runter" | "hoch" | "leiser" | "lauter" | "die" | "den" | "my" | "meine" | "mein"
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            let level: u64 = c[2].parse().ok()?;
            (!name.is_empty() && crate::audio_mixer::is_app_name(&name)).then(|| json!({ "app": name, "level": level.min(100) }))
        })
        .collect();
    if !apps.is_empty() {
        return Some(json!({ "action": "set_apps", "apps": apps }));
    }
    static MUTE_APP: LazyLock<Regex> = LazyLock::new(|| re(r"^(mute|unmute|stumm schalten|stummschalten)\s+([a-z][a-z0-9 ]{1,24})$|^([a-z][a-z0-9 ]{1,24})\s+(stumm|stummschalten|muten|unmuten)$"));
    if let Some(c) = MUTE_APP.captures(&s) {
        let verb = c.get(1).or(c.get(4))?.as_str();
        let app = c.get(2).or(c.get(3))?.as_str().trim();
        if crate::audio_mixer::is_app_name(app) {
            let mute = !verb.starts_with("unmute");
            return Some(json!({ "action": if mute { "mute_app" } else { "unmute_app" }, "app": app }));
        }
    }
    None
}

static BRIEFING: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:good morning|morning|guten morgen|moin|brief me|briefing|daily briefing|morning briefing|evening briefing|executive briefing|morning report|daily report|give me (?:a|my|the) (?:morning |evening |daily |executive )?(?:briefing|rundown|daily update|update)|update me on my day|brief me on my day|what(?:s| is) on (?:today|my agenda|the agenda|for today)|whats the plan for today|was steht heute an|tagesbriefing|gib mir (?:ein|mein) briefing|lagebericht)$")
});
static LIST_REMINDERS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:what are|list|show|read)(?: me)? (?:my|all|the|all my) reminders|(?:do i have )?any reminders(?: today)?|my reminders|reminders|welche erinnerungen habe ich|meine erinnerungen|zeig (?:mir )?meine erinnerungen|(?:what|which) reminders do i have(?: today| set| left| open)?|do i have (?:any )?reminders(?: today| set| left)?|(?:list|show)(?: me)? (?:my )?reminders|habe ich erinnerungen)$")
});
static CANCEL_REMINDERS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:cancel|delete|remove|clear)\s+(?:all\s+)?(?:of\s+)?(?:my\s+|the\s+)?reminders?(?:\s+(?:about|for|to)\s+(.+))?$|^(?:lösche|lösch|entferne)\s+(?:alle\s+)?(?:meine\s+)?erinnerung(?:en)?$")
});
static TIMER: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:set|start|create|make)\s+(?:a\s+|an\s+)?)?(?:timer|countdown)\s+(?:for\s+|of\s+)?(\d+|a|an|one|two|three|four|five|ten|fifteen|twenty|thirty|forty|fifty|sixty)\s+(seconds?|minutes?|mins?|hours?)$|^(?:(?:set|start)\s+(?:a\s+|an\s+)?)?(\d+)[\s-]*(second|minute|min|hour)s?\s+(?:timer|countdown)$|^(?:stell|stelle|starte|start)\s+(?:einen\s+|ein\s+)?(?:timer|countdown)\s+(?:auf|für|fuer|von)\s+(\d+|eine|einen|zwei|drei|fünf|zehn|fünfzehn|zwanzig|dreißig)\s+(sekunden?|minuten?|stunden?)$|^timer\s+(\d+)\s+(sekunden?|minuten?|stunden?|seconds?|minutes?)$")
});
static FOCUS_START: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:start|engage|enter|activate|begin|turn on|switch to|starte|aktiviere)\s+)?(?:a\s+|the\s+|den\s+)?(?:focus|deep work|fokus|konzentration)(?:\s*(?:mode|session|time|modus|zeit))?(?:\s+(?:for|für)\s+(\d+)\s*(?:minutes?|mins?|minuten?))?$|^(?:i need to|let me|ich muss mich)\s+(?:focus|konzentrieren)(?:\s+for\s+(\d+)\s*minutes)?$")
});
static FOCUS_STOP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:stop|end|exit|cancel|leave|finish|turn off|disengage)\s+(?:the\s+)?(?:focus|fokus)(?:\s*(?:mode|session|modus))?$|^(?:fokusmodus|fokus)\s+(?:beenden|aus|stoppen)$")
});
static COPY_THAT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:copy (?:that|this|it)(?: (?:to|into) (?:the |my )?clipboard)?|copy (?:your|the) (?:answer|reply|last answer)(?: (?:to|into) (?:the |my )?clipboard)?|put (?:that|it) (?:in|into|on) (?:the |my )?clipboard|kopier das|kopiere das|kopier es|in die zwischenablage)$")
});
static NETWORK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:am i online|are we online|are we connected|is (?:my |the )?internet (?:working|down|up|ok|okay)|check (?:my |the )?(?:internet|connection|network|wifi|wlan)(?: connection)?|hows? my (?:internet|connection|network|wifi)|how is my (?:internet|connection|network)|what(?:s| is) my (?:public )?ip(?: address)?|ping|network status|internet status|bin ich online|sind wir online|geht (?:mein |das )?internet|internet check|netzwerkstatus|wie ist meine verbindung|wie ist mein internet)$")
});
static PROCESSES: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:whats slowing (?:down )?my (?:pc|computer|system|machine|laptop)(?: down)?|what is slowing (?:down )?my (?:pc|computer|system|machine|laptop)(?: down)?|why is my (?:pc|computer|system|machine|laptop) (?:so )?(?:slow|laggy|sluggish)|which (?:app|program|process|application)s? (?:uses|use|is using|are using) (?:the most |so much )?(?:cpu|memory|ram|resources)(?: the most)?|what(?:s| is) using (?:my |all my |so much |the most )?(?:cpu|memory|ram|resources)|top processes|show (?:me )?the top processes|was (?:verbraucht|frisst|bremst) (?:so viel )?(?:ram|cpu|speicher|leistung|meinen pc)|warum ist mein (?:pc|rechner|computer|laptop) so langsam)$")
});
static VOICE_FASTER: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:speak|talk) (?:a (?:bit|little) |much )?(?:faster|quicker)$|^sprich (?:etwas |ein bisschen )?schneller$|^rede schneller$"));
static VOICE_SLOWER: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:speak|talk) (?:a (?:bit|little) |much )?(?:slower|more slowly)$|^sprich (?:etwas |ein bisschen )?langsamer$|^rede langsamer$"));
static FIND_FILE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:find|search for|look for|locate|where is|wheres|where are|wo ist|wo sind|finde|such|suche)\s+(?:my |the |meine |meinen |mein |die |das |den )?(.{2,60}?)$")
});
static OPEN_FILE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:open|öffne|oeffne|show me)\s+(?:my |the |meine |die |das )?(?:file|document|datei|dokument)\s+(?:called\s+|named\s+)?(.{2,60}?)$|^(?:open|öffne|oeffne)\s+(?:my |the |meine |die |das )?([\w äöüß\-]{1,50}\.(?:pdf|docx?|xlsx?|pptx?|txt|md|csv|png|jpe?g|mp4|mp3|zip|odt|ods))$")
});
const FILE_WORDS: &[&str] = &[
    "file", "files", "document", "documents", "datei", "dateien", "dokument", "pdf", "photo", "photos", "picture", "pictures", "spreadsheet",
    "presentation", "bild", "bilder", "foto", "fotos", "screenshot", "screenshots", "video", "videos", "song", "excel", "word", "powerpoint",
];
static READ_SCREEN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:what(?:s| is) on (?:my |the )?(?:screen|monitor|display)|read (?:my|the|this) (?:screen|window|page|monitor)|what am i looking at|look at (?:my |the )?screen|what does (?:this|the|my) (?:screen|window|page|error|message|dialog) say|explain (?:this|the) (?:error|message|dialog|screen|page|window)|summari[sz]e (?:this|the) (?:page|window|screen|article)|was steht (?:da|hier|auf (?:meinem|dem) bildschirm)|lies (?:meinen|den) bildschirm|was siehst du(?: auf meinem bildschirm)?|schau (?:dir )?(?:meinen )?bildschirm an)\b")
});
/// Sir's highlighted text.
static SELECTION: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:i (?:have |ve )?(?:selected|highlighted|marked)|(?:the |this |my )?(?:selected|highlighted|marked) (?:text|code|part|line|lines|paragraph|sentence|word|words|section|function|snippet)|my selection|the selection|this selection|(?:das |den |die )?(?:markierte|markierten|ausgewählte|ausgewählten)(?: text| code| teil)?|meine auswahl|was ich markiert habe)\b")
});
/// A specific monitor to look at.
static LOOK_MONITOR: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:look at|check|read|scan|what(?:s| is) on|whats happening on|summari[sz]e(?: what(?:s| is) on)?|describe|schau (?:dir )?|schau auf|was ist auf|was passiert auf|lies)\s+((?:the\s+|my\s+|den\s+|meinen\s+|dem\s+|meinem\s+)?(?:(?:first|second|third|other|left|right|primary|main|erste[nm]?|zweite[nm]?|dritte[nm]?|andere[nm]?|linke[nm]?|rechte[nm]?|haupt)\s*(?:monitor|screen|display|bildschirm)|(?:monitor|screen|display|bildschirm)\s*(?:\d|one|two|three|eins|zwei|drei)))\b")
});
/// "What does this error mean?", "what's that error?", "explain this code".
static SCREEN_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:what does (?:this|that|the) (?:error|message|warning|dialog|popup|exception|code) mean|what(?:s| is) (?:this|that) (?:error|message|warning|exception)|explain (?:this|that) (?:code|exception|warning|stack trace)|(?:fix|debug) (?:this|that) error|was bedeutet (?:dieser|der|diese|die) (?:fehler|meldung|warnung)|was ist das für ein fehler)\b")
});
static READ_CLIPBOARD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:what(?:s| is) (?:in|on) (?:my|the) clipboard|read (?:my|the) clipboard|(?:summari[sz]e|translate|explain|check|fix|proofread) (?:what i copied|my clipboard|the clipboard|the copied text|the text i copied)|was ist in (?:meiner|der) zwischenablage|lies (?:meine|die) zwischenablage|(?:fasse|übersetze|erkläre) (?:die zwischenablage|was ich kopiert habe))\b")
});
static RECALL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:what did (?:we|i) (?:talk|speak|chat) about|what were we (?:talking|speaking) about|what have we (?:talked|spoken) about|what did i (?:say|tell you|ask you|ask) (?:about|regarding)?|what did you (?:tell|say to) me (?:about)?|do you remember (?:what|when) (?:i|we)|worüber haben wir (?:gesprochen|geredet)|was haben wir (?:besprochen|beredet)|was habe ich (?:dir )?(?:gesagt|erzählt))\b")
});

/// Small numbers spoken as words, for timers.
fn small_number(word: &str) -> Option<f64> {
    Some(match word {
        "a" | "an" | "one" | "eine" | "einen" => 1.0,
        "two" | "zwei" => 2.0,
        "three" | "drei" => 3.0,
        "four" => 4.0,
        "five" | "fünf" => 5.0,
        "ten" | "zehn" => 10.0,
        "fifteen" | "fünfzehn" => 15.0,
        "twenty" | "zwanzig" => 20.0,
        "thirty" | "dreißig" => 30.0,
        "forty" => 40.0,
        "fifty" => 50.0,
        "sixty" => 60.0,
        n => return n.parse().ok(),
    })
}

/// "Set a timer for 5 minutes" → a countdown widget.
fn timer_command(s: &str) -> Option<Value> {
    let c = TIMER.captures(s)?;
    let groups: Vec<&str> = c.iter().skip(1).flatten().map(|m| m.as_str()).collect();
    let (n, unit) = (small_number(groups.first()?)?, *groups.get(1)?);
    let secs = n * if unit.starts_with("sec") || unit.starts_with("sek") {
        1.0
    } else if unit.starts_with('h') || unit.starts_with("stunde") {
        3600.0
    } else {
        60.0
    };
    let label = match secs as u64 {
        s if s < 60 => format!("{s} second timer"),
        s if s % 3600 == 0 => format!("{} hour timer", s / 3600),
        s if s % 60 == 0 => format!("{} minute timer", s / 60),
        s => format!("{s} second timer"),
    };
    Some(json!({
        "id": format!("timer_{}", secs as u64),
        "type": "CountdownOrStopwatch",
        "title": label,
        "mode": "countdown",
        "duration_secs": secs,
        "running": true,
        "elapsed_ms": 0,
        "alerted": false,
        "started_at_ms": crate::widgets::now_ms(),
    }))
}

/// Requests whose tool runs first, then the language model explains the result (screen text,
/// clipboard contents, earlier conversation).
pub fn upfront_tool(text: &str) -> Option<(&'static str, Value)> {
    let s = normalize(text);
    let inspect = |args: Value| Some(("inspect_screen_or_selection", args));
    // "Refactor the code I selected", "translate the highlighted text", "was bedeutet das markierte?"
    if SELECTION.is_match(&s) {
        return inspect(json!({ "mode": "selection" }));
    }
    // "Look at my second monitor", "what's on monitor 2", "summarise what's on my left screen".
    if let Some(c) = LOOK_MONITOR.captures(&s) {
        let phrase = c.get(1).map(|m| m.as_str()).unwrap_or("");
        if let Some(m) = monitor_value(phrase) {
            return inspect(json!({ "mode": "monitor", "target_monitor": m }));
        }
    }
    if READ_SCREEN.is_match(&s) || SCREEN_ERROR.is_match(&s) {
        let whole = s.contains("screen") || s.contains("bildschirm") || s.contains("monitor") || s.contains("display");
        return inspect(if whole { json!({ "mode": "monitor", "target_monitor": "primary" }) } else { json!({ "mode": "active_window" }) });
    }
    // "What do you see?", "was ist auf meinem Bildschirm": the whole primary monitor.
    if is_screen_question(text) && !s.contains("simulation") && !s.contains("calendar") && !s.contains("kalender") {
        return inspect(json!({ "mode": "monitor", "target_monitor": "primary" }));
    }
    if READ_CLIPBOARD.is_match(&s) {
        return Some(("clipboard", json!({ "action": "read" })));
    }
    if RECALL.is_match(&s) {
        let days = if s.contains("yesterday") || s.contains("gestern") {
            2
        } else if s.contains("today") || s.contains("heute") {
            1
        } else if s.contains("week") || s.contains("woche") {
            7
        } else {
            30
        };
        // Keywords after "about"/"über"; none = the latest lines.
        let query = ["about ", "regarding ", "über ", "zu "].iter().find_map(|k| s.split_once(k).map(|(_, rest)| rest.trim().to_string()));
        let query = query.filter(|q| !q.is_empty() && !["yesterday", "today", "earlier", "gestern", "heute", "last week"].contains(&q.as_str()));
        return Some(("search_conversation", json!({ "query": query, "days_back": days })));
    }
    None
}

/// The new assistant abilities as instant commands (part of `parse`).
fn assistant_command(s: &str, original: &str) -> Option<Intent> {
    let tool = |name: &'static str, args: Value| Some(Intent::Tool { name, args });
    if BRIEFING.is_match(s) {
        let time_of_day = if s.contains("evening") { "evening" } else if s.contains("morning") || s.contains("morgen") { "morning" } else { "" };
        return tool("run_executive_briefing", if time_of_day.is_empty() { json!({}) } else { json!({ "time_of_day": time_of_day }) });
    }
    if let Some((due, what)) = crate::reminders::parse_request(original, chrono::Local::now()) {
        return tool("reminders", json!({ "action": "set", "text": what, "due_ms": due.timestamp_millis() }));
    }
    if LIST_REMINDERS.is_match(s) {
        return tool("reminders", json!({ "action": "list" }));
    }
    if let Some(c) = CANCEL_REMINDERS.captures(s) {
        let target = c.get(1).map(|m| m.as_str().to_string()).unwrap_or_else(|| if s.contains("all") || s.contains("alle") || s.ends_with('s') || s.ends_with("en") { "all".into() } else { "next".into() });
        return tool("reminders", json!({ "action": "cancel", "id": target }));
    }
    if let Some(args) = timer_command(s) {
        return tool("create_or_update_widget", args);
    }
    if FOCUS_STOP.is_match(s) {
        return tool("focus_mode", json!({ "action": "stop" }));
    }
    if let Some(c) = FOCUS_START.captures(s) {
        let minutes = c.iter().skip(1).flatten().find_map(|m| m.as_str().parse::<f64>().ok()).unwrap_or(25.0);
        return tool("focus_mode", json!({ "action": "start", "minutes": minutes }));
    }
    if let Some(name) = crate::protocols::match_utterance(s) {
        return tool("manage_stark_protocol", json!({ "action": "execute_protocol", "protocol_name": name }));
    }
    if COPY_THAT.is_match(s) {
        return tool("clipboard", json!({ "action": "write", "from_last_reply": true }));
    }
    if NETWORK.is_match(s) {
        return tool("network_status", json!({}));
    }
    if PROCESSES.is_match(s) {
        return tool("top_processes", json!({}));
    }
    if VOICE_FASTER.is_match(s) {
        return tool("media_and_volume_control", json!({ "action": "jarvis_faster" }));
    }
    if VOICE_SLOWER.is_match(s) {
        return tool("media_and_volume_control", json!({ "action": "jarvis_slower" }));
    }
    if let Some(c) = OPEN_FILE.captures(s) {
        let name = c.iter().skip(1).flatten().map(|m| m.as_str().trim()).find(|n| !n.is_empty())?;
        if !name.contains(" and ") && !name.contains(" und ") {
            return tool("find_files", json!({ "query": name, "open": true }));
        }
    }
    if let Some(c) = FIND_FILE.captures(s) {
        let what = c.get(1)?.as_str().trim();
        let is_file = what.split_whitespace().any(|w| FILE_WORDS.contains(&w))
            || what.rsplit_once('.').is_some_and(|(_, ext)| (2..=4).contains(&ext.len()) && ext.chars().all(char::is_alphanumeric));
        if is_file {
            // "my jarvis config file" searches for "jarvis config" (the generic word is no name).
            let words: Vec<&str> = what.split_whitespace().collect();
            let query = match words.split_last() {
                Some((last, rest)) if !rest.is_empty() && matches!(*last, "file" | "files" | "document" | "documents" | "datei" | "dateien" | "dokument") => rest.join(" "),
                _ => what.to_string(),
            };
            return tool("find_files", json!({ "query": query }));
        }
    }
    if let Some(answer) = crate::calc::try_answer(original) {
        return Some(Intent::Answer(answer));
    }
    None
}

fn normalize(text: &str) -> String {
    // Sentence-final punctuation first, so a last filler word ("Yes, please.") is still seen.
    let lower = text.trim().to_lowercase();
    let lower = lower.trim_end_matches(['.', '!', '?', ' ']);
    let cleaned: String = lower
        .chars()
        .filter(|c| !matches!(c, '\'' | '’' | '`'))
        .map(|c| if c.is_alphanumeric() || c == '%' || c == ' ' || c == '-' || c == '.' { c } else { ' ' })
        .collect();
    // German/English fillers anywhere in the sentence ("öffne bitte Discord", "mach mal auf").
    const FILLERS: &[&str] = &["bitte", "please", "mal", "doch", "eben", "kurz", "schnell", "gerade", "einfach"];
    let mut s = cleaned
        .split_whitespace()
        .filter(|w| !FILLERS.contains(w))
        .collect::<Vec<_>>()
        .join(" ");
    s = s.trim_end_matches('.').trim().to_string();
    // Polite wrappers and spoken interjections ("Nana, open my browser") that don't change the
    // command.
    for _ in 0..4 {
        let before = s.clone();
        for prefix in [
            "hey jarvis ", "jarvis ", "please ", "bitte ", "can you ", "could you ", "would you ", "kannst du ",
            "könntest du ", "würdest du ", "mal ", "doch mal ", "ok ", "okay ", "so ", "nana ", "na ", "nah ", "no ", "yeah ",
            "yes ", "yep ", "um ", "umm ", "uh ", "uhm ", "äh ", "ähm ", "öhm ", "hmm ", "also ", "well ", "alright ", "right ",
            "and ", "und ", "now ", "then ", "dann ", "ja ", "nein ", "oh ", "ah ", "actually ", "eigentlich ", "hey ",
        ] {
            if let Some(rest) = s.strip_prefix(prefix) {
                s = rest.to_string();
            }
        }
        for suffix in [" please", " bitte", " jarvis", " for me", " für mich", " now", " jetzt", " mal", " sofort", " right now"] {
            if let Some(rest) = s.strip_suffix(suffix) {
                s = rest.to_string();
            }
        }
        if s == before {
            break;
        }
    }
    s.trim().to_string()
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid intent regex")
}

/// The fast path's normalisation (fillers, wake word, punctuation), for phrase tables elsewhere.
pub fn normalize_phrase(text: &str) -> String {
    normalize(text)
}

/// Spoken layout commands for the HUD (`control_jarvis_interface` arguments), classified on Sir's
/// own words: "make the interface less full", "remove the audio devices tab and the skills
/// tab", "bring back the audio settings", "make the orb bigger", "zen mode".
pub fn ui_layout_command(text: &str) -> Option<Value> {
    let s = normalize(text);
    if s.is_empty() || s.split_whitespace().count() > 32 {
        return None;
    }
    interface::layout_command(&s)
}

/// "Open my system stats (in a new window)", "show my telemetry on monitor 2", "open my calendar
/// in a new window", "open the simulation lab", "show the QR code", "open my gym logs", Whisper's
/// "open message from stats": `open_jarvis_view` arguments. Jarvis' own views are never programs
/// for the app launcher. Panel words ("hide the system panel", "show the telemetry tab") stay HUD
/// layout commands.
pub fn internal_view_command(text: &str) -> Option<Value> {
    let s = normalize(text);
    if s.is_empty() || s.split_whitespace().count() > 16 {
        return None;
    }
    static VERB: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(open up|open|show me|show|pull up|bring up|display|launch|start|give me|get me|put up|pop up|zeig mir|zeige mir|zeig|zeige|öffne mir|öffne|oeffne|mach|mache)\s+(.+)$")
    });
    static PANEL_WORDS: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:panel|panels|tab|tabs|sidebar|sidebars|section|leiste|bereich|reiter)\b"));
    static WINDOW_TAIL: LazyLock<Regex> = LazyLock::new(|| {
        re(r"\s+(?:in|into|as|on|im|in einem|in einen|als)\s+(?:a|an|its own|their own|one|the|einem|einen|eigenen|ein)?\s*(?:new|separate|second|own|extra|another|real|neuen|neues|eigenen|separaten|zweiten)?\s*(?:native\s+|os\s+)?(?:window|fenster)$|\s+(?:auf|an)$")
    });
    if PANEL_WORDS.is_match(&s) {
        return None;
    }
    let c = VERB.captures(&s)?;
    let verb = c.get(1)?.as_str();
    let mut object = c.get(2)?.as_str().to_string();
    let monitor = MONITOR_ANYWHERE.captures(&s).and_then(|m| monitor_value(m.get(1)?.as_str()));
    if let Some(m) = MONITOR_ANYWHERE.find(&object) {
        object.truncate(m.start());
    }
    let in_window = WINDOW_TAIL.is_match(object.trim()) || WINDOW_WORD.is_match(&object);
    let object = strip_article(&WINDOW_TAIL.replace(object.trim(), ""));
    // "Show my calendar" reads it out in the chat; "open" (or "in a window") gives it a window.
    let opening = in_window || matches!(verb, "open up" | "open" | "pull up" | "bring up" | "launch" | "öffne mir" | "öffne" | "oeffne" | "mach" | "mache");
    let view = view_named(&object, opening)?;
    let mut args = json!({ "view": view });
    if SEPARATE_WINDOW.is_match(&s) {
        args["separate_window"] = json!(true);
    }
    if let Some(m) = monitor {
        args["monitor"] = m;
    }
    Some(args)
}

/// The HUD's own views, before the layout classifier or the model can guess at them: the core
/// sphere (also as Whisper mishears it, see `typos::core_homophones`), the board restore ("bring
/// back the screen"), docking and detaching windows, and the navigation dock's tabs.
pub fn hud_view_command(text: &str) -> Option<Intent> {
    // "No. Close your core.": the command may be the last of a few short sentences.
    // Sentence ends only (". " or the end), never the dots inside "play.example.net".
    let mut pieces: Vec<String> = Vec::new();
    for part in text.split_inclusive(['.', '!', '?']) {
        match pieces.last_mut() {
            // "play." + "example." + "net": no space after the dot, so still one sentence.
            Some(last) if !part.starts_with(char::is_whitespace) && !last.is_empty() => last.push_str(part),
            _ => pieces.push(part.to_string()),
        }
    }
    let pieces: Vec<String> = pieces.into_iter().map(|p| p.trim().trim_end_matches(['.', '!', '?']).to_string()).filter(|p| !p.is_empty()).collect();
    if pieces.len() > 1 && pieces.len() <= 4 {
        if let Some(intent) = pieces.iter().rev().find_map(|p| hud_view_sentence(p)) {
            return Some(intent);
        }
    }
    hud_view_sentence(text)
}

fn hud_view_sentence(text: &str) -> Option<Intent> {
    let s = normalize(text);
    if s.is_empty() || s.split_whitespace().count() > 12 {
        return None;
    }
    let interface = |args: Value| Some(Intent::Tool { name: "configure_interface", args });
    if let Some(c) = HUD_CORE.captures(&s) {
        let verb = capture(&c)?;
        let close = ["close", "hide", "exit", "leave", "schließ", "schliess", "versteck", "collapse", "minimi"].iter().any(|v| verb.starts_with(v));
        return interface(json!({ "core_view": !close }));
    }
    if HUD_CORE_SCALE.is_match(&s) || HUD_CORE_SPIN.is_match(&s) {
        return interface_command(&s).and_then(interface);
    }
    // With a block maximised, "back to the board" minimises it (the board's own rule, later).
    if BOARD_RESTORE.is_match(&s) && !crate::hud_board::spotlight_active() {
        return Some(Intent::Tool { name: "hud_board", args: json!({ "action": "restore" }) });
    }
    if DOCK_ALL.is_match(&s) {
        return interface(json!({ "dock_windows": true }));
    }
    if let Some(c) = DETACH.captures(&s) {
        let name = capture(&c)?;
        return interface(json!({ "detach_window": name }));
    }
    if let Some(intent) = shadow_or_server_command(&s) {
        return Some(intent);
    }
    if let Some(action) = discord_call_command(&s) {
        return Some(Intent::Tool { name: "discord_call", args: json!({ "action": action }) });
    }
    if let Some(name) = theme_preset_request(&s) {
        return interface(json!({ "hud_palette": name }));
    }
    if let Some(c) = NAV_VIEW.captures(&s) {
        let mode = crate::hud_dock::HudViewMode::parse(capture(&c)?)?;
        return interface(json!({ "hud_view": mode.id() }));
    }
    None
}

/// Going out: "where can I take my fiancée for a Halloween night?" (Jarvis picks the venue and
/// puts it on the board) and "let's go here" (the route to it).
pub fn outing_command(text: &str) -> Option<Intent> {
    if let Some(request) = crate::venue_guide::venue_request(text) {
        return Some(Intent::Tool { name: crate::venue_guide::TOOL, args: json!({ "action": "recommend", "request": request }) });
    }
    if crate::venue_guide::go_request(text) {
        return Some(Intent::Tool { name: crate::venue_guide::TOOL, args: json!({ "action": "go_here" }) });
    }
    None
}

/// A spoken number of minutes / items ("ten", "zehn", "15").
fn count_word(word: &str) -> Option<u32> {
    word.parse().ok().or(match word {
        "one" | "a" | "eins" | "einer" => Some(1),
        "two" | "zwei" => Some(2),
        "three" | "drei" => Some(3),
        "four" | "vier" => Some(4),
        "five" | "fünf" | "fuenf" => Some(5),
        "six" | "sechs" => Some(6),
        "seven" | "sieben" => Some(7),
        "eight" | "acht" => Some(8),
        "ten" | "zehn" => Some(10),
        "fifteen" | "fünfzehn" => Some(15),
        "twenty" | "zwanzig" => Some(20),
        "thirty" | "dreißig" | "dreissig" => Some(30),
        _ => None,
    })
}

/// SHADOW memory ("what did I say ten minutes ago", "turn my last five thoughts into a note",
/// "search my last half hour for Kaufland", "was habe ich vor zehn Minuten gesagt") and the AEGIS
/// server deck ("check server play.example.net", "is my server online").
fn shadow_or_server_command(s: &str) -> Option<Intent> {
    static SAID_AGO: LazyLock<Regex> = LazyLock::new(|| re(r"^what did i (?:just )?say (?:about )?(\S+) minutes? ago$|^was hab(?:e)? ich (?:vor )?(\S+) minuten gesagt$|^was hab(?:e)? ich vor (\S+) minuten gesagt$"));
    static TO_NOTE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:turn|make|put) my last (\S+) (?:thoughts|things i said|lines|ideas) (?:into|in|as|on) (?:a )?note(?: block)?$|^(?:mach|mache) aus meinen letzten (\S+) gedanken (?:eine )?notiz$"));
    static SEARCH: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:search|find|check) (?:my|the) last (?:half hour|30 minutes|thirty minutes) for (.+)$|^(?:durchsuche|such) (?:die )?letzte halbe stunde nach (.+)$"));
    static SERVER: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:check|ping) (?:the |my )?(?:minecraft |mc )?(vps |server )(\S+)$|^is (?:the |my )?(?:minecraft |mc )?(vps |server )(\S+) (?:online|up|running)$|^(?:status of|server status) (?:the )?(\S+)$"));
    static MY_SERVER: LazyLock<Regex> = LazyLock::new(|| re(r"^is my (?:minecraft |mc )?server (?:online|up|running)$|^ist mein (?:minecraft )?server online$"));
    let shadow = |args: Value| Some(Intent::Tool { name: "shadow_memory", args });
    if let Some(c) = SAID_AGO.captures(s) {
        let minutes = count_word(capture(&c)?)?;
        return shadow(json!({ "action": "said_ago", "minutes": minutes }));
    }
    if let Some(c) = TO_NOTE.captures(s) {
        let count = count_word(capture(&c)?)?;
        return shadow(json!({ "action": "to_note", "count": count }));
    }
    if let Some(c) = SEARCH.captures(s) {
        return shadow(json!({ "action": "search", "query": capture(&c)? }));
    }
    if MY_SERVER.is_match(s) {
        return Some(Intent::Tool { name: "server_deck", args: json!({}) });
    }
    if let Some(c) = SERVER.captures(s) {
        let groups: Vec<&str> = c.iter().skip(1).flatten().map(|m| m.as_str().trim()).collect();
        let (kind, host) = match groups.as_slice() {
            [kind, host] => (*kind, *host),
            [host] => ("server", *host),
            _ => return None,
        };
        let target = if kind == "vps" { format!("vps:{host}") } else { host.to_string() };
        return Some(Intent::Tool { name: "server_deck", args: json!({ "hosts": [target] }) });
    }
    None
}

/// Sir's Discord call: "listen to my Discord call" / "hör im Discord mit" (listen), "you can talk
/// to them" / "du darfst mit ihnen reden" (converse), "don't talk in the call" (quiet), "stop
/// listening to the call" / "hör auf mitzuhören" (off), "what did they just say" (recap).
fn discord_call_command(s: &str) -> Option<&'static str> {
    let call = ["discord", "the call", "my call", "im call", "den call", "dem call", "in the call", "the others", "my friends", "meinen freunden", "die anderen"].iter().any(|w| s.contains(w));
    let stop = ["stop listening", "stop hearing", "hör auf", "hoer auf", "leave the call", "verlass den call", "nicht mehr zuhören", "nicht mehr mithören"].iter().any(|w| s.contains(w));
    if stop && call {
        return Some("off");
    }
    // "Turn the Discord call on", "Discord call off": the bridge by name. On means listening;
    // "you can talk to them" lets him speak.
    if s.contains("discord call") || s.contains("discord bridge") || s.contains("call bridge") {
        let padded = format!(" {s} ");
        let says = |words: &[&str]| words.iter().any(|w| padded.contains(&format!(" {w} ")));
        if says(&["turn off", "switch off", "disable", "deactivate", "call off", "bridge off", "ausschalten", "call aus"]) {
            return Some("off");
        }
        if says(&["turn on", "switch on", "enable", "activate", "call on", "bridge on", "einschalten", "call an"]) {
            return Some("listen");
        }
    }
    // A question about the bridge, not an order: "are you listening to the call?" must not
    // switch it on.
    if (call || s.contains("call")) && ["call status", "status of the call", "are you listening", "are you in the call", "are you in my call", "are you talking", "can they hear you", "bist du im call", "hörst du mit", "hoerst du mit", "hörst du zu", "callstatus", "call-status"].iter().any(|w| s.contains(w)) {
        return Some("status");
    }
    if call && ["don't talk", "dont talk", "do not talk", "stop talking", "be quiet", "keep quiet", "nur zuhören", "nicht mitreden", "red nicht", "rede nicht"].iter().any(|w| s.contains(w)) {
        return Some("quiet");
    }
    if ["you can talk to them", "talk to them", "talk in the call", "join the conversation", "speak in the call", "you may talk to them", "du darfst mit ihnen reden", "rede mit ihnen", "rede im call", "sprich mit ihnen", "red mit ihnen"].iter().any(|w| s.contains(w)) {
        return Some("converse");
    }
    if call && ["listen", "hear", "hör", "hoer", "mithören", "mithoeren", "zuhören", "zuhoeren"].iter().any(|w| s.contains(w)) && !s.contains("music") && !s.contains("musik") {
        return Some("listen");
    }
    if crate::discord_bridge::mode() != crate::discord_bridge::CallMode::Off
        && ["what did they say", "what did they just say", "what are they talking about", "what did he say", "what did she say", "was haben sie gesagt", "was hat er gesagt", "was hat sie gesagt", "worüber reden sie"].iter().any(|w| s.contains(w))
    {
        return Some("recap");
    }
    None
}

/// "Switch to the arc cyan theme", "stark gold theme", "Theme Ultraviolet", "nimm das Emerald
/// Matrix Theme": one of the eight HUD presets by name.
fn theme_preset_request(s: &str) -> Option<&'static str> {
    if !(s.contains("theme") || s.contains("palette") || s.contains("thema") || s.contains("farbschema")) {
        return None;
    }
    crate::theme_engine::PRESETS.iter().map(|(n, _)| *n).find(|n| s.contains(&n.replace('_', " ")))
}

/// "In a new / separate / its own window": Sir explicitly wants an OS window, not a HUD block.
static SEPARATE_WINDOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:new|separate|own|extra|another|real|native|second|neuen|neues|eigenen|separaten|zweiten)\s+(?:native\s+|os\s+)?(?:window|fenster)\b")
});

/// The Jarvis view a phrase names (`opening`: Sir wants a window, not a reading in the chat).
fn view_named(object: &str, opening: bool) -> Option<&'static str> {
    let o = object.trim();
    static STATS: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:system|systems|pc|computer|hardware|rechner|performance|machine)\s*(?:stats|statistics|status|monitor|telemetry|info|information|werte|statistik|usage|auslastung)|stats|statistics|telemetry|telemetrie|systemmonitor|systemstatus|systemstats|pc werte|message from stats|my sister stats|system s stats|systems stats|cpu and gpu usage|cpu and gpu stats|hardware monitor)$")
    });
    if STATS.is_match(o) {
        return Some("system_stats");
    }
    // Whisper's other versions of "my system stats" ("message stats", "misses stats"): something
    // short ending in "stats" that is no installed program or game.
    if let Some(head) = o.strip_suffix(" stats").or_else(|| o.strip_suffix(" statistics")) {
        let gamey = ["game", "player", "match", "kill", "steam", "valorant", "minecraft", "fortnite", "league", "rocket", "discord", "spotify", "youtube", "channel"];
        if head.split_whitespace().count() <= 3 && !known_app(head) && !gamey.iter().any(|g| head.contains(g)) {
            return Some("system_stats");
        }
    }
    match o {
        "simulation lab" | "sim lab" | "simlab" | "simulationlab" | "simulation labor" | "physics lab" | "stark simulation lab" | "lab" => Some("simulation_lab"),
        "qr code" | "qr" | "qr code for my phone" | "pairing qr" | "pairing code" | "mobile link" | "mobile satellite" | "satellite" | "phone link"
        | "qr code für mein handy" | "qr-code" => Some("mobile_qr"),
        "gym logs" | "gym log" | "gym" | "gym coach" | "workout logs" | "workout log" | "workout coach" | "gym tracker" | "finance tracker" | "finances"
        | "finance" | "finanzen" | "gym and finances" | "workout and finance" | "workouts" => Some("workout_finance"),
        "contacts" | "contact list" | "kontakte" | "contact book" => Some("contacts"),
        "schedule" | "calendar" | "kalender" | "webuntis" | "timetable" | "stundenplan" | "agenda" | "school schedule" | "schedule and calendar"
        | "calendar and schedule" | "termine" | "weekly schedule" | "week" if opening => Some("schedule"),
        _ => None,
    }
}

static STOP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(stop|stopp|halt|enough|cancel|abort|be quiet|quiet|silence|shut up|stop talking|never mind|nevermind|forget it|sei still|sei ruhig|ruhe|still|hör auf|hoer auf|das reicht|abbrechen|schluss|genug|vergiss es|egal|schon gut)$")
});
/// Quit Jarvis himself. "Close Jarvis" arrives as "close" (the trailing name is stripped);
/// "shut down the computer" deliberately does not match.
static SHUTDOWN: LazyLock<Regex> = LazyLock::new(|| {
    let forms = [
        // "Turn down" stays a volume request.
        r"(?:(?:shut|power)(?:\s+(?:yourself|jarvis))?\s+(?:down|off)|(?:turn|switch)(?:\s+(?:yourself|jarvis))?\s+off)(?:\s+(?:yourself|jarvis|completely|for (?:today|now|the night)))?",
        r"shutdown(?:\s+(?:yourself|jarvis))?",
        r"(?:close|quit|exit|terminate|end|kill|stop)\s+(?:yourself|jarvis|your\s+(?:app|application|program|process|self)|the\s+(?:assistant|jarvis app))",
        r"close|quit|exit|terminate|log off|sign off|go offline|self[- ]destruct",
        r"(?:beende|beend)(?:\s+(?:dich(?:\s+selbst)?|jarvis|dein\s+programm|deine\s+app))?",
        r"(?:dich|jarvis)?\s*(?:beenden|ausschalten|abschalten|herunterfahren|runterfahren|schließen|schliessen)",
        r"(?:schalt|schalte|mach|mache)\s+(?:dich|jarvis)\s+(?:aus|ab)",
        r"(?:fahr|fahre)\s+(?:dich|jarvis)?\s*(?:herunter|runter)",
        r"(?:schließ|schließe|schliess|schliesse)(?:\s+(?:dich|jarvis))?",
    ];
    re(&format!("^(?:{})$", forms.join("|")))
});
/// "Close" on its own (Jarvis not named): it means the window, not quitting.
static BARE_QUIT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:close|schließ|schließe|schliess|schliesse|schließen|schliessen)$")
});

/// The raw sentence names Jarvis himself ("close Jarvis", "quit yourself", "beende dich").
fn names_jarvis(text: &str) -> bool {
    raw_words(text).split_whitespace().any(|w| matches!(w, "jarvis" | "yourself" | "urself" | "dich" | "jarvi"))
}
static MEMORY_COMMAND: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(merk dir|merke dir|merk|notier dir|notiere dir|notiere|notier|speicher|speichere|vergiss nicht|vergiss|lösch die erinnerung|lösche die erinnerung|ich heiße|ich heisse|mein name ist|remember|note that|make a note|keep in mind|don't forget|dont forget|do not forget|forget that|forget about|forget my|my name is|call me)\b")
});

/// Explicit long-term-memory requests ("Merk dir …", "Remember that …", "Vergiss, dass …",
/// "My name is …"). Small LLMs often *claim* to have stored these without calling the tool,
/// so they are handled deterministically.
pub fn is_memory_command(text: &str) -> bool {
    let s = normalize(text);
    // "Erinnere mich …" / "remind me …" are reminders, not facts.
    !s.starts_with("erinnere mich") && !s.starts_with("remind me") && MEMORY_COMMAND.is_match(&s)
}
static TIME: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(what time is it|what's the time|whats the time|what is the time|tell me the time|current time|the time|time|wie spät ist es|wie spaet ist es|wie viel uhr ist es|wieviel uhr ist es|wie viel uhr|uhrzeit|was ist die uhrzeit)$")
});
static DATE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(what's the date|whats the date|what is the date|what date is it|what day is it|what day is it today|today's date|todays date|welcher tag ist heute|welches datum haben wir|was ist heute für ein tag|der wievielte ist heute|datum|(?:whats|what is) (?:the date|todays date|today s date)(?: today)?|what day is today|whats today|what is today|(?:the )?date today|welches datum ist heute|was ist heute für ein datum|welcher tag ist es)$")
});
static STATUS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(system status|status report|systemstatus|system report|hardware status|run diagnostics|diagnostics|wie geht es dem system|wie geht's dem system|systembericht|cpu usage|gpu temperature|gpu temp|wie warm ist die gpu|auslastung)$")
});
static VOLUME_SET: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:set|turn|put|stell|stelle|setz|setze|mach|mache|make|change|bring|adjust|crank|pump)\s+)?(?:the\s+|die\s+|my\s+|meine\s+|mein\s+|den\s+)?(?:system\s*|master\s+|pc\s+|computer\s+|overall\s+|gesamt\s*|windows\s+)?(?:volume|lautstärke|lautstaerke|sound|ton|audio|loudness)(?:\s+level)?\s+(?:to\s+|auf\s+|at\s+|on\s+)?(\d{1,3})\s*(?:%|percent|prozent)?$")
});
static VOLUME_SET_REV: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(\d{1,3})\s*(?:%|percent|prozent)\s*(?:volume|lautstärke|lautstaerke)?$"));
/// "Make my system 100 % loudness", "set the PC to 40 %", "mach den Rechner auf 30 Prozent".
static VOLUME_SET_PC: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make|set|turn|put|bring|mach|mache|stell|stelle|setz|setze)\s+(?:the\s+|my\s+|die\s+|meine\s+|mein\s+|den\s+|meinen\s+)?(?:system|pc|computer|master|rechner|audio)\s+(?:to\s+|auf\s+|at\s+|on\s+)?(\d{1,3})\s*(?:%|percent|prozent)\s*(?:loudness|loud|volume|lautstärke|laut)?$")
});
static VOLUME_UP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(volume up|louder|turn it up|turn up the volume|turn the volume up|increase the volume|lauter|mach lauter|mach es lauter|lautstärke hoch|lautstärke erhöhen|ton lauter)$")
});
static VOLUME_DOWN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(volume down|quieter|turn it down|turn down the volume|turn the volume down|decrease the volume|leiser|mach leiser|mach es leiser|lautstärke runter|lautstärke verringern|ton leiser)$")
});
static MUTE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(mute|mute audio|mute the sound|mute sound|ton aus|stumm|stummschalten|stumm schalten|lautlos)$"));
static UNMUTE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(unmute|unmute audio|sound on|ton an|ton wieder an|stummschaltung aufheben|laut schalten)$"));
static PLAY_PAUSE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(play|pause|resume|play music|pause music|pause the music|resume music|play pause|musik pausieren|musik pause|pausieren|pausiere|musik weiter|musik fortsetzen|fortsetzen|abspielen|spiel weiter)$")
});
static NEXT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(next|next track|next song|skip|skip song|skip this song|skip track|nächster titel|nächstes lied|nächster song|nächstes|überspringen|lied überspringen)$")
});
static PREVIOUS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(previous|previous track|previous song|last song|go back a song|letzter titel|vorheriger titel|vorheriges lied|voriges lied|zurück)$")
});
static VOICE_LOUDER: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(speak louder|talk louder|speak up|sprich lauter|rede lauter|sei lauter)$"));
static VOICE_QUIETER: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(speak quieter|speak softer|talk quieter|talk softer|sprich leiser|rede leiser|sei leiser)$"));
static OPEN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:open up|open|launch|start up|start|run|fire up|öffne|oeffne|öffne mir|starte|start mal|lade)\s+(.+?)$")
});
static OPEN_DE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:mach|mache)\s+(.+?)\s+(?:auf|an)$"));
static OPEN_DE_SUFFIX: LazyLock<Regex> = LazyLock::new(|| re(r"^(.+?)\s+(?:öffnen|starten|aufmachen)$"));
static CLOSE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:close|quit|exit|kill|terminate|shut down|schließe|schliesse|schließ|schliess|beende|beend)\s+(.+?)$")
});
static CLOSE_DE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:mach|mache)\s+(.+?)\s+(?:zu|aus)$"));
static CLOSE_DE_SUFFIX: LazyLock<Regex> = LazyLock::new(|| re(r"^(.+?)\s+(?:schließen|schliessen|beenden|zumachen)$"));
static MINIMIZE_ALL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(minimi[sz]e all|minimi[sz]e all windows|minimi[sz]e everything|show desktop|show the desktop|show me the desktop|alle fenster minimieren|alles minimieren|desktop anzeigen|zeig den desktop|zeige den desktop)$")
});
static WINDOW_ACTION: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(minimi[sz]e|maximi[sz]e|restore|focus|switch to|bring up|go to|wechsel zu|wechsle zu|zeig mir|zeige)\s+(.+?)$")
});
static WINDOW_ACTION_DE: LazyLock<Regex> = LazyLock::new(|| re(r"^(.+?)\s+(minimieren|maximieren|wiederherstellen)$"));
static SNAP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:snap|move|put|schieb|verschieb|verschiebe)\s+(.+?)\s+(?:to the\s+|nach\s+|to\s+|auf die\s+)?(left|right|links|rechts)(?:\s+side|\s+seite|\s+hälfte)?$")
});

// ---- The HUD restyling itself ("make the interface red", "hide the system panel") ----------
// Every rule below only fires when its captured part really names a colour, theme, layout,
// panel or window position; otherwise parsing continues with the other rules.

/// The HUD itself, as the object of a window action.
const HUD_SELF: &str = r"(?:yourself|your (?:window|interface|hud|gui)|the (?:hud|interface|gui)|dich|dein fenster)";

static HUD_SHOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:show yourself|show your (?:window|interface|hud|gui|face)|open (?:your (?:window|interface|hud|gui)|the (?:interface|hud|gui))|bring (?:yourself|your window|the hud|the interface) (?:up|back|forward|to the front)|come back|appear|zeig dich|zeige dich|komm zurück|komm her|öffne (?:dein|das) (?:fenster|interface|hud))$")
});
static HUD_HIDE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:hide yourself|hide your (?:window|interface|hud|gui)|hide the (?:interface|hud|gui)|close your (?:window|interface|hud)|go (?:to|into) the (?:tray|background)|go to tray|disappear|vanish|versteck dich|verstecke dich|verschwinde|geh in den hintergrund|ab in den hintergrund|schließe dein fenster|schliesse dein fenster)$")
});
static HUD_MIN_MAX: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(?:(minimi[sz]e|maximi[sz]e) {HUD_SELF}|(minimiere|minimier|maximiere|maximier) dich)$"))
});
static HUD_PLACE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:move|put|place|dock|snap|go|schieb|schiebe|verschieb|verschiebe|beweg|bewege|geh|gehe)\s+{HUD_SELF}\s+(?:(?:to|into|in|on|nach|in die|auf die|an den|an die)\s+)?(?:the\s+)?(.+?)$"
    ))
});
static HUD_WINDOW_SIZE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make\s+)?(?:yourself|your window|the hud window|your hud)\s+(bigger|larger|smaller)$|^(?:mach|mache)\s+(?:dich|dein fenster)\s+(größer|groesser|grösser|kleiner)$")
});
/// "Exit full screen": Jarvis himself if he is the one in full screen, otherwise the app in focus
/// (decided by the `window_management` tool).
static FULLSCREEN_OFF: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:exit|leave|stop|quit|end|close|disable|turn off|cancel)\s+full[ -]?screen(?:\s+mode)?$|^full[ -]?screen\s+off$|^vollbild(?:modus)?\s+(?:aus|beenden|verlassen)$|^(?:verlass|verlasse|beende|beend)\s+(?:den\s+)?vollbild(?:modus)?$|^kein vollbild(?:\s+mehr)?$")
});
static HUD_ON_TOP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:stay|keep yourself|keep your window|keep the hud|be|pin yourself|pin your window|put yourself)\s+)?(?:always\s+)?on top(?:\s+of (?:everything|other windows|all windows|all other windows))?$|^(?:bleib|bleibe)\s+(?:immer\s+)?(?:im vordergrund|ganz oben|oben)$|^immer im vordergrund$")
});
static HUD_NOT_ON_TOP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:dont|do not|stop|no longer|quit)\s+(?:staying|stay|keeping|keep|being|be)?\s*(?:always\s+)?on top|unpin yourself|always on top off|turn off always on top|disable always on top)$|^(?:nicht (?:mehr\s+)?(?:immer\s+)?im vordergrund|(?:bleib|bleibe)\s+nicht\s+(?:mehr\s+)?im vordergrund)$")
});
static HUD_RESET: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:reset|restore|default)\s+(?:(?:the|your)\s+)?(?:interface|gui|ui|hud|layout|theme|design|look|colou?rs?|appearance|everything)(?:\s+(?:back\s+)?to\s+(?:the\s+|its\s+|those\s+)?(?:defaults?|standard|normal|original)(?:\s+settings)?)?$|^(?:change|set|put|turn|switch|bring|make)\s+(?:(?:the|your|it|everything|this)\s+)?(?:(?:interface|gui|ui|hud|layout|theme|design|look|colou?rs?|appearance)\s+)?back\s+(?:to\s+)?(?:the\s+|its\s+|those\s+|how it was|normal|default)(?:\s*(?:defaults?|standard|original|settings|colou?rs?|look|theme|design))*$|^(?:go\s+)?back\s+to\s+(?:the\s+|those\s+)?defaults?(?:\s+(?:settings|theme|look|design|colou?rs?))?$|^(?:use\s+)?(?:the\s+)?default\s+(?:settings|look|theme|design|colou?rs?|interface)$|^(?:zurück\s+zum|zurück\s+auf)\s+(?:standard|normal)(?:design|einstellungen)?$|^(?:stell|stelle|setz|setze|mach|mache)\s+(?:(?:das|die|dein|deine|alles)\s+)?(?:(?:interface|design|oberfläche|oberflaeche|layout|farben|aussehen|hud)\s+)?(?:wieder\s+)?(?:auf|zurück auf)\s+(?:standard|normal)$|^(?:interface|design|oberfläche|oberflaeche|layout|farben|aussehen|hud)\s+zurücksetzen$|^(?:setz|setze)\s+(?:(?:das|die|dein|deine)\s+)?(?:interface|design|oberfläche|oberflaeche|layout|farben|aussehen|hud)\s+zurück$")
});
/// "Open the text interface", "pop up the text field", "close the console", "Textkonsole
/// öffnen": the HUD's slide-out text console. Group 1/2/3: the verb.
static HUD_CONSOLE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(open|show|bring up|pull up|pop up|open up|close|hide|shut)\s+(?:me\s+)?(?:(?:in|up)\s+)?(?:(?:the|your|my|a)\s+)?(?:text\s+(?:interface|console|input|field|box|chat|window)|(?:command\s+)?console|chat\s+(?:box|console))$|^(öffne|oeffne|zeig|zeige|schließ|schließe|schliess|schliesse)\s+(?:mir\s+)?(?:die\s+|deine\s+)?(?:text\s?konsole|konsole|texteingabe|textfeld)$|^(?:text\s?konsole|konsole|textfeld)\s+(öffnen|oeffnen|zeigen|schließen|schliessen)$")
});
/// "Show me your core", "open your core", "hide core", "core view", "Zeig mir deinen Kern" /
/// "close your core" (Whisper's "close your car" is repaired to this by `typos::core_homophones`).
static HUD_CORE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(show|open|display|bring up|pull up|bring back|close|hide|exit|leave|collapse|minimi[sz]e)\s+(?:me\s+)?(?:(?:your|the|my|ur)\s+)?core(?:\s+(?:view|architecture|sphere))?(?:\s+again)?$|^(core)\s+view$|^(zeig|zeige|öffne|oeffne|schließ|schließe|schliess|schliesse|versteck|verstecke)\s+(?:mir\s+)?(?:deinen|den|dein)\s+(?:kern|core)$")
});
/// "Bring back the screen", "restore the screen", "screen back on": the last board comes back.
static BOARD_RESTORE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:go\s+)?back\s+to\s+(?:the\s+)?(?:board|hud board|screen)$|^(?:bring|put|get)\s+(?:back\s+)?(?:the|my|your)\s+(?:screen|board|hud board|blocks|report)\s*(?:back)?(?:\s+(?:up|on))?$|^(?:bring|get)\s+back\s+(?:the|my|your)\s+(?:screen|board|hud board|blocks)$|^(?:restore|reopen)\s+(?:the|my|your)\s+(?:screen|blocks|last board|hud board)$|^(?:screen|board)\s+(?:back(?:\s+(?:on|up))?|up)$|^(?:turn|switch)\s+(?:the\s+)?screen\s+back\s+on$|^(?:hol|bring)\s+(?:den\s+)?(?:bildschirm|screen)\s+zurück$")
});
/// "Dock all windows back inside Jarvis" (the name is stripped by `normalize`), "bring all the
/// windows back in".
static DOCK_ALL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^dock\s+(?:all\s+)?(?:(?:the|your|my)\s+)?(?:windows|views|blocks)(?:\s+back)?(?:\s+(?:in|into|inside|onto)(?:\s+(?:the\s+)?(?:hud|board|main window|app|yourself))?)?$|^(?:bring|put|move|pull)\s+(?:all\s+)?(?:(?:the|your|my)\s+)?(?:windows|views)\s+back\s+(?:in|into|inside|onto)(?:\s+(?:the\s+)?(?:hud|board|main window|app|yourself))?$|^(?:hol|hole)\s+alle\s+fenster\s+(?:wieder\s+)?(?:rein|zurück)$")
});
/// "Detach the telemetry window", "pop out the plotter", "undock the schedule".
static DETACH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:detach|pop out|undock|float)\s+(?:the\s+|my\s+|your\s+|that\s+)?(.+?)(?:\s+(?:window|block|view))?(?:\s+(?:into|in|as)\s+(?:a\s+|its\s+)?(?:separate|new|own)\s+window)?$|^(?:put|move)\s+(?:the\s+|my\s+|your\s+)?(.+?)\s+(?:into|in)\s+(?:a\s+|its\s+)?(?:separate|new|own)\s+window$")
});
/// A navigation dock tab named on its own: "home view", "go to the sim lab", "HUD board".
static NAV_VIEW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:go\s+(?:to\s+)?|switch\s+to\s+|back\s+to\s+)?(?:the\s+)?(home|home screen|emblem|hud board|3d core|sim lab|simulation lab|telemetry|schedule|text console)(?:\s+(?:view|tab))?$")
});
/// "Make your core bigger", "core to 200 percent", "rotate your core", "spin faster".
static HUD_CORE_SCALE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make\s+)?(?:your|the)\s+core\s+(bigger|larger|smaller)$|^(?:set\s+|scale\s+)?(?:your|the)\s+core\s+(?:to\s+|scale\s+(?:to\s+)?)?(\d{2,3})\s*(?:%|percent)$")
});
static HUD_CORE_SPIN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(rotate|spin)\s+(?:your|the)\s+core$|^spin\s+(?:your\s+core\s+|it\s+)?(faster|slower)$|^(stop)\s+(?:spinning|rotating)(?:\s+(?:your|the)\s+core)?$")
});
/// "Reboot your language model", "restart LLM", "reboot brain", "Starte dein Sprachmodell neu".
static LLM_REBOOT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:reboot|restart|reset|reload)\s+(?:your\s+|the\s+)?(?:language\s+model|llm|brain|language\s+engine|ai\s+model)$|^(?:starte|start)\s+(?:dein|das)\s+sprachmodell\s+neu$|^sprachmodell\s+neu\s+starten$")
});
/// "Windowed mode", "switch to windowed mode", "Fenstermodus": the HUD board leaves fullscreen.
static HUD_WINDOWED: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:switch|go|change)\s+(?:back\s+)?(?:to|into)\s+|use\s+)?(?:windowed|window)\s+mode$|^windowed$|^(?:exit|leave)\s+fullscreen\s+(?:yourself|jarvis|hud|board)$|^(?:wechsel|wechsle|geh|gehe)?\s*(?:in\s+den\s+)?fenstermodus$")
});
/// "Open the interface settings", "show me your settings", "Einstellungen öffnen".
static HUD_SETTINGS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:open|show|bring up|pull up|show me|display)\s+(?:me\s+)?(?:(?:the|your|my)\s+)?(?:interface\s+|hud\s+|gui\s+|design\s+|look\s+|appearance\s+)?(?:settings|options|preferences|settings window|settings menu)$|^(?:öffne|oeffne|zeig|zeige)\s+(?:mir\s+)?(?:die\s+|deine\s+)?(?:interface[- ]?|design[- ]?)?einstellungen$|^einstellungen\s+(?:öffnen|oeffnen|zeigen)$")
});
/// "Make it darker", "a bit lighter", "mach es dunkler": the depth of the dark-glass background.
static HUD_SHADE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make|turn|set|go)\s+(?:it|this|the (?:interface|background|hud|gui|ui|theme|window|design|colou?rs?)|your (?:interface|background|window|hud)|everything|yourself|the whole (?:thing|interface))\s+(?:a\s+(?:bit|little|lot)\s+|much\s+|even\s+|slightly\s+|way\s+)?(darker|lighter|brighter|deeper|blacker)$|^(?:a\s+(?:bit|little)\s+|much\s+|even\s+)?(darker|lighter|brighter)(?:\s+(?:please|still))?$|^(?:mach|mache)\s+(?:es|das|den hintergrund|das interface|die oberfläche|alles|dich|das design)\s+(?:etwas\s+|noch\s+|ein bisschen\s+|viel\s+)?(dunkler|heller)$|^(?:noch\s+|etwas\s+)?(dunkler|heller)$")
});
static HUD_ZOOM: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(zoom in|zoom out|reset (?:the\s+)?zoom|zoom reset|normal zoom|default zoom|reinzoomen|rauszoomen|hineinzoomen|herauszoomen|zoom zurücksetzen|bigger text|larger text|smaller text)$")
});
static HUD_TEXT_SIZE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make\s+)?(?:(?:the|your|my)\s+)?(?:text|font|fonts|writing|letters|interface|ui|gui|hud|everything|display)\s+(bigger|larger|smaller|tinier|huge|tiny|normal|normal size|default size)$|^(?:mach|mache)\s+(?:(?:die|das|den)\s+)?(?:schrift|text|interface|oberfläche|oberflaeche|alles|hud|anzeige)\s+(größer|groesser|grösser|kleiner|normal)$|^(?:schrift|text)\s+(größer|groesser|kleiner)$")
});
static HUD_ZOOM_SET: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:set\s+)?(?:the\s+)?(?:zoom|scale|ui scale|interface scale|text size|font size|skalierung)\s+(?:to\s+|auf\s+|at\s+)?(\d{2,3})\s*(?:%|percent|prozent)?$")
});
static HUD_OPACITY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make\s+)?(?:(?:the|your)\s+)?(?:window|interface|hud|background|yourself|gui|ui|everything)\s+(?:a bit\s+|a little\s+|slightly\s+)?((?:more|less)\s+(?:transparent|see[- ]through|opaque|solid)|transparent|see[- ]through|translucent|opaque|solid|non[- ]transparent)$|^(?:mach|mache)\s+(?:dich|das fenster|das interface|die oberfläche|den hintergrund|das hud|alles)\s+(durchsichtig|durchsichtiger|transparent|transparenter|undurchsichtig|weniger transparent|weniger durchsichtig)$")
});
static HUD_OPACITY_SET: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:set\s+)?(?:the\s+)?(?:(?:window|interface|hud|background)\s+)?(opacity|transparency|deckkraft|transparenz|durchsichtigkeit)\s+(?:to\s+|auf\s+)?(\d{1,3})\s*(?:%|percent|prozent)?$")
});
static HUD_ONLY_ORB: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:just|only)\s+(?:show\s+)?(?:the\s+)?(?:orb|reactor)$|^nur\s+(?:noch\s+)?(?:den\s+)?(?:reaktor|orb)$"));
static HUD_BACKGROUND: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make|turn|change|set|paint|switch)\s+(?:(?:the|your)\s+)?(?:background|backdrop)(?:\s+colou?r)?\s+(?:(?:to|into)\s+)?(.+?)$|^(?:(?:use|give me|set|switch to)\s+)?(?:(?:a|an|the)\s+)?(.+?)\s+background(?:\s+colou?r)?$|^(?:mach|mache|färb|färbe|faerbe|stell|stelle|setz|setze|ändere|änder|aendere)\s+(?:den\s+)?hintergrund\s+(?:(?:auf|zu|in)\s+)?(.+?)$|^hintergrund(?:farbe)?\s+(?:(?:auf|zu|in)\s+)?(.+?)$|^(.+?)\s+hintergrund$")
});
/// "Red theme", "switch to the Ultron theme", "dark mode", "Kompaktmodus".
static HUD_THEME_MODE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:switch|change|go|turn)\s+(?:to|into)\s+|(?:use|activate|enable|apply|load|start)\s+|(?:wechsel|wechsle|geh|gehe)\s+(?:zu|zum|zur|in den|in die|auf)\s+|(?:aktiviere|aktivier|nimm|nutze)\s+)?(?:(?:the|a|den|das|die|dem)\s+)?(.+?)\s*(theme|mode|modus|design|look|skin|style|stil|ansicht|view|layout|colou?rs|farben|thema)$")
});
static HUD_COLOUR: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make|turn|change|set|switch|paint|colou?r)\s+(?:(?:the|your|my)\s+)?(?:(?:interface|gui|ui|hud|display|screen|theme|seam|steam|team|scheme|design|look|accents?|accent colou?rs?|colou?r scheme|colou?rs?|everything|yourself|window|reactor|orb|highlights?)\s+)?(?:colou?rs?\s+)?(?:(?:to|into|in)\s+)?(?:the\s+)?(.+?)$")
});
/// "Create a custom theme called Cyberpunk with neon yellow borders", "design a dark stealth theme"
/// (STT: seam / steam / scheme).
static THEME_CREATE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:create|build|design|generate|make|erstell|erstelle|entwirf|entwerfe)\s+(?:me\s+)?(?:a|an|my|ein|eine|einen)\s+(?:new\s+|custom\s+|neue[ns]?\s+|eigene[ns]?\s+)*(.*?)\s*(?:theme|seam|steam|scheme|thema)(?:\s+(?:called|named|namens|genannt)\s+(.+?))?(?:\s+(?:with|mit)\s+(.+))?$")
});
static THEME_SAVE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:save|store|speicher|speichere)\s+(?:this|the|my|the current|current|dieses|das|mein)?\s*(?:theme|seam|steam|scheme|look|design|thema)\s+(?:as|als|under|unter)\s+(.+)$")
});
static THEME_LOAD: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:load|lade)\s+(?:my|the|mein|meine|das)?\s*(.+?)\s+(?:theme|seam|steam|scheme|thema)$"));
static WINDOWS_OVERLAP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:overlap|stack|cascade)\s+(?:the\s+|my\s+|all\s+|your\s+)?(?:windows|blocks|panels|them)|let\s+(?:the\s+)?windows\s+overlap)$")
});
static WINDOWS_APART: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:keep\s+(?:the\s+)?windows\s+apart|(?:don'?t|do not|stop)\s+overlap(?:ping)?(?:\s+the\s+windows)?|unstack\s+(?:the\s+)?windows)$")
});
static SPAN_ALL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:use|span(?:\s+across)?|(?:stretch|spread)(?:\s+yourself|\s+the\s+hud)?\s+(?:over|across))\s+(?:all|every|both)\s+(?:of\s+)?(?:my\s+|the\s+)?(?:monitors?|screens?|displays?)$")
});
static SPAN_ONE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:use|go back to|back to)\s+(?:only\s+)?(?:one|a single|1)\s+(?:monitor|screen|display)$"));
/// "Move your orb back to the middle", "put the emblem on the right side", "move the orb slightly
/// left", "bewege deinen Orb in die Mitte".
static ORB_TO: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:move|put|place|drag|bring|send|shift)\s+(?:the\s+|your\s+|my\s+)?(?:orb|emblem|core|sphere)\s+(?:back\s+)?(?:(?:over\s+)?(?:to|in|into|on|at)\s+)?(?:the\s+)?(.+)|(?:beweg|bewege|schieb|schiebe|setz|setze|pack|packe)\s+(?:den\s+|deinen\s+|die\s+|deine\s+)?(?:orb|kugel|emblem)\s+(?:wieder\s+)?(?:(?:in die|in den|auf die|zur|zum|nach)\s+)?(.+))$")
});

/// "night ops" → "Night Ops" (a saved theme's name as Sir would write it).
fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Custom themes, the window packer, spanning monitors and the emblem's spot →
/// `configure_interface` arguments.
fn layout_and_theme_command(s: &str) -> Option<Value> {
    if let Some(c) = THEME_CREATE.captures(s) {
        let description = [c.get(1), c.get(3)].into_iter().flatten().map(|m| m.as_str().trim()).filter(|d| !d.is_empty()).collect::<Vec<_>>().join(" ");
        let mut spec = json!({ "description": description });
        if let Some(name) = c.get(2) {
            spec["name"] = json!(title_case(name.as_str()));
        }
        return Some(json!({ "create_theme": spec }));
    }
    if let Some(c) = THEME_SAVE.captures(s) {
        return Some(json!({ "save_theme": title_case(&c[1]) }));
    }
    if let Some(c) = THEME_LOAD.captures(s) {
        return Some(json!({ "load_theme": title_case(&c[1]) }));
    }
    if WINDOWS_OVERLAP.is_match(s) || WINDOWS_APART.is_match(s) {
        return Some(json!({ "allow_window_overlap": WINDOWS_OVERLAP.is_match(s) }));
    }
    if SPAN_ALL.is_match(s) || SPAN_ONE.is_match(s) {
        return Some(json!({ "span_all_monitors": SPAN_ALL.is_match(s) }));
    }
    let c = ORB_TO.captures(s)?;
    let spot = c.get(1).or(c.get(2))?.as_str().trim();
    interface::board_spot(spot).or_else(|| interface::nudge(spot, None)).map(|_| json!({ "orb_board_position": spot }))
}

static HUD_COLOUR_DE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:mach|mache|färbe|färb|faerbe|stell|stelle|setz|setze|ändere|änder|aendere|wechsel|wechsle)\s+(?:(?:das|die|den|dein|deine|deinen|mein|meine)\s+)?(?:(?:interface|gui|ui|hud|oberfläche|oberflaeche|design|farbe|farben|akzentfarbe|anzeige|fenster|theme|thema|bildschirm|dich|alles|aussehen|reaktor)\s+)?(?:(?:auf|zu|in|nach)\s+)?(.+?)$")
});
static HUD_PANEL_HIDE: LazyLock<Regex> = LazyLock::new(|| {
    // Not "minimize": "minimize everything" means the desktop.
    re(r"^(?:hide|close|remove|collapse|turn off|switch off|disable|get rid of)\s+(?:(?:the|your|my)\s+)?(.+?)$|^(?:versteck|verstecke|verberge|verbirg|schließ|schließe|schliess|schliesse|entferne|entfern)\s+(?:(?:den|die|das|dein|deine|deinen)\s+)?(.+?)$")
});
static HUD_PANEL_SHOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:show|open|display|bring back|restore|turn on|switch on|enable|unhide|expand|reveal)\s+(?:me\s+)?(?:(?:the|your|my)\s+)?(.+?)(?:\s+again)?$|^(?:zeig|zeige|öffne|oeffne|öffn)\s+(?:mir\s+)?(?:(?:den|die|das|dein|deine|deinen)\s+)?(.+?)(?:\s+(?:wieder an|wieder|an))?$")
});
/// "Blende das Systempanel aus", "Chat einblenden".
static HUD_PANEL_DE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:blende|blend)\s+(?:(?:den|die|das|dein|deine|deinen)\s+)?(.+?)\s+(aus|ein)$|^(?:(?:den|die|das|dein|deine|deinen)\s+)?(.+?)\s+(ausblenden|verstecken|schließen|schliessen|einblenden|anzeigen|zeigen|öffnen|oeffnen)$")
});

/// First non-empty capture group.
fn capture<'t>(c: &regex::Captures<'t>) -> Option<&'t str> {
    c.iter().skip(1).flatten().map(|m| m.as_str().trim()).find(|s| !s.is_empty())
}

/// A theme, layout or colour named in a request → `configure_interface` arguments.
fn look_args(name: &str, prefer_layout: bool) -> Option<Value> {
    let layout = || HudLayout::parse(name).map(|l| json!({ "layout": l.id() }));
    let theme = || interface::theme(name).map(|_| json!({ "theme": name }));
    let colour = || interface::parse_color(name).map(|_| json!({ "accent_color": name }));
    if prefer_layout {
        layout().or_else(theme).or_else(colour)
    } else {
        theme().or_else(colour).or_else(layout)
    }
}

/// Requests to restyle or move the HUD → `configure_interface` arguments.
fn interface_command(s: &str) -> Option<Value> {
    if HUD_SHOW.is_match(s) {
        return Some(json!({ "window": "show" }));
    }
    if HUD_HIDE.is_match(s) {
        return Some(json!({ "window": "hide" }));
    }
    if let Some(c) = HUD_MIN_MAX.captures(s) {
        let verb = capture(&c)?;
        return Some(json!({ "window": if verb.starts_with("min") { "minimize" } else { "maximize" } }));
    }
    if let Some(c) = HUD_PLACE.captures(s) {
        if let Some(cmd @ (WindowCommand::Place(_) | WindowCommand::Center)) = capture(&c).and_then(WindowCommand::parse) {
            let id = match cmd {
                WindowCommand::Place(corner) => format!("{corner:?}"),
                _ => "center".to_string(),
            };
            return Some(json!({ "window": to_snake(&id) }));
        }
    }
    if let Some(c) = HUD_WINDOW_SIZE.captures(s) {
        let bigger = !matches!(capture(&c)?, "smaller" | "kleiner");
        return Some(json!({ "window": if bigger { "bigger" } else { "smaller" } }));
    }
    if HUD_NOT_ON_TOP.is_match(s) {
        return Some(json!({ "always_on_top": false }));
    }
    if HUD_ON_TOP.is_match(s) {
        return Some(json!({ "always_on_top": true }));
    }
    if HUD_RESET.is_match(s) {
        return Some(json!({ "reset": true }));
    }
    // "Open the interface settings": the HUD's own settings window.
    if HUD_SETTINGS.is_match(s) {
        return Some(json!({ "open_settings": true }));
    }
    // "Open the text interface" / "close the console": the slide-out text console.
    if let Some(c) = HUD_CONSOLE.captures(s) {
        let verb = capture(&c)?;
        let close = ["close", "hide", "shut", "schließ", "schliess"].iter().any(|v| verb.starts_with(v));
        return Some(json!({ "text_console": !close }));
    }
    // "Show me your core": the 3D module sphere.
    if let Some(c) = HUD_CORE.captures(s) {
        let verb = capture(&c)?;
        let close = ["close", "hide", "exit", "leave", "schließ", "schliess", "versteck", "collapse", "minimi"].iter().any(|v| verb.starts_with(v));
        return Some(json!({ "core_view": !close }));
    }
    if let Some(c) = HUD_CORE_SCALE.captures(s) {
        let v = capture(&c)?;
        return Some(match v.parse::<f64>() {
            Ok(percent) => json!({ "core_scale": percent }),
            Err(_) => json!({ "core_scale": if v == "smaller" { "smaller" } else { "bigger" } }),
        });
    }
    if let Some(c) = HUD_CORE_SPIN.captures(s) {
        let v = capture(&c)?;
        return Some(json!({ "core_spin": match v { "faster" | "rotate" | "spin" => "faster", "slower" => "slower", _ => "stop" } }));
    }
    // "Windowed mode": the board leaves fullscreen (maximised).
    if HUD_WINDOWED.is_match(s) {
        return Some(json!({ "fullscreen": false }));
    }
    // "Make it darker": a deeper obsidian, never a light colour.
    if let Some(c) = HUD_SHADE.captures(s) {
        let darker = matches!(capture(&c)?, "darker" | "deeper" | "blacker" | "dunkler" | "dark" | "more dark" | "noch dunkler");
        let amount = if s.contains("much") || s.contains("viel") || s.contains("a lot") { 0.3 } else { 0.15 };
        return Some(json!({ "background_shade": if darker { amount } else { -amount } }));
    }
    for re in [&*HUD_ZOOM, &*HUD_TEXT_SIZE] {
        if let Some(c) = re.captures(s) {
            let scale = match capture(&c)? {
                "zoom in" | "reinzoomen" | "hineinzoomen" | "bigger text" | "larger text" | "bigger" | "larger" | "huge"
                | "größer" | "groesser" | "grösser" => "bigger",
                "zoom out" | "rauszoomen" | "herauszoomen" | "smaller text" | "smaller" | "tinier" | "tiny" | "kleiner" => {
                    "smaller"
                }
                _ => "normal",
            };
            return Some(json!({ "scale": scale }));
        }
    }
    if let Some(c) = HUD_ZOOM_SET.captures(s) {
        let pct: f64 = capture(&c)?.parse().ok()?;
        return Some(json!({ "scale": pct / 100.0 }));
    }
    if let Some(c) = HUD_OPACITY.captures(s) {
        let opacity = match capture(&c)? {
            w if w.starts_with("more transparent") || w.starts_with("less opaque") || w.starts_with("less solid") => json!("-15"),
            "durchsichtiger" | "transparenter" => json!("-15"),
            w if w.starts_with("less") || w.starts_with("more") || w.starts_with("weniger") => json!("+15"),
            "opaque" | "solid" | "non-transparent" | "non transparent" | "undurchsichtig" => json!(100),
            _ => json!(75),
        };
        return Some(json!({ "opacity": opacity }));
    }
    if let Some(c) = HUD_OPACITY_SET.captures(s) {
        let value: u64 = c.get(2)?.as_str().parse().ok()?;
        let key = if c[1].starts_with("trans") || c[1].starts_with("durch") { "transparency" } else { "opacity" };
        return Some(json!({ key: value }));
    }
    if HUD_ONLY_ORB.is_match(s) {
        return Some(json!({ "layout": "orb" }));
    }
    if let Some(name) = HUD_BACKGROUND.captures(s).as_ref().and_then(capture) {
        if interface::parse_color(name).is_some() {
            return Some(json!({ "background_color": name }));
        }
    }
    if let Some(args) = layout_and_theme_command(s) {
        return Some(args);
    }
    if let Some(c) = HUD_THEME_MODE.captures(s) {
        let prefer_layout = matches!(&c[2], "mode" | "modus" | "layout" | "view" | "ansicht");
        if let Some(args) = look_args(c[1].trim(), prefer_layout) {
            return Some(args);
        }
    }
    for re in [&*HUD_COLOUR, &*HUD_COLOUR_DE] {
        if let Some(args) = re.captures(s).as_ref().and_then(capture).and_then(|name| look_args(name, false)) {
            return Some(args);
        }
    }
    if let Some(c) = HUD_PANEL_DE.captures(s) {
        // Groups 1/2: "blende … aus/ein"; groups 3/4: "… ausblenden/einblenden".
        let (name, verb) = match (c.get(1), c.get(2), c.get(3), c.get(4)) {
            (Some(n), Some(v), _, _) | (_, _, Some(n), Some(v)) => (n.as_str(), v.as_str()),
            _ => return None,
        };
        if !HudPanel::parse(name).is_empty() {
            let show = matches!(verb, "ein" | "einblenden" | "anzeigen" | "zeigen" | "öffnen" | "oeffnen");
            return Some(json!({ if show { "show_panels" } else { "hide_panels" }: [name] }));
        }
    }
    for (re, key) in [(&*HUD_PANEL_HIDE, "hide_panels"), (&*HUD_PANEL_SHOW, "show_panels")] {
        if let Some(name) = re.captures(s).as_ref().and_then(capture) {
            if !HudPanel::parse(name).is_empty() {
                return Some(json!({ key: [name] }));
            }
        }
    }
    None
}

/// "TopRight" → "top_right".
fn to_snake(id: &str) -> String {
    let mut out = String::new();
    for (i, ch) in id.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.extend(ch.to_lowercase());
    }
    out
}

// ---- Conversational focus ("full screen", "maximize it", "close it", "move it left") --------
// Without a name these act on the app or website the conversation is about (resolved by the
// tools through `focus`), never on Jarvis; he is only the target when Sir says so.

/// Object words for the app, window or website just talked about.
const IT: &str = r"(?:it|that|this|that one|this one|that app|this app|the app|that program|this program|the program|that window|this window|the window|es|das|dies|ihn|sie|das fenster|das programm|die app)";

static FOCUS_FULLSCREEN: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:(?:go|switch to|enter|turn on)\s+)?full[ -]?screen(?:\s+mode)?(?:\s+{IT})?$|^(?:make|put|set|turn|switch|bring)\s+{IT}\s+(?:to\s+|in\s+|into\s+|on\s+)?full[ -]?screen(?:\s+mode)?$|^vollbild(?:modus)?(?:\s+(?:an|ein))?$|^(?:mach|mache|stell|stelle|schalt|schalte)\s+(?:{IT}\s+)?(?:auf\s+|in\s+(?:den\s+)?|zum\s+)?vollbild(?:modus)?(?:\s+an)?$|^(?:wechsel|wechsle|geh|gehe)\s+(?:in den|zum|in|auf)\s+vollbild(?:modus)?$"
    ))
});
static FOCUS_MAX_MIN: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(maximi[sz]e|minimi[sz]e)(?:\s+{IT})?$|^(restore|unminimi[sz]e)\s+{IT}$|^(?:{IT}\s+)?(maximieren|minimieren|wiederherstellen)$|^(maximiere|maximier|minimiere|minimier)\s+{IT}$"
    ))
});
static FOCUS_SNAP: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:move|snap|put|push|drag|shift|schieb|schiebe|verschieb|verschiebe|pack)\s+{IT}\s+(?:to\s+the\s+|to\s+|on\s+the\s+|onto\s+the\s+|nach\s+|auf\s+die\s+|an\s+den\s+|in\s+die\s+)?(left|right|links|rechts|top|bottom|oben|unten)(?:\s+(?:side|half|seite|hälfte|haelfte|half of the screen))?$|^(?:snap|move)\s+(?:to\s+the\s+)?(left|right|top|bottom)$"
    ))
});
static FOCUS_FRONT: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(?:focus|switch to|go back to)\s+{IT}$|^bring\s+{IT}\s+(?:up|back|to the front|forward)$|^(?:hol|hole)\s+(?:es|das|ihn|sie|das fenster)\s+(?:nach vorne|zurück|her)$"))
});
static FOCUS_CLOSE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:close|quit|exit|kill|shut)\s+(?:it|that|this|that one|this one|that app|this app|the app|that program|the program|es|das|dies|ihn|sie|das programm|die app)(?:\s+(?:down|now))?$|^(?:mach|mache)\s+(?:es|das|ihn|sie|das programm|die app)\s+zu$|^(?:schließ|schließe|schliess|schliesse|beende|beend)\s+(?:es|das|dies|ihn|sie|das programm|die app)$")
});
static FOCUS_CLOSE_TAB: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:close|shut)\s+(?:the|that|this)\s+tab$|^(?:schließ|schließe|schliess|schliesse)\s+(?:den|diesen)\s+tab$|^tab\s+(?:schließen|schliessen|zu)$"));

/// Sir explicitly means Jarvis' own window ("maximize Jarvis", "fullscreen yourself", "make the
/// HUD full screen"). Checked on the raw sentence: normalisation drops a trailing "Jarvis".
static SELF_WINDOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:go\s+)?(full ?screen|maximi[sz]e|minimi[sz]e)\s+(?:yourself|jarvis|your\s+(?:own\s+)?(?:window|hud|interface|gui|self)|the\s+(?:hud|interface|gui))\b|\b(?:make|put|set|switch|turn)\s+(?:yourself|jarvis|your\s+(?:window|hud|interface|gui)|the\s+(?:hud|interface|gui))\s+(?:to\s+|in\s+|into\s+)?(full ?screen|maximi[sz]ed|minimi[sz]ed)\b|\bgo\s+(full ?screen)\s+(?:yourself|jarvis)\b|\b(?:mach|mache|schalte|schalt)\s+dich\s+(?:auf\s+|in\s+den\s+)?(vollbild)|\b(maximiere|minimiere)\s+dich\b")
});

/// Lower-case, punctuation-free, but nothing stripped (unlike `normalize`).
fn raw_words(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { ' ' })
        .collect::<String>()
        .replace('-', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// "Maximize Jarvis", "fullscreen yourself" → `configure_interface` arguments for the HUD.
fn self_window_command(text: &str) -> Option<Value> {
    let words = raw_words(text);
    let c = SELF_WINDOW.captures(&words)?;
    let verb = capture(&c)?;
    Some(if verb.starts_with("full") || verb.starts_with("vollbild") {
        json!({ "fullscreen": true })
    } else if verb.starts_with("maxim") {
        json!({ "window": "maximize" })
    } else {
        json!({ "window": "minimize" })
    })
}

/// "Full screen", "maximize it", "close it", "move it to the left" → a tool call without a
/// target, which the tools resolve against the conversation focus.
fn focus_command(s: &str) -> Option<Intent> {
    let tool = |name: &'static str, args: Value| Some(Intent::Tool { name, args });
    let window = |action: &str| tool("window_management", json!({ "action": action }));
    if FULLSCREEN_OFF.is_match(s) {
        return window("exit_fullscreen");
    }
    if FOCUS_FULLSCREEN.is_match(s) {
        return window("fullscreen");
    }
    if let Some(c) = FOCUS_MAX_MIN.captures(s) {
        return window(match capture(&c)? {
            v if v.starts_with("maxim") => "maximize",
            v if v.starts_with("minim") => "minimize",
            _ => "restore",
        });
    }
    if let Some(c) = FOCUS_SNAP.captures(s) {
        return window(match capture(&c)? {
            "left" | "links" => "snap_left",
            "right" | "rechts" => "snap_right",
            "top" | "oben" => "snap_top",
            _ => "snap_bottom",
        });
    }
    if FOCUS_FRONT.is_match(s) {
        return window("focus");
    }
    if FOCUS_CLOSE_TAB.is_match(s) {
        return window("close_tab");
    }
    if FOCUS_CLOSE.is_match(s) {
        return tool("close_application", json!({ "app_name": "it", "force": true }));
    }
    None
}

// ---- Files the conversation is about ("undo that edit", "open that file in VS Code") -------

static UNDO_EDIT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:undo|revert|reverse|roll back|rollback)\s+(?:that|the|my|your|this)?\s*(?:last\s+)?(?:file\s+)?(?:edit|change|changes|modification)(?:\s+(?:to|in|on)\s+(?:the|that|this)\s+file)?$|^(?:mach|mache)\s+(?:das|die|deine|die letzte)?\s*(?:änderung|aenderung|bearbeitung)?\s*rückgängig$|^(?:änderung|aenderung)\s+rückgängig(?:\s+machen)?$")
});
static OPEN_LAST_FILE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:open|show|reveal|öffne|oeffne|zeig|zeige)\s+(?:me\s+|mir\s+)?(?:that|this|the|die|diese|den|diesen)\s+(?:file|document|datei|folder|ordner|dokument)(?:\s+(?:in|with|mit|im)\s+(vs code|vscode|visual studio code|code|the editor|editor|explorer|file explorer|the explorer|the file explorer|dem explorer)|\s+(in (?:the |its )?folder|im ordner))?$")
});

fn file_followup(s: &str) -> Option<Intent> {
    if UNDO_EDIT.is_match(s) {
        return Some(Intent::Tool { name: "undo_last_file_edit", args: json!({}) });
    }
    let c = OPEN_LAST_FILE.captures(s)?;
    let with = match c.get(1).map(|m| m.as_str()).or(c.get(2).map(|_| "explorer")) {
        Some(w) if w.contains("code") || w.contains("editor") => "vscode",
        Some(_) => "explorer",
        None => "default",
    };
    Some(Intent::Tool { name: "open_application", args: json!({ "open_last_file": true, "with": with }) })
}

// ---- Jarvis' extra windows ("make that window transparent", "pin that window") ------------

const JARVIS_WINDOW: &str =
    r"(?:that|this|the new|the second|the other|the extra|the last|your second|your other|your new|the jarvis|that jarvis)\s+window";

static WINDOW_STYLE: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^(?:make|set|turn|switch)\s+{JARVIS_WINDOW}\s+(?:to\s+|into\s+)?(.+)$")));
static WINDOW_PIN: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(pin|unpin)\s+{JARVIS_WINDOW}$|^keep\s+{JARVIS_WINDOW}\s+(on top)$|^(?:dont|do not|stop)\s+keep(?:ing)?\s+{JARVIS_WINDOW}\s+(on top)$"))
});

/// "make that window transparent / borderless / red / Ultron / always on top".
fn window_style_command(s: &str) -> Option<Intent> {
    let style = |args: Value| {
        let mut args = args;
        args["target"] = json!("focus");
        Some(Intent::Tool { name: "customize_hud_and_windows", args })
    };
    if WINDOW_PIN.is_match(s) {
        let on = !s.starts_with("unpin") && !s.starts_with("dont") && !s.starts_with("do not") && !s.starts_with("stop");
        return style(json!({ "always_on_top": on }));
    }
    let what = WINDOW_STYLE.captures(s)?.get(1)?.as_str().trim();
    let what = what.trim_start_matches("a bit ").trim_start_matches("a little ").trim_start_matches("slightly ");
    match what {
        "transparent" | "see through" | "see-through" | "translucent" | "more transparent" => style(json!({ "opacity": 0.6 })),
        "opaque" | "solid" | "less transparent" | "non transparent" => style(json!({ "opacity": 1.0 })),
        "borderless" | "frameless" | "without borders" | "without a border" | "without a title bar" => style(json!({ "borderless": true })),
        "bordered" | "normal" | "with a title bar" | "with borders" => style(json!({ "borderless": false })),
        "always on top" | "stay on top" | "on top" => style(json!({ "always_on_top": true })),
        "bigger text" | "text bigger" | "larger text" => style(json!({ "font_scale": "bigger" })),
        "smaller text" | "text smaller" => style(json!({ "font_scale": "smaller" })),
        other => {
            let name = other.trim_end_matches(" theme").trim_end_matches(" colour").trim_end_matches(" color");
            if interface::theme(name).is_some() {
                style(json!({ "theme": name }))
            } else if interface::parse_color(name).is_some() {
                style(json!({ "accent_color": name }))
            } else {
                None
            }
        }
    }
}

// ---- Websites, creator channels and platform searches -------------------------------------

/// Lower-case with apostrophes, `@`, `/`, `.`, `-`, `+`, `&` and `#` kept (handles, domains,
/// queries), wake word, politeness and fillers stripped.
fn nav_normalize(text: &str) -> String {
    let lower = text.trim().to_lowercase().replace(['’', '`', '´'], "'");
    let cleaned: String = lower.chars().map(|c| if c.is_alphanumeric() || " '@/.-_+&#".contains(c) { c } else { ' ' }).collect();
    let mut s = cleaned.split_whitespace().filter(|w| !matches!(*w, "bitte" | "please" | "mal" | "doch")).collect::<Vec<_>>().join(" ");
    for _ in 0..3 {
        let before = s.clone();
        for prefix in ["hey jarvis ", "jarvis ", "can you ", "could you ", "would you ", "kannst du ", "könntest du ", "ok ", "okay ", "now ", "and ", "und "] {
            if let Some(rest) = s.strip_prefix(prefix) {
                s = rest.to_string();
            }
        }
        for suffix in [" jarvis", " for me", " für mich", " now", " jetzt", " sir"] {
            if let Some(rest) = s.strip_suffix(suffix) {
                s = rest.to_string();
            }
        }
        if s == before {
            break;
        }
    }
    s.trim_end_matches(['.', '?', '!']).trim().to_string()
}

/// `needle` with its original capitalisation ("bastighg" → "BastiGHG").
fn original_case(original: &str, needle: &str) -> String {
    let lower = original.to_lowercase();
    match lower.find(needle) {
        Some(i) if original.is_char_boundary(i) && original.is_char_boundary(i + needle.len()) && lower.len() == original.len() => {
            original[i..i + needle.len()].to_string()
        }
        _ => needle.to_string(),
    }
}

/// Longer forms first: the first alternative that matches wins ("show me X", not "me X").
const NAV_OPEN: &str = r"(?:open up|open|show me|show|go to|pull up|bring up|take me to|navigate to|load|öffne mir|öffne|oeffne|zeig mir|zeige mir|zeige|zeig|geh auf|geh zu|gehe auf|gehe zu)";
const NAV_KIND: &str = r"(?:channel|profile|page|account|stream|repo|repository|kanal|profil|seite|konto)";

static NAV_CHANNEL_ON: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^{NAV_OPEN}\s+(?:the\s+|den\s+|das\s+)?(?:{NAV_KIND}\s+(?:of\s+|von\s+|from\s+)?)?(.+?)(?:'s|s')?\s+(?:on|auf|at|bei)\s+([a-z0-9][a-z0-9 .+&-]{{0,30}}?)(?:[\s-]+{NAV_KIND})?$"
    ))
});
static NAV_CHANNEL_POSSESSIVE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^{NAV_OPEN}\s+(?:the\s+)?(.+?)(?:'s|s')?\s+([a-z0-9][a-z0-9 .+&-]{{0,30}}?)[\s-]+{NAV_KIND}$"))
});
static NAV_CHANNEL_OF: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^{NAV_OPEN}\s+(?:the\s+|den\s+|das\s+)?([a-z0-9][a-z0-9 .+&-]{{0,30}}?)[\s-]+{NAV_KIND}\s+(?:of|from|for|von)\s+(.+)$"))
});
static NAV_SUBREDDIT: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^{NAV_OPEN}\s+(?:the\s+)?(?:(r/\S+)|(.+?)\s+subreddit)$")));
static NAV_SEARCH: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^{NAV_OPEN}\s+([a-z0-9][a-z0-9 .+&-]{{0,30}}?)\s+(?:and|und)\s+(?:then\s+)?(?:search|look|suche|such|find)\s+(?:for\s+|up\s+|nach\s+)?(.+)$|^(?:search|look up|look for|find|suche|such|durchsuche)\s+(?:on\s+|auf\s+|in\s+|bei\s+)?([a-z0-9][a-z0-9 .+&-]{{0,30}}?)\s+(?:for|nach)\s+(.+)$"
    ))
});
static NAV_SEARCH_ON: LazyLock<Regex> = LazyLock::new(|| {
    // Longer verbs first: the first alternative that matches wins ("search for X", not "for X").
    re(r"^(?:search for|search|look up|look for|find|google|suche nach|such nach|suche|such|play|spiele|spiel)\s+(.+?)\s+(?:on|auf|in|at|bei|via)\s+([a-z0-9][a-z0-9 .+&-]{0,30}?)$|^([a-z0-9][a-z0-9 .+&-]{0,30}?)\s+(?:search for|search|suche nach|suche)\s+(.+)$")
});
static NAV_GOOGLE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:google|googel|googl)\s+(?:for\s+|nach\s+)?(.+)$"));
static NAV_DIRECT: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^{NAV_OPEN}\s+(?:the\s+|my\s+)?(.+?)(?:\s+(?:website|site|homepage|webseite|seite|in (?:the|my) browser|im browser))?$|^(?:launch|start|starte)\s+(?:the\s+)?(.+?)\s+(?:website|webseite)$"))
});

/// Wikipedia language: German when Sir asked in German.
fn nav_lang(s: &str) -> &'static str {
    let german = ["öffne", "suche", "such ", "nach ", " auf ", "zeig", "geh ", "kanal", "seite", "über "];
    if german.iter().any(|w| s.contains(w)) { "de" } else { "en" }
}

/// "Open BastiGHG on YouTube", "open YouTube and search for Minecraft", "RTX 3060 Ti on
/// Kleinanzeigen", "open r/rust", "open Twitch" → `open_website`.
fn website_command(text: &str) -> Option<Intent> {
    use crate::sites::find;
    let s = nav_normalize(text);
    let open = |platform: &str, mode: &str, target: Option<String>| {
        let site = find(platform)?;
        let mut args = json!({ "platform_or_url": site.id, "action_type": mode, "lang": nav_lang(&s) });
        if let Some(t) = target.map(|t| t.trim().trim_matches('"').to_string()).filter(|t| !t.is_empty()) {
            args["target_or_query"] = json!(original_case(text, &t));
        }
        Some(Intent::Tool { name: "open_website", args })
    };
    let channel = |platform: &str, who: &str| {
        let site = find(platform)?;
        // German genitive without an apostrophe ("BastiGHGs YouTube-Kanal"): drop the s.
        let who = if s.contains("kanal") || s.contains("profil") { who.strip_suffix('s').filter(|w| w.len() > 2).unwrap_or(who) } else { who };
        let who = who.trim_start_matches("the ").trim();
        if who.is_empty() || who.split_whitespace().count() > 4 || is_pronoun_word(who) {
            return None;
        }
        // A site without profile pages: search it for the name instead.
        open(site.id, if site.profile_url("x").is_some() { "channel_or_profile" } else { "site_search" }, Some(who.to_string()))
    };
    if let Some(c) = NAV_SUBREDDIT.captures(&s) {
        let sub = c.get(1).or(c.get(2))?.as_str();
        return open("reddit", "channel_or_profile", Some(sub.trim_start_matches("r/").to_string()));
    }
    if let Some(c) = NAV_SEARCH.captures(&s) {
        let (platform, query) = match (c.get(1), c.get(2), c.get(3), c.get(4)) {
            (Some(p), Some(q), _, _) | (_, _, Some(p), Some(q)) => (p.as_str(), q.as_str()),
            _ => return None,
        };
        if let Some(intent) = open(platform, "site_search", Some(query.to_string())) {
            return Some(intent);
        }
    }
    if let Some(c) = NAV_SEARCH_ON.captures(&s) {
        let (query, platform) = match (c.get(1), c.get(2), c.get(3), c.get(4)) {
            (Some(q), Some(p), _, _) => (q.as_str(), p.as_str()),
            (_, _, Some(p), Some(q)) => (q.as_str(), p.as_str()),
            _ => return None,
        };
        if let Some(intent) = open(platform, "site_search", Some(query.to_string())) {
            return Some(intent);
        }
    }
    for (re, who_first) in [(&*NAV_CHANNEL_OF, false), (&*NAV_CHANNEL_ON, true), (&*NAV_CHANNEL_POSSESSIVE, true)] {
        if let Some(c) = re.captures(&s) {
            let (who, platform) = if who_first { (c.get(1)?.as_str(), c.get(2)?.as_str()) } else { (c.get(2)?.as_str(), c.get(1)?.as_str()) };
            if let Some(intent) = channel(platform, who) {
                return Some(intent);
            }
        }
    }
    if let Some(c) = NAV_GOOGLE.captures(&s) {
        let query = c.get(1)?.as_str();
        if find(&format!("google {query}")).is_none() {
            return open("google", "site_search", Some(query.to_string()));
        }
    }
    if let Some(c) = NAV_DIRECT.captures(&s) {
        let name = capture(&c)?;
        // An installed app of that name (Spotify, Discord, Steam …) is opened as the app.
        if find(name).is_some() && !known_app(name) {
            return open(name, "direct_url", None);
        }
    }
    None
}

fn is_pronoun_word(w: &str) -> bool {
    crate::focus::is_pronoun(w) || matches!(w, "me" | "him" | "her" | "them" | "something" | "anything" | "mir" | "ihm")
}

/// Words that suggest a sentence is a real question/request for the LLM, not an app name.
fn looks_like_app_name(name: &str) -> bool {
    let words = name.split_whitespace().count();
    let blacklist = [
        "the door", "a ", "my mind", "conversation", "the conversation", "with", "about", "how", "why", "what", "wie",
        "warum", "was", "and ", " und ", "that", "dass", "this", "it ", "es ", "timer", "a timer", "reminder",
        "meeting", "file", "datei", "folder with", "new", "neue", "skill", "yourself", "your ", "dich", "jarvis",
    ];
    words <= 4 && !blacklist.iter().any(|b| name == b.trim() || name.contains(b))
}

// ---- Live windows, Jarvis' windows, monitors, music and Stark Protocols --------------------
// These are the requests the small local model tends to *answer* ("I'm creating a window with
// your PC stats, Sir") without calling a tool, so they never reach it: they run instantly.

/// A monitor named in a phrase: "monitor 2", "the second screen", "my left monitor".
static MONITOR_PHRASE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:the\s+|my\s+|den\s+|meinen\s+|dem\s+|mein\s+|die\s+|meinem\s+)?(?:(first|second|third|fourth|1st|2nd|3rd|4th|other|next|left|right|primary|main|erste[nm]?|zweite[nm]?|dritte[nm]?|vierte[nm]?|andere[nm]?|linke[nm]?|rechte[nm]?|nächste[nm]?|haupt)\s*(?:monitor|screen|display|bildschirm)|(?:monitor|screen|display|bildschirm)\s*(\d|one|two|three|four|eins|zwei|drei|vier))$")
});
/// Anywhere in a sentence ("… on my second monitor").
static MONITOR_ANYWHERE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:on|to|onto|auf|zum|nach)\s+((?:the\s+|my\s+|den\s+|meinen\s+|dem\s+|meinem\s+)?(?:(?:first|second|third|fourth|other|next|left|right|primary|main|erste[nm]?|zweite[nm]?|dritte[nm]?|andere[nm]?|linke[nm]?|rechte[nm]?|haupt)\s*(?:monitor|screen|display|bildschirm)|(?:monitor|screen|display|bildschirm)\s*(?:\d|one|two|three|four|eins|zwei|drei)))\b")
});
static WINDOW_WORD: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:window|windows|fenster)\b"));
static CREATE_VERB: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:create|make|open|spawn|show|showing|shows|display|give|put|pop|bring|start|launch|want|need|like|have|see|get|erstelle|erstell|mach|öffne|oeffne|zeig|zeige|gib)\b")
});
static NOT_CREATE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:close|shut|hide|move|resize|smaller|bigger|larger|minimi[sz]e|maximi[sz]e|schließ|schließe|schliess|verschieb|verschiebe|kleiner|größer|transparent|borderless|pin|unpin|add|remove)\b")
});
static KIND_DRIVES: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:drives?|disks?|storage|disk space|drive space|free space|remaining space|space left|hard drives?|festplatten?|speicherplatz|laufwerke?|ssds?|hdds?)\b")
});
static KIND_PROCESSES: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(?:processes|process list|process monitor|task manager|running programs|top processes|prozesse|taskmanager)\b"));
static KIND_NETWORK: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(?:network|internet speed|bandwidth|network traffic|download speed|upload speed|netzwerk)\b"));
static KIND_PC_STATS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:pc stats|pc statistics|stats|statistics|system stats|system status|system info|performance|telemetry|hardware|cpu|gpu|ram|vram|memory usage|temperatures?|usage|auslastung|leistung|systemstatus|computer stats|pc werte)\b")
});
static SIZE_PAIR: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{3,4})\s*(?:x|by|times|mal|×)\s*(\d{3,4})\b"));

/// The live views a sentence names, in the order it names them.
fn live_kinds_in(s: &str) -> Vec<&'static str> {
    let mut found: Vec<(usize, &'static str)> = [(&*KIND_DRIVES, "drive_space"), (&*KIND_PROCESSES, "processes"), (&*KIND_NETWORK, "network"), (&*KIND_PC_STATS, "pc_stats")]
        .iter()
        .filter_map(|(re, kind)| re.find(s).map(|m| (m.start(), *kind)))
        .collect();
    // "remaining space on my drives" is not also "pc stats" because it says "usage"…
    if found.iter().any(|(_, k)| *k == "drive_space") && !s.contains("stats") && !s.contains("cpu") && !s.contains("gpu") && !s.contains("ram") {
        found.retain(|(_, k)| *k != "pc_stats");
    }
    found.sort_by_key(|(i, _)| *i);
    found.into_iter().map(|(_, k)| k).collect()
}

/// The monitor a phrase names, as the tools take it (a number or a word they resolve).
fn monitor_value(phrase: &str) -> Option<Value> {
    let c = MONITOR_PHRASE.captures(phrase.trim())?;
    if let Some(n) = c.get(2) {
        let n = match n.as_str() {
            "one" | "eins" => 1,
            "two" | "zwei" => 2,
            "three" | "drei" => 3,
            "four" | "vier" => 4,
            d => d.parse().ok()?,
        };
        return Some(json!(n));
    }
    let word = c.get(1)?.as_str();
    Some(match word {
        "first" | "1st" | "erste" | "ersten" | "erstem" => json!(1),
        "second" | "2nd" | "zweite" | "zweiten" | "zweitem" => json!(2),
        "third" | "3rd" | "dritte" | "dritten" | "drittem" => json!(3),
        "fourth" | "4th" | "vierte" | "vierten" => json!(4),
        "main" | "haupt" => json!("primary"),
        "andere" | "anderen" | "anderem" => json!("other"),
        "linke" | "linken" | "linkem" => json!("left"),
        "rechte" | "rechten" | "rechtem" => json!("right"),
        "nächste" | "nächsten" | "nächstem" => json!("next"),
        other => json!(other),
    })
}

/// "Create a new window that shows my PC stats", "please create a new window showing my
/// remaining space on my drives", "show my processes in a window on monitor 2".
fn live_window_request(s: &str) -> Option<Value> {
    if !WINDOW_WORD.is_match(s) || !CREATE_VERB.is_match(s) || NOT_CREATE.is_match(s) {
        return None;
    }
    let kinds = live_kinds_in(s);
    let first = kinds.first()?;
    let mut args = json!({ "live": first });
    if kinds.len() > 1 {
        args["add_content"] = json!(kinds[1..]);
    }
    if let Some(m) = MONITOR_ANYWHERE.captures(s).and_then(|c| monitor_value(c.get(1)?.as_str())) {
        args["monitor"] = m;
    }
    if s.contains("on top") || s.contains("im vordergrund") {
        args["always_on_top"] = json!(true);
    }
    if let Some(c) = SIZE_PAIR.captures(s) {
        args["width"] = json!(c[1].parse::<u32>().ok()?);
        args["height"] = json!(c[2].parse::<u32>().ok()?);
    }
    Some(args)
}

/// A Jarvis window named in a phrase → the id the window tools resolve ("focus" = the one the
/// conversation is about).
fn jarvis_window_target(phrase: &str) -> Option<String> {
    let p = phrase.trim();
    if crate::focus::is_pronoun(p) || matches!(p, "that window" | "this window" | "the new window" | "the second window" | "the other window" | "the last window" | "your second window" | "your new window") {
        return Some("focus".into());
    }
    let name = p.trim_start_matches("the ").trim_start_matches("my ").trim_start_matches("your ");
    let name = name.strip_suffix(" window").or_else(|| name.strip_suffix(" fenster"))?.trim();
    // "The window" / "your window" mean the HUD.
    (!name.is_empty() && !matches!(name, "hud" | "jarvis" | "main")).then(|| name.to_string())
}

static WINDOW_RESIZE_TO: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:resize|make|set|change|scale)\s+(.+?)\s+(?:to\s+)?(\d{3,4})\s*(?:x|by|times|mal|×)\s*(\d{3,4})(?:\s*(?:pixels?|px))?$")
});
static WINDOW_RESIZE_REL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make|set|turn|mach|mache)\s+(.+?)\s+(?:a bit\s+|a little\s+|slightly\s+|much\s+|way\s+|etwas\s+|viel\s+)?(smaller|bigger|larger|kleiner|größer|groesser|half the size|twice as big|tiny|huge)$")
});
static WINDOW_STYLE_IT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make|set|turn|switch|mach|mache)\s+(?:it|that|this|es|das)\s+(borderless|frameless|transparent|see through|opaque|solid|always on top|stay on top|on top)$|^(?:pin|keep)\s+(?:it|that|this)(?:\s+(?:always\s+)?on top)?$")
});
static WINDOW_ADD_CONTENT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:add|put|include|also show|show|füge|füg)\s+(?:the\s+|my\s+|a\s+|die\s+|den\s+)?(.+?)\s+(?:to|in|into|on|zu|in)\s+(that|this|the|my|it|dem|das|diesem|diese[nm]?)?\s*(.*?)\s*(?:window|fenster)?(?:\s+(?:too|as well|also|hinzu|dazu|auch))?$")
});

/// "Make the drive space window smaller", "resize it to 800 by 500", "make it borderless", "pin
/// it always on top", "add CPU temperature to that window too".
fn jarvis_window_edit(s: &str) -> Option<Intent> {
    let tool = |name: &'static str, args: Value| Some(Intent::Tool { name, args });
    if let Some(c) = WINDOW_RESIZE_TO.captures(s) {
        let id = jarvis_window_target(&c[1])?;
        return tool("update_native_window", json!({ "id": id, "width": c[2].parse::<u32>().ok()?, "height": c[3].parse::<u32>().ok()? }));
    }
    if let Some(c) = WINDOW_RESIZE_REL.captures(s) {
        let id = jarvis_window_target(&c[1])?;
        let much = s.contains(" much ") || s.contains(" way ") || s.contains(" viel ");
        let size = match &c[2] {
            "smaller" | "kleiner" if much => "much smaller",
            "smaller" | "kleiner" => "smaller",
            "half the size" => "half",
            "twice as big" => "double",
            "tiny" => "much smaller",
            "huge" => "much bigger",
            _ if much => "much bigger",
            _ => "bigger",
        };
        return tool("update_native_window", json!({ "id": id, "size": size }));
    }
    if let Some(c) = WINDOW_STYLE_IT.captures(s) {
        let args = match c.get(1).map(|m| m.as_str()) {
            Some("borderless" | "frameless") => json!({ "borderless": true }),
            Some("transparent" | "see through") => json!({ "opacity": 0.6 }),
            Some("opaque" | "solid") => json!({ "opacity": 1.0 }),
            _ => json!({ "always_on_top": true }),
        };
        let mut args = args;
        args["target"] = json!("focus");
        return tool("customize_hud_and_windows", args);
    }
    if let Some(c) = WINDOW_ADD_CONTENT.captures(s) {
        let what = c.get(1)?.as_str().trim();
        let says_window = s.contains("window") || s.contains("fenster");
        let target = format!("{} {}", c.get(2).map(|m| m.as_str()).unwrap_or(""), c.get(3).map(|m| m.as_str()).unwrap_or("")).trim().to_string();
        let known = crate::widgets::gauge_source(what).is_some() || crate::widgets::BlockKind::live_from_name(what).is_some();
        if !says_window || !known {
            return None;
        }
        let id = if target.is_empty() || matches!(target.as_str(), "that" | "this" | "it" | "the" | "dem" | "das" | "diesem") {
            "focus".to_string()
        } else {
            jarvis_window_target(&format!("{target} window")).unwrap_or_else(|| "focus".into())
        };
        return tool("update_native_window", json!({ "id": id, "add_content": [what] }));
    }
    None
}

static MOVE_TO: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:move|put|throw|send|shift|drag|place|bring|verschieb|verschiebe|schieb|schiebe|pack|beweg|bewege|tu|tue)\s+(.+?)\s+(?:over\s+)?(?:to|onto|on|auf|nach|zu|zum|in)\s+(.+)$")
});
static SELF_TO_MONITOR: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:move|go|switch|jump|head)\s+(?:over\s+)?(?:to|onto)\s+(.+)$"));
static OPEN_ON: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:open|launch|start|starte|öffne|oeffne)\s+(.+?)\s+(?:on|auf)\s+(.+)$"));

/// "Move Sonora to monitor 2", "put Discord on my second screen", "move the PC stats window to
/// monitor 1", "move yourself to the left monitor", "open Spotify on my second monitor".
fn monitor_command(s: &str) -> Option<Intent> {
    if let Some(monitor) = SELF_TO_MONITOR.captures(s).and_then(|c| monitor_value(&c[1])) {
        return Some(Intent::Tool { name: "move_window_to_monitor", args: json!({ "app_or_window_name": "jarvis", "target_monitor": monitor }) });
    }
    if let Some(c) = MOVE_TO.captures(s) {
        let monitor = monitor_value(&c[2])?;
        let what = c[1].trim();
        let what = if crate::focus::is_pronoun(what) { "it".to_string() } else { what.trim_start_matches("the ").trim_start_matches("my ").to_string() };
        return Some(Intent::Tool { name: "move_window_to_monitor", args: json!({ "app_or_window_name": what, "target_monitor": monitor }) });
    }
    if let Some(c) = OPEN_ON.captures(s) {
        let monitor = monitor_value(&c[2])?;
        let app = strip_article(&c[1]);
        if looks_like_app_name(&app) {
            return Some(Intent::Tool { name: "place_application_window", args: json!({ "app_name": app, "monitor": monitor }) });
        }
    }
    None
}

static WHAT_PLAYING: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:what(?:s| is) (?:playing|this song|that song|the song|this track|on)(?: right now| now)?|what song is (?:this|playing|that)|which song is (?:this|playing)|who sings (?:this|that)|what am i listening to|was läuft(?: gerade)?|welches lied ist das|welcher song ist das|was ist das für ein lied|was höre ich gerade)$")
});
static PAUSE_MUSIC: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:pause|stop|halt)\s+(?:the\s+|my\s+)?(?:music|song|track|playback|spotify|sonora|playlist)$|^pause$|^(?:musik|lied|song)\s+(?:pausieren|stoppen|anhalten|aus)$|^(?:pausier|pausiere|stopp|stoppe)\s+(?:die\s+)?musik$|^musik pause$|^pausieren$|^pausiere$")
});
static RESUME_MUSIC: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:resume|continue|unpause)(?:\s+(?:the\s+)?(?:music|song|track|playback))?$|^play(?:\s+(?:some\s+|the\s+|my\s+)?music)?$|^play music$|^musik\s+(?:weiter|fortsetzen|an)$|^spiel weiter$|^fortsetzen$|^weiter$|^abspielen$|^play pause$")
});
/// Verbs that may start music when a playlist / song is named explicitly.
const PLAY_VERB: &str = r"(?:play|spiel|spiele|put on|start|starte|shuffle|queue up|throw on|leg)";
/// Verbs that mean music on their own ("start Discord" opens an app, even if a song is called so).
const PLAY_ONLY: &str = r"(?:play|spiel|spiele|put on|shuffle|queue up|throw on)";
static PLAY_ON_PLAYER: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^({PLAY_VERB}\s+.+?)\s+(?:on|in|auf|with|mit|über)\s+(spotify|sonora)$")));
static PLAY_PLAYLIST: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^{PLAY_VERB}\s+(?:me\s+|mir\s+)?(?:my\s+|the\s+|meine\s+|die\s+|meinen\s+)?(?:playlist|play list)\s+(?:called\s+|named\s+|namens\s+)?(.+)$|^{PLAY_VERB}\s+(?:me\s+|mir\s+)?(?:my\s+|the\s+|meine\s+|die\s+)?(.+?)\s*(?:playlist|play list)(?:\s+ab)?$"
    ))
});
static PLAY_SOMETHING: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^{PLAY_ONLY}\s+(?:the\s+song\s+|the\s+track\s+|das\s+lied\s+)?(.+)$")));

/// "Play Back in Black in JaysRock": the split (tried from the right, since titles contain
/// "in" too) whose second half is one of Sir's playlists.
fn song_in_playlist(s: &str) -> Option<(String, String)> {
    let body = PLAY_SOMETHING.captures(s)?.get(1)?.as_str();
    let mut splits: Vec<(usize, usize)> = [" in ", " from ", " aus ", " off ", " von "].iter().flat_map(|c| body.match_indices(c).map(|(i, m)| (i, i + m.len()))).collect();
    splits.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (start, end) in splits {
        let song = body[..start].trim();
        let mut list = body[end..].trim();
        for prefix in ["my ", "the ", "meiner ", "der ", "playlist "] {
            list = list.strip_prefix(prefix).unwrap_or(list);
        }
        let list = list.strip_suffix(" playlist").unwrap_or(list).trim();
        let list = list.strip_prefix("playlist ").unwrap_or(list).trim();
        if song.is_empty() || list.is_empty() || crate::sites::find(list).is_some() {
            continue;
        }
        if matches!(crate::music::quick_lookup(list), Some(crate::music::ItemKind::Playlist | crate::music::ItemKind::Album)) {
            return Some((song.to_string(), list.to_string()));
        }
    }
    None
}
static PLAY_BY: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^{PLAY_ONLY}\s+(?:the\s+song\s+|the\s+track\s+)?(.+?)\s+(?:by|from|von)\s+(.+)$")));
static PLAY_SONG: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^{PLAY_VERB}\s+(?:the\s+|my\s+|das\s+)?(?:song|track|lied|title)\s+(.+)$")));
static PLAY_ANY: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^{PLAY_ONLY}\s+(?:me\s+|mir\s+)?(?:my\s+|some\s+|the\s+|meine\s+)?(.+?)(?:\s+(?:for me|now|please|ab))?$")));

/// "Pick some good music", "play something", "surprise me": Jarvis chooses (executive initiative).
static PICK_MUSIC: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:pick|choose|select|find)\s+(?:me\s+)?(?:(?:some|good|nice|great|the right|something)\s+)*(?:music|songs?|tunes|playlist|something to listen to)(?:\s+for me)?$|^(?:play|put on)\s+(?:me\s+)?(?:something|something good|something nice|something i like|some music|some good music|good music|some songs|some tunes|anything|whatever)$|^surprise me(?:\s+with\s+(?:some\s+)?music)?$|^(?:spiel|leg)\s+(?:mir\s+)?(?:irgendwas|was gutes|etwas gutes|gute musik|irgendeine musik|musik)\s*(?:auf|ab)?$|^(?:such|wähl)\s+(?:mir\s+)?(?:gute\s+)?musik\s+aus$")
});

/// Music: playlists and songs from Sir's library, pause / resume / skip on the active player,
/// "what's playing".
fn music_command(s: &str) -> Option<Intent> {
    let music = |args: Value| Some(Intent::Tool { name: "control_music", args });
    if WHAT_PLAYING.is_match(s) {
        return music(json!({ "action": "what_is_playing" }));
    }
    if PAUSE_MUSIC.is_match(s) {
        return music(json!({ "action": "pause" }));
    }
    if RESUME_MUSIC.is_match(s) {
        return music(json!({ "action": if s == "play pause" { "toggle" } else { "resume" } }));
    }
    if NEXT.is_match(s) {
        return music(json!({ "action": "next_track" }));
    }
    if PREVIOUS.is_match(s) {
        return music(json!({ "action": "previous_track" }));
    }
    if PICK_MUSIC.is_match(s) {
        return music(json!({ "action": "pick_music" }));
    }
    // "Play X on Spotify / in Sonora": the player, then the rest as usual.
    if let Some(c) = PLAY_ON_PLAYER.captures(s) {
        let mut intent = music_command(&c[1]);
        if let Some(Intent::Tool { args, .. }) = intent.as_mut() {
            args["player_preference"] = json!(&c[2]);
        }
        return intent;
    }
    let not_a_site = |name: &str| crate::sites::find(name).is_none();
    // "Play Back in Black from my JaysRock playlist" (before the playlist rule would take it all
    // as one name); "play Minecraft on YouTube" is a website search.
    if let Some((song, list)) = song_in_playlist(s) {
        return music(json!({ "action": "play", "song_name": song, "playlist_name": list }));
    }
    if let Some(c) = PLAY_PLAYLIST.captures(s) {
        let name = capture(&c)?.trim().to_string();
        // "Play Back in Black from my JaysRock playlist": a song in the playlist, not a playlist
        // called "back in black from my jaysrock".
        for sep in [" from my ", " in my ", " off my ", " aus meiner ", " von meiner ", " in meiner "] {
            if let Some((song, list)) = name.split_once(sep) {
                let list = list.trim().trim_start_matches("playlist ").trim();
                if !song.trim().is_empty() && !list.is_empty() {
                    return music(json!({ "action": "play", "song_name": song.trim(), "playlist_name": list }));
                }
            }
        }
        return music(json!({ "action": "play", "playlist_name": name }));
    }
    if let Some(c) = PLAY_BY.captures(s) {
        let (what, artist) = (c[1].trim(), c[2].trim());
        // "Play Thunderstruck from my playlist JaysRock" / "… from my JaysRock playlist": a song in a
        // playlist even before the library index knows the name, never an artist called "my …".
        let owned = ["my ", "meiner ", "meinem ", "the playlist ", "playlist "].iter().find_map(|p| artist.strip_prefix(p));
        if let Some(rest) = owned.filter(|_| !what.is_empty()) {
            let list = rest.trim_start_matches("playlist ").trim_start_matches("play list ").trim_end_matches(" playlist").trim_end_matches(" play list").trim();
            if !list.is_empty() && list != "playlist" {
                return music(json!({ "action": "play", "song_name": what, "playlist_name": list }));
            }
        }
        if not_a_site(artist) {
            return if matches!(what, "something" | "some music" | "music" | "songs" | "some songs" | "anything" | "etwas" | "was") {
                music(json!({ "action": "play", "artist_name": artist }))
            } else {
                music(json!({ "action": "play", "song_name": what, "artist_name": artist }))
            };
        }
    }
    if let Some(c) = PLAY_SONG.captures(s) {
        return music(json!({ "action": "play", "song_name": c[1].trim() }));
    }
    // "Play JaysRock", "play Back in Black": only names in Sir's library (never "play Minecraft").
    if let Some(c) = PLAY_ANY.captures(s) {
        let name = c[1].trim();
        let field = match crate::music::quick_lookup(name)? {
            crate::music::ItemKind::Track => "song_name",
            crate::music::ItemKind::Artist => "artist_name",
            crate::music::ItemKind::Album => "album_name",
            _ => "playlist_name",
        };
        return music(json!({ "action": "play", field: name }));
    }
    None
}

static CAPTURE_WORKSPACE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:save|store|capture|remember|record|snapshot|speicher|speichere|sichere)\s+(?:my\s+|the\s+|this\s+|meinen\s+|mein\s+|das\s+|dieses\s+|diesen\s+|den\s+)?(?:current\s+|whole\s+|aktuelle[ns]?\s+)?(?:setup|workspace|layout|desktop|arrangement|window layout|screen layout|windows|arbeitsplatz|arbeitsbereich|anordnung|fensterlayout)\s+(?:as|als|under|unter)\s+(?:the\s+|a\s+|my\s+)?(.+?)$")
});

/// "Save my current setup as Gaming Protocol" → capture the workspace.
fn protocol_command(s: &str) -> Option<Intent> {
    let c = CAPTURE_WORKSPACE.captures(s)?;
    let name = c[1].trim().trim_end_matches(" protocol").trim_end_matches(" protokoll").trim().to_string();
    (!name.is_empty()).then(|| Intent::Tool { name: "manage_stark_protocol", args: json!({ "action": "capture_current_workspace", "protocol_name": name }) })
}

static AFFIRM: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:yes|yeah|yep|yup|yes sir|sure|sure thing|ok|okay|alright|do it|do that|do so|go ahead|go for it|please do|sounds good|make it so|proceed|absolutely|of course|affirmative|why not|lets do it|let s do it|that would be great|that would be nice|ja|jawohl|klar|gerne|gern|mach das|mach es|mach ruhig|los|auf jeden fall|genau|gerne doch|ja gerne|open it|open the window|show me|yes open it|do that for me)(?:\s+(?:sir|jarvis|then|now|bitte|please|thanks|thank you|danke))?$")
});
static NEGATE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:no|nope|nah|no thanks|no thank you|not now|no need|thats fine|that s fine|its fine|it s fine|never mind|leave it|nein|nee|nö|lieber nicht|nicht jetzt|nein danke|lass mal|passt schon)$")
});
static TERMINATE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:terminate|kill|end|close|stop|quit|beende|beend|schließ|schließe|schliess|schliesse)\s+(?:it|them|that|this|those|the process|the program|the app|it all|background tasks|the background tasks|all background tasks|es|sie|das|den prozess|die hintergrundprozesse|die hintergrundaufgaben)$|^(?:terminate|kill)(?:\s+background tasks)?$")
});

/// "Yes", "yeah, do that please", "go ahead", "mach das": the answer to an offer.
pub fn is_affirmation(text: &str) -> bool {
    let s = normalize(text);
    !s.is_empty() && AFFIRM.is_match(&s)
}

/// "No thanks", "nein": the offer is declined.
pub fn is_negation(text: &str) -> bool {
    // Raw words: `normalize` would read "no thanks" as "thanks".
    let s = raw_words(text);
    let s = s.trim_start_matches("jarvis ").trim_end_matches(" jarvis").trim_end_matches(" sir");
    !s.is_empty() && NEGATE.is_match(s)
}

/// "Terminate it", "kill background tasks": the Guardian's second offer.
pub fn is_termination(text: &str) -> bool {
    TERMINATE.is_match(&normalize(text))
}

/// An action a reply of Jarvis' states or offers.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplyAction {
    /// "Shall I …?" (true) or "I'm creating …" / "Opening …" (false).
    pub offer: bool,
    pub name: &'static str,
    pub args: Value,
}

/// First-person openings of a claim or an offer, longest first.
const CLAIM_OPENERS: &[(&str, bool)] = &[
    ("would you like me to ", true),
    ("do you want me to ", true),
    ("shall i also ", true),
    ("should i also ", true),
    ("want me to ", true),
    ("shall i ", true),
    ("should i ", true),
    ("may i ", true),
    ("i can also ", true),
    ("i could ", true),
    ("i can ", true),
    ("i'm now ", false),
    ("im now ", false),
    ("i am now ", false),
    ("i'm ", false),
    ("im ", false),
    ("i am ", false),
    ("i've ", false),
    ("ive ", false),
    ("i have ", false),
    ("i'll ", false),
    ("ill ", false),
    ("i will ", false),
    ("let me ", false),
    ("now ", false),
];

/// Progressive / past forms back to the command form ("creating" → "create").
fn imperative(clause: &str) -> String {
    const FORMS: &[(&str, &str)] = &[
        ("creating", "create"), ("created", "create"), ("opening", "open"), ("opened", "open"), ("spawning", "open"), ("spawned", "open"),
        ("moving", "move"), ("moved", "move"), ("putting", "put"), ("playing", "play"), ("starting", "start"), ("started", "start"),
        ("showing", "show"), ("displaying", "show"), ("displayed", "show"), ("launching", "launch"), ("launched", "launch"),
        ("bringing up", "open"), ("brought up", "open"), ("pulling up", "open"), ("pulled up", "open"), ("popping up", "open"),
        ("initiating", "initiate"), ("initiated", "initiate"), ("engaging", "engage"), ("engaged", "engage"), ("activating", "activate"),
        ("pausing", "pause"), ("paused", "pause"), ("resuming", "resume"), ("resumed", "resume"), ("skipping to", "skip to"),
        ("relocating", "move"), ("relocated", "move"), ("sending", "send"), ("sent", "send"), ("queuing up", "play"), ("queueing up", "play"),
        ("setting up", "create"), ("set up", "create"), ("making", "make"), ("made", "make"), ("saving", "save"), ("saved", "save"),
        ("capturing", "capture"), ("captured", "capture"),
    ];
    let c = clause.trim();
    for (from, to) in FORMS {
        if let Some(rest) = c.strip_prefix(from) {
            if rest.is_empty() || rest.starts_with(' ') {
                return format!("{to}{rest}");
            }
        }
    }
    c.to_string()
}

/// The actions a reply announces ("Very good, Sir. I'm creating a new window showing your PC
/// stats.") or offers ("Shall I open a live telemetry window?"), as the fast path would run them.
pub fn reply_actions(reply: &str) -> Vec<ReplyAction> {
    let lower = reply.to_lowercase().replace(['’', '`'], "'");
    let mut out = Vec::new();
    for raw in lower.split(['.', '!', '?', ';', '\n']) {
        let mut clause = raw.trim().trim_start_matches(['-', '–', '—', ',', ' ']).to_string();
        // "Very good, Sir, opening …": drop the courtesy before the verb.
        for courtesy in ["very good, sir, ", "right away, sir, ", "of course, sir, ", "certainly, sir, ", "sir, ", "of course, ", "certainly, ", "right away, ", "very well, ", "very good, ", "absolutely, ", "sure, "] {
            if let Some(rest) = clause.strip_prefix(courtesy) {
                clause = rest.to_string();
            }
        }
        let mut offer = None;
        for (opener, is_offer) in CLAIM_OPENERS {
            if let Some(rest) = clause.strip_prefix(opener) {
                offer = Some(*is_offer);
                clause = rest.trim().trim_start_matches("go ahead and ").trim_start_matches("just ").to_string();
                break;
            }
        }
        let command = imperative(&clause);
        // A claim must start with the action itself ("Opening …"); an offer with its opener.
        let starts_with_action = command != clause || offer.is_some();
        if !starts_with_action || command.split_whitespace().count() < 2 {
            continue;
        }
        let command = command.trim_end_matches(" for you").trim_end_matches(" now").trim_end_matches(" right away").trim_end_matches(", sir").trim_end_matches(" sir").to_string();
        if let Some(Intent::Tool { name, args }) = parse(&command).or_else(|| resolve_fuzzy(&command)) {
            // Quitting is never inferred from a reply.
            if !matches!(name, "shutdown_jarvis" | "control_jarvis_lifecycle") {
                out.push(ReplyAction { offer: offer.unwrap_or(false), name, args });
            }
        }
    }
    out
}

/// Sir's request asked for an action (not "how do I …").
fn requested_action(request: &str) -> bool {
    static ASKED: LazyLock<Regex> = LazyLock::new(|| {
        re(r"\b(?:open|create|make|show|play|move|put|start|launch|spawn|display|give|bring|close|pause|resume|skip|initiate|engage|run|save|capture|can you|could you|would you|will you|please|i want|i need|id like|i d like|bitte|mach|zeig|öffne|oeffne|spiel|verschieb|starte|erstelle|kannst du|könntest du)\b")
    });
    let s = raw_words(request);
    !s.starts_with("how ") && !s.starts_with("wie ") && ASKED.is_match(&s)
}

/// The offer a reply ends with ("Shall I open the drive space window?"), for Sir's "yes".
pub fn offered_action(reply: &str) -> Option<(&'static str, Value)> {
    reply_actions(reply).into_iter().find(|a| a.offer).map(|a| (a.name, placed_by_sir(a.args, "")))
}

/// Only Sir's own words decide whether a view gets its own OS window ("a new window"), never
/// the model's phrasing of its reply.
fn placed_by_sir(mut args: Value, request: &str) -> Value {
    if let Some(o) = args.as_object_mut() {
        o.remove("separate_window");
        if SEPARATE_WINDOW.is_match(&normalize(request)) {
            o.insert("separate_window".into(), json!(true));
        }
    }
    args
}

/// Compiles every pattern up front (first use otherwise costs ~0.5 s on the first command).
pub fn warm_up() {
    crate::music::warm_up();
    let _ = parse("warm up the parser");
    let _ = crate::typos::normalize_user_input_typos("warm up your core");
    let _ = crate::intent_engine::deterministic_action("warm up the windows");
    let _ = resolve_fuzzy("warm up the fuzzy parser and open it");
    let _ = looks_incomplete("warm up and");
    let _ = live_info_query("warm up the weather news");
    let _ = is_memory_command("warm up memory");
    let _ = upfront_tool("warm up the selected text on monitor 2");
    let _ = reply_actions("I'm warming up. Shall I open a window?");
    let _ = is_affirmation("warm up");
}

pub fn parse(text: &str) -> Option<Intent> {
    let s = normalize(text);
    if s.is_empty() {
        return None;
    }
    let tool = |name: &'static str, args: Value| Some(Intent::Tool { name, args });
    let media = |action: &str| json!({ "action": action });

    if STOP.is_match(&s) {
        return Some(Intent::Stop);
    }
    // The HUD's own views ("close your core", "bring back the screen", "dock all windows").
    if let Some(intent) = hud_view_command(text) {
        return Some(intent);
    }
    if let Some(intent) = outing_command(text) {
        return Some(intent);
    }
    // The HUD board first: "sign off" puts the screen down (it no longer quits Jarvis), and with
    // a board up "maximise that" means a block, not a window.
    if let Some(intent) = board_command(text, crate::hud_board::is_active(), crate::hud_board::spotlight_active()) {
        return Some(intent);
    }
    // Before the close/open rules, which would read "close ur self" or "kill process" as apps.
    if SHUTDOWN.is_match(&s) || is_self_shutdown(&s) {
        // A bare "close" / "quit" (Jarvis not named) only sends the HUD to the tray.
        if BARE_QUIT.is_match(&s) && !names_jarvis(text) {
            return tool("configure_interface", json!({ "window": "hide" }));
        }
        return tool("shutdown_jarvis", json!({}));
    }
    // The HUD's own modes and the volume mixer (before the volume rules and "open/close X").
    if let Some(args) = orb_mode_command(text) {
        return tool("control_orb_mode", args);
    }
    if let Some(args) = listening_mode_command(text) {
        return tool("set_listening_mode", args);
    }
    if let Some(args) = mixer_command(text) {
        return tool("control_audio_mixer", args);
    }
    if TIME.is_match(&s) {
        return Some(Intent::Time);
    }
    if DATE.is_match(&s) {
        return Some(Intent::Date);
    }
    if STATUS.is_match(&s) {
        return tool("get_system_status", json!({}));
    }
    for re in [&*VOLUME_SET, &*VOLUME_SET_REV, &*VOLUME_SET_PC] {
        if let Some(c) = re.captures(&s) {
            let level: i64 = c[1].parse().ok()?;
            return tool("media_and_volume_control", json!({ "action": "set_volume", "level": level.min(100) }));
        }
    }
    if VOLUME_UP.is_match(&s) {
        return tool("media_and_volume_control", media("volume_up"));
    }
    if VOLUME_DOWN.is_match(&s) {
        return tool("media_and_volume_control", media("volume_down"));
    }
    if MUTE.is_match(&s) {
        return tool("media_and_volume_control", media("mute"));
    }
    if UNMUTE.is_match(&s) {
        return tool("media_and_volume_control", media("unmute"));
    }
    // Music: the player's own media session (pause really pauses), playlists and songs.
    if let Some(intent) = music_command(&s) {
        return Some(intent);
    }
    if PLAY_PAUSE.is_match(&s) {
        return tool("media_and_volume_control", media("play_pause"));
    }
    if VOICE_LOUDER.is_match(&s) {
        return tool("media_and_volume_control", media("jarvis_louder"));
    }
    if VOICE_QUIETER.is_match(&s) {
        return tool("media_and_volume_control", media("jarvis_quieter"));
    }
    if let Some(intent) = lifecycle_or_window_command(&s) {
        return Some(intent);
    }
    // "Create a new window that shows my PC stats / the space left on my drives."
    if let Some(mut args) = live_window_request(&s) {
        // "A new window" said out loud: its own OS window (otherwise a block inside the HUD).
        if SEPARATE_WINDOW.is_match(&s) {
            args["separate_window"] = json!(true);
        }
        return tool("spawn_native_window", args);
    }
    // "Make the drive space window smaller", "add CPU temperature to that window".
    if let Some(intent) = jarvis_window_edit(&s) {
        return Some(intent);
    }
    // "Move Sonora to monitor 2", "open Spotify on my second screen".
    if let Some(intent) = monitor_command(&s) {
        return Some(intent);
    }
    // "Save my current setup as Gaming Protocol".
    if let Some(intent) = protocol_command(&s) {
        return Some(intent);
    }
    // "Maximize Jarvis", "fullscreen yourself": Jarvis only when Sir names him.
    if let Some(args) = self_window_command(text) {
        return tool("configure_interface", args);
    }
    // "Full screen", "close it", "move it to the left": the app the conversation is about.
    if let Some(intent) = focus_command(&s) {
        return Some(intent);
    }
    if let Some(intent) = file_followup(&s) {
        return Some(intent);
    }
    if let Some(intent) = window_style_command(&s) {
        return Some(intent);
    }
    // Websites, channels and platform searches (before "open", which would treat them as apps).
    if let Some(intent) = website_command(text) {
        return Some(intent);
    }
    // Reminders, timers, protocols, files, maths … (before "open"/"close", which would read
    // "open budget.xlsx" or "start a timer" as applications).
    if let Some(intent) = assistant_command(&s, text) {
        return Some(intent);
    }
    // "Reboot your language model": a hard reset of the engine (before "restart" reads as an app).
    if LLM_REBOOT.is_match(&s) {
        return tool("reboot_language_model", json!({}));
    }
    // Before the window/app rules, which would read "minimize yourself" or "close the chat" as apps.
    if let Some(args) = interface_command(&s) {
        return tool("configure_interface", args);
    }
    if MINIMIZE_ALL.is_match(&s) {
        return tool("window_management", json!({ "action": "minimize_all" }));
    }
    if let Some(c) = SNAP.captures(&s) {
        let side = if matches!(&c[2], "left" | "links") { "snap_left" } else { "snap_right" };
        return tool("window_management", json!({ "action": side, "target_window": strip_article(&c[1]) }));
    }
    if let Some(c) = WINDOW_ACTION.captures(&s) {
        let action = match &c[1] {
            a if a.starts_with("minimi") => "minimize",
            a if a.starts_with("maximi") => "maximize",
            "restore" => "restore",
            _ => "focus",
        };
        let target = strip_article(&c[2]);
        if looks_like_app_name(&target) {
            return tool("window_management", json!({ "action": action, "target_window": target }));
        }
    }
    if let Some(c) = WINDOW_ACTION_DE.captures(&s) {
        let action = match &c[2] {
            "minimieren" => "minimize",
            "maximieren" => "maximize",
            _ => "restore",
        };
        let target = strip_article(&c[1]);
        if looks_like_app_name(&target) {
            return tool("window_management", json!({ "action": action, "target_window": target }));
        }
    }
    for re in [&*CLOSE, &*CLOSE_DE, &*CLOSE_DE_SUFFIX] {
        if let Some(c) = re.captures(&s) {
            let app = strip_article(&c[1]);
            if looks_like_app_name(&app) {
                return tool("close_application", json!({ "app_name": app, "force": true }));
            }
        }
    }
    // Jarvis' own views are never programs ("open my system stats", "open message from stats").
    if let Some(args) = internal_view_command(text) {
        return tool("open_jarvis_view", args);
    }
    for re in [&*OPEN, &*OPEN_DE, &*OPEN_DE_SUFFIX] {
        if let Some(c) = re.captures(&s) {
            let app = strip_article(&c[1]);
            if looks_like_app_name(&app) {
                return tool("open_application", json!({ "app_name": app }));
            }
        }
    }
    None
}

// ---- The HUD board ---------------------------------------------------------------------------
// "Hey Jarvis, are you there?", "can you do a state of VR report for me?", "yes please, present
// the report", "bring up that VR report you gave me earlier", "maximise the chart", "back to the
// board", "show me Tulleys Farm on the map", "thanks Jarvis, you can go to sleep again".

static BOARD_WAKE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:are you (?:there|awake|up|still there|with me)|you there|you awake|bist du (?:da|wach|noch da))(?:\s+jarvis)?$")
});
/// The screen goes down (and the GPU is emptied afterwards). "Clear the board" is not a sign-off:
/// it only takes the blocks away (`intent_engine::close_command`).
static BOARD_SIGN_OFF: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:you can|you may|feel free to)\s+go (?:back )?to sleep(?: again)?\b|^(?:sign off|signing off|screen down|board off|du kannst (?:wieder )?schlafen gehen|bildschirm aus)$")
});
static BOARD_PRESENT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:yes|yeah|yep|sure|ja)\s+)?(?:go ahead\s+(?:and\s+)?)?(?:present|show|put up|bring up|pull up|display|put)\s+(?:me\s+)?(?:the|that|your|it)?\s*(?:report|it)(?:\s+(?:up|on (?:the )?(?:screen|board|hud)))?(?:\s+and walk me through it)?$|^walk me through (?:it|the report)$|^(?:zeig|zeige)\s+(?:mir\s+)?(?:den|deinen)?\s*bericht$")
});
static BOARD_WALK: LazyLock<Regex> = LazyLock::new(|| re(r"\bwalk\s+(?:me|us)\s+through\s+(?:it|that|this|the report|the board|the blocks)\b|\bgo\s+through\s+(?:it|the report|the board)\s+with\s+me\b|^(?:führ|fuehr)\s+mich\s+durch"));
static BOARD_RECALL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:bring up|pull up|show me|show|open|put up|get)\s+(?:that|the|my|your)\s+(.+?)\s+report(?:\s+(?:again|from (?:earlier|yesterday|before)|(?:that\s+)?you\s+(?:gave|made|did|showed)\s+(?:me\s+)?(?:earlier|before|yesterday)?))*$")
});
static BOARD_RESEARCH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:do|make|run|put together|prepare|write|build|give)\s+(?:me\s+)?(?:a|an)\s+(.+?)\s+report$|^(?:research|look into)\s+(.+)$|^(?:mach|erstelle)\s+(?:mir\s+)?(?:einen|ein)\s+(.+?)[\s-]*(?:bericht|report)$")
});
static BOARD_MAXIMIZE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:maximi[sz]e|expand|enlarge|blow up|zoom in on|spotlight|focus on|maximieren|vergrößere|vergroessere)(?:\s+(?:that|this|it|the|den|die|das))?(?:\s+(.*?))?$")
});
static BOARD_MINIMIZE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:minimi[sz]e(?: (?:it|that|this))?|back to (?:the )?board|restore (?:the )?board|shrink (?:it|that)|close the spotlight|minimieren|zurück zum board)$")
});
/// Turning the pages of a stacked block: "next page", "show me the next result", "previous card",
/// "go back", "page 3", "show result 3", "the second result", "nächste Seite", "Seite zwei".
static BOARD_FLIP: LazyLock<Regex> = LazyLock::new(|| {
    const GOTO: &str = r"(?:(?:show|give)\s+(?:me\s+)?|go to\s+|go back to\s+|open\s+|flip to\s+|turn to\s+|jump to\s+)?(?:the\s+)?";
    // "The second one" may answer another question; a page is named by what it is.
    const NAMED: &str = r"(?:notes?|page|slide|result|card|seite|karte|notiz|ergebnis)";
    const THING: &str = r"(?:notes?|page|slide|result|card|one|seite|karte|notiz|ergebnis)";
    re(&format!(
        r"^(?:{GOTO}(?:next|following|nächste|naechste)\s+{THING}|(?:flip|turn)\s+(?:the\s+)?{NAMED}|flip (?:it|that|the stack)|go forward|weiter|(?P<back>{GOTO}(?:previous|last|prior|vorherige|letzte)\s+{THING}|go back|zurück|zurueck)|{GOTO}{NAMED}\s+(?:number\s+|nummer\s+)?(?P<page>\d{{1,2}}|one|two|three|four|five|six|seven|eight|nine|ten|eins|zwei|drei|vier|fünf|sechs)|{GOTO}(?P<nth>first|second|third|fourth|fifth|sixth|seventh|eighth|erste|zweite|dritte|vierte|fünfte|sechste)\s+{NAMED})$"
    ))
});
static BOARD_MAP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:show me|show|put|bring up|pull up|where is|wo ist|zeig mir|zeige mir)\s+(.+?)\s+(?:on|auf)\s+(?:the|a|der|einer)\s+(?:map|karte)$")
});
/// "for three hours", "for 90 minutes": a multi-hour Deep Research mission, not a board report.
static RESEARCH_DURATION: LazyLock<Regex> = LazyLock::new(|| re(r"\bfor\s+(?:\d+|a|an|one|two|three|four|five|six|ten|twelve)\s+(?:hours?|minutes?|mins?)\b"));
/// Words that name a board block ("the chart", "photos").
const BOARD_NOUNS: [&str; 16] = ["chart", "graph", "map", "photo", "photos", "picture", "image", "images", "gallery", "note", "notes", "figure", "stat", "video", "block", "report"];

/// The HUD board's voice commands (`active`: a board is on screen, `spotlight`: a block is
/// maximised). Maximise, minimise and flipping notes are silent (a badge and the UI tone).
pub fn board_command(text: &str, active: bool, spotlight: bool) -> Option<Intent> {
    let s = normalize(text);
    if s.is_empty() {
        return None;
    }
    let tool = |args: Value| Some(Intent::Tool { name: "hud_board", args });
    let silent = |args: Value, label: &str| Some(Intent::Silent(SilentAction { tool: "hud_board", args, label: label.to_string() }));
    if BOARD_WAKE.is_match(&s) {
        return tool(json!({ "action": "wake" }));
    }
    if BOARD_SIGN_OFF.is_match(&s) && !NEGATION.is_match(&s) {
        return tool(json!({ "action": "sign_off" }));
    }
    // "Close the HUD board", "close to web search", "close the feature lab", "clear the board":
    // Jarvis' own blocks, never a program for Windows to look up.
    if let Some(action) = crate::intent_engine::close_command(text) {
        return Some(Intent::Silent(action));
    }
    // "Walk me through it" (also "yes, put it up and walk me through it"): narrated block by block.
    if BOARD_WALK.is_match(&s) {
        return tool(json!({ "action": "walkthrough" }));
    }
    if BOARD_PRESENT.is_match(&s) {
        return tool(json!({ "action": "present" }));
    }
    if let Some(c) = BOARD_MAP.captures(&s) {
        return tool(json!({ "action": "show_map", "place": original_case(text, c[1].trim()) }));
    }
    if let Some(c) = BOARD_RECALL.captures(&s) {
        return tool(json!({ "action": "recall", "topic": c[1].trim() }));
    }
    if let Some(c) = BOARD_RESEARCH.captures(&s) {
        let topic = c.get(1).or(c.get(2)).or(c.get(3))?.as_str().trim();
        let workspace = [" and ", "window", "workspace", "dossier"].iter().any(|w| s.contains(w));
        if !RESEARCH_DURATION.is_match(&s) && !workspace && !topic.is_empty() && topic.split_whitespace().count() <= 8 {
            return tool(json!({ "action": "research", "topic": original_case(text, topic) }));
        }
    }
    if active {
        if spotlight && BOARD_MINIMIZE.is_match(&s) {
            return silent(json!({ "action": "minimize" }), "Back to the board");
        }
        if let Some(c) = BOARD_MAXIMIZE.captures(&s) {
            let target = c.get(1).map(|m| m.as_str().trim().trim_start_matches("the ").to_string()).unwrap_or_default();
            let names_block = target.split_whitespace().any(|w| BOARD_NOUNS.contains(&w)) || crate::hud_board::names_block(&target);
            if target.is_empty() || names_block {
                return silent(json!({ "action": "maximize", "target": target }), "Maximised");
            }
        }
        if let Some(c) = BOARD_FLIP.captures(&s) {
            if let Some(page) = c.name("page").or(c.name("nth")) {
                let n = match page.as_str() {
                    "one" | "eins" | "first" | "erste" => 1,
                    "two" | "zwei" | "second" | "zweite" => 2,
                    "three" | "drei" | "third" | "dritte" => 3,
                    "four" | "vier" | "fourth" | "vierte" => 4,
                    "five" | "fünf" | "fifth" | "fünfte" => 5,
                    "six" | "sechs" | "sixth" | "sechste" => 6,
                    "seven" | "seventh" => 7,
                    "eight" | "eighth" => 8,
                    "nine" => 9,
                    "ten" => 10,
                    d => d.parse().unwrap_or(1),
                };
                return silent(json!({ "action": "flip_note", "page": n }), "PAGE");
            }
            let direction = if c.name("back").is_some() { "previous" } else { "next" };
            return silent(json!({ "action": "flip_note", "direction": direction }), "PAGE");
        }
    }
    None
}

static LIVE_INFO: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(weather|wetter|forecast|vorhersage|temperature outside|regnet es|rain today|news|nachrichten|schlagzeilen|headlines|neuigkeiten|stock price|aktienkurs|aktie von|share price|bitcoin|ethereum|crypto|kurs von|exchange rate|wechselkurs|who won|wer hat gewonnen|score of|spielstand|ergebnis des spiels|bundesliga|champions league|premier league|election results|wahlergebnis)\b")
});

/// Questions whose answer depends on live data (weather, news, prices, scores). These always
/// get a web search first so a small LLM can't answer from stale or invented knowledge.
pub fn live_info_query(text: &str) -> Option<String> {
    let s = normalize(text);
    if s.split_whitespace().count() < 2 || !LIVE_INFO.is_match(&s) {
        return None;
    }
    Some(text.trim().trim_end_matches(['?', '.', '!']).trim().to_string())
}

// ---- Semantic & phonetic fallback ----------------------------------------------------------
// `parse` only takes whole, unambiguous commands. `resolve_fuzzy` runs when it found nothing and
// catches the natural, indirect or misheard forms of a few high-value actions anywhere in the
// sentence ("can you please close yourself?", "would you mind opening Spotify", "help my
// browser" for "open my browser"), so Jarvis acts instead of only answering with words.

/// Jarvis himself as the object of a quit verb, including Whisper's spellings. ("Close your
/// window" hides the HUD instead.)
const SELF_REF: &str = r"(?:ur ?self|your ?selfe?|urself|yo ?self|yer ?self|jarvis|your (?:app|application|program|process|self))";

static SELF_SHUTDOWN_ANY: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"\b(?:close|quit|exit|kill|terminate|end)\s+{SELF_REF}\b|\b(?:shut|turn|switch|power)\s+(?:(?:down|off)\s+{SELF_REF}|{SELF_REF}\s+(?:down|off))\b|\b(?:mach|mache)\s+(?:dich|jarvis)\s+(?:aus|zu|ab)\b|\b(?:schalt|schalte)\s+(?:dich|jarvis)\s+(?:aus|ab)\b|\b(?:fahr|fahre)\s+(?:dich|jarvis)\s+(?:selbst\s+)?(?:runter|herunter)\b|\b(?:beende|beend|schließe|schliesse|schließ)\s+(?:dich|jarvis)\b"
    ))
});
/// Whole-sentence forms Sir uses for "quit yourself".
static SELF_SHUTDOWN_EXACT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:go away|kill process|kill the process|turn off|switch off|power off|shut off|get lost|geh weg|mach aus|mach zu|feierabend|schluss für heute|bye bye jarvis|goodbye jarvis)$")
});
static NEGATION: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:dont|do not|don t|never|nicht|nie|kein|keine)\b"));
static SLEEP_MODE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:go to sleep|sleep mode|sleep|take a nap|nap time|standby|stand by|go on standby|schlaf|schlafen|geh schlafen|schlafmodus|ruhemodus|standby modus|bereitschaft)$")
});
static VOICE_MUTE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:mute yourself|mute your voice|mute tts|turn off your voice|stummschalten dich|schalte deine stimme aus|deine stimme aus)$")
});
static VOICE_UNMUTE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:unmute yourself|unmute your voice|unmute tts|turn your voice (?:back )?on|you can talk again|you may speak again|deine stimme (?:wieder )?an|du darfst wieder sprechen)$")
});
static RESTART: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(?:restart|reboot|reload)(?:\s+{SELF_REF})?$|^(?:starte?|start)\s+(?:dich|jarvis)\s+neu$|^neustart$"))
});
/// "Open my browser", "launch web", "starte den Browser" anywhere in a sentence.
static OPEN_BROWSER_ANY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:open|launch|start|fire up|bring up|pull up|öffne|oeffne|starte)\s+(?:up\s+)?(?:me\s+|mir\s+)?(?:(?:my|the|a|your|den|meinen|mein|einen|ein)\s+)?(?:default\s+|standard\s*|web\s*|internet\s*)?(?:browser|webbrowser|internet|web)\b")
});
static BROWSER_SEARCH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\bbrowser\b.*?\b(?:and\s+|und\s+)?(?:search|google|look up|suche|such)\s+(?:for\s+|nach\s+)?(.+)$")
});
/// "Would you mind opening Spotify for me" (only for known apps).
static OPEN_ANY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:open|opening|launch|launching|start|starting|fire up|öffne|oeffne|starte)\s+(?:up\s+)?(?:(?:my|the|den|die|das|mein|meinen)\s+)?([a-z0-9äöüß][a-z0-9äöüß .\-]{1,30}?)(?:\s+(?:for me|please|bitte|now|jetzt|app|application|for a second))?(?:\s+and\b|\s+und\b|$)")
});
/// Whisper's usual mishearings of a sentence-initial "open".
const OPEN_SOUNDALIKES: &[&str] = &["help", "hope", "opel", "oben", "upon", "oven", "opens", "opened", "ope", "obelix", "offen"];
static WINDOW_FROM_REPLY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:put|show|display|open|move|place|throw|pop)\s+(?:that|this|it|the)(?:\s+(?:answer|result|results|information|info|text|list|data))?(?:\s+out)?\s+(?:in|into|on|to)\s+(?:a|an|the|its)?\s*(?:second|new|separate|another|extra|own|different)?\s*(?:jarvis\s+)?window\b|\b(?:zeig|zeige|pack|leg|mach|öffne|oeffne)\s+(?:das|es|dies|diese infos?)\s+(?:in|im)\s+(?:einem|ein|einen|eigenen|neuen|zweiten|separaten|extra)?\s*(?:neuen|zweiten|separaten|eigenen|extra)?\s*fenster\b")
});
static NEW_WINDOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:open|spawn|create|make|give me)\s+(?:a|an|another|one more)\s+(?:new\s+|second\s+|separate\s+|extra\s+)?(?:jarvis\s+|native\s+)?window$|^(?:öffne|oeffne|mach|erstelle)\s+(?:ein|ein neues|ein zweites|noch ein)\s+(?:neues\s+)?fenster(?:\s+auf)?$|^neues fenster$")
});
static WIDGET_FROM_REPLY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:put|turn|make|save|pin|add|store)\s+(?:that|this|it|the)(?:\s+(?:answer|result|results|information|info|text|list|data))?\s+(?:in|into|as|to|on)\s+(?:a|an|the|my)?\s*(?:new\s+)?(?:widget|note card|note|card)\b|\b(?:pack|mach|speicher|speichere|leg)\w*\s+(?:das|es|dies)\s+(?:in|als)\s+(?:ein|einen|eine)?\s*(?:neues\s+)?(?:widget|notiz)\b")
});
/// "Create a live CPU gauge widget", "add a GPU temperature chart".
static LIVE_WIDGET: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:create|make|add|show|give me|build|put up|erstelle|mach|zeig)\s+(?:me\s+|mir\s+)?(?:a|an|ein|einen|eine)?\s*(?:live\s+|new\s+|neues\s+|neuen\s+)?(gpu temperature|gpu temp|cpu|processor|prozessor|ram|memory|arbeitsspeicher|gpu|graphics card|grafikkarte|vram|disk|drive|festplatte)\s*(?:load|usage|auslastung|temperature|temperatur)?\s*(gauge|meter|anzeige|chart|graph|diagramm|sparkline|monitor|widget)s?\b")
});
/// "Show my system status in a new window".
static TELEMETRY_WINDOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:show|put|display|open|give me|zeig|zeige)\s+(?:me\s+|mir\s+)?(?:my\s+|the\s+|meinen\s+|den\s+)?(?:system status|system stats|telemetry|system monitor|performance|hardware status|system info|systemstatus)\s+(?:in|into|on|im)\s+(?:a|an|the|einem)?\s*(?:new|second|separate|own|extra|another|neuen|eigenen|separaten)?\s*(?:window|fenster)\b")
});
static CLOSE_JARVIS_WINDOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:close|shut)\s+(?:that|this|the|the jarvis|the new|the second|the separate|the extra|the last|the other)\s+(?:jarvis\s+)?window$|^(?:schließ|schließe|schliess|schliesse|mach)\s+(?:das|dieses|dein)\s+(?:neue\s+|zweite\s+)?fenster(?:\s+zu)?$")
});
static CLOSE_ALL_JARVIS_WINDOWS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:close|shut)\s+all\s+(?:of\s+)?(?:your|the|jarvis|the jarvis|extra|secondary)?\s*windows$|^(?:schließ|schließe|schliess|schliesse)\s+alle\s+(?:deine\s+|zusätzlichen\s+)?fenster$")
});

/// "Close ur self", "kill process", "mach dich zu" — Jarvis quitting himself (never negated).
fn is_self_shutdown(s: &str) -> bool {
    !NEGATION.is_match(s) && (SELF_SHUTDOWN_EXACT.is_match(s) || SELF_SHUTDOWN_ANY.is_match(s))
}

/// The natural-language fallback (see the section comment). Runs after `parse` found nothing.
pub fn resolve_fuzzy(text: &str) -> Option<Intent> {
    let s = normalize(text);
    if s.is_empty() {
        return None;
    }
    let tool = |name: &'static str, args: Value| Some(Intent::Tool { name, args });

    if is_self_shutdown(&s) {
        return tool("shutdown_jarvis", json!({}));
    }
    if let Some(intent) = lifecycle_or_window_command(&s) {
        return Some(intent);
    }
    if TELEMETRY_WINDOW.is_match(&s) {
        return tool("spawn_native_window", json!({ "title": "System status", "telemetry": true, "width": 460, "height": 420 }));
    }
    if WINDOW_FROM_REPLY.is_match(&s) {
        return tool("spawn_native_window", json!({ "title": "J.A.R.V.I.S.", "from_last_reply": true }));
    }
    if let Some(c) = LIVE_WIDGET.captures(&s) {
        let (source, title) = match &c[1] {
            "ram" | "memory" | "arbeitsspeicher" => ("ram", "RAM usage"),
            "gpu" | "graphics card" | "grafikkarte" => ("gpu", "GPU load"),
            "vram" => ("vram", "VRAM usage"),
            "gpu temp" | "gpu temperature" => ("gpu_temp", "GPU temperature"),
            "disk" | "drive" | "festplatte" => ("disk", "Disk usage"),
            _ => ("cpu", "CPU load"),
        };
        let kind = match &c[2] {
            "chart" | "graph" | "diagramm" => "LiveLineChart",
            "sparkline" => "Sparkline",
            _ => "MetricGauge",
        };
        let unit = if source == "gpu_temp" { "°C" } else { "%" };
        return tool(
            "create_or_update_widget",
            json!({ "id": format!("{source}_{}", kind.to_lowercase()), "type": kind, "title": title, "source": source, "unit": unit }),
        );
    }
    if WIDGET_FROM_REPLY.is_match(&s) {
        return tool("create_or_update_widget", json!({ "type": "MarkdownNoteCard", "title": "Note", "from_last_reply": true }));
    }
    if OPEN_BROWSER_ANY.is_match(&s) {
        if let Some(query) = BROWSER_SEARCH.captures(&s).and_then(|c| c.get(1)).map(|m| m.as_str().trim().to_string()) {
            let vague = ["whatever", "something", "anything", "irgendwas", "was ich will"].iter().any(|v| query.contains(v));
            if !vague && !query.is_empty() {
                let q: String = query.split_whitespace().collect::<Vec<_>>().join("+");
                return tool("open_application", json!({ "app_name": format!("https://www.google.com/search?q={q}") }));
            }
        }
        return tool("open_application", json!({ "app_name": "browser" }));
    }
    // "Help my browser" → "open my browser".
    if let Some((first, rest)) = s.split_once(' ') {
        if OPEN_SOUNDALIKES.contains(&first) {
            let app = strip_article(rest);
            if known_app(&app) {
                return tool("open_application", json!({ "app_name": app }));
            }
        }
    }
    if let Some(c) = OPEN_ANY.captures(&s) {
        let app = strip_article(c.get(1)?.as_str());
        if known_app(&app) {
            return tool("open_application", json!({ "app_name": app }));
        }
    }
    None
}

/// Whole-sentence lifecycle and Jarvis-window commands (part of `parse`, ahead of the app rules
/// that would read "close the second window" as an application called "second").
fn lifecycle_or_window_command(s: &str) -> Option<Intent> {
    let tool = |name: &'static str, args: Value| Some(Intent::Tool { name, args });
    let lifecycle = |action: &str| tool("control_jarvis_lifecycle", json!({ "action": action }));
    if RESTART.is_match(s) {
        return lifecycle("restart_jarvis");
    }
    if SLEEP_MODE.is_match(s) {
        return lifecycle("sleep_mode");
    }
    if VOICE_MUTE.is_match(s) {
        return lifecycle("mute_tts");
    }
    if VOICE_UNMUTE.is_match(s) {
        return lifecycle("unmute_tts");
    }
    if CLOSE_ALL_JARVIS_WINDOWS.is_match(s) {
        return tool("close_native_window", json!({ "id": "all" }));
    }
    if CLOSE_JARVIS_WINDOW.is_match(s) {
        return tool("close_native_window", json!({ "id": "last", "fallback_to_active_window": true }));
    }
    if NEW_WINDOW.is_match(s) {
        return tool("spawn_native_window", json!({ "title": "J.A.R.V.I.S.", "from_last_reply": true }));
    }
    None
}

static CLAIM_SHUTDOWN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:shutting (?:myself |me )?down|powering (?:myself )?(?:down|off)|switching (?:myself )?off|signing off|going offline|closing (?:myself|down now)|shutting off|i(?:'ll| will) shut down)\b")
});
static CLAIM_OPEN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:opening|launching|starting|firing up|bringing up|pulling up)\s+(?:up\s+)?(?:your\s+|the\s+|a\s+)?([a-z0-9][a-z0-9 .\-]{1,30}?)(?:\s+(?:for you|now|right away|at once|immediately|straight away))?\s*(?:,|\.|!|\bsir\b|$)")
});
static CLAIM_CLOSE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:closing|quitting|terminating)\s+(?:down\s+)?(?:your\s+|the\s+)?([a-z0-9][a-z0-9 .\-]{1,30}?)(?:\s+(?:for you|now|right away|at once|immediately))?\s*(?:,|\.|!|\bsir\b|$)")
});
static ASKED_QUIT: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(?:close|quit|exit|shut|off|bye|goodbye|go away|leave|kill|aus|beend|schließ|schliess|schluss|stop)"));
static ASKED_OPEN: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(?:open|launch|start|run|fire up|öffne|oeffne|starte|auf\b|an\b)|\b(?:bring|pull|fire)\b.*\bup\b"));
static ASKED_CLOSE: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:close|quit|exit|kill|terminate|shut|schließ|schliess|beend|zu\b)"));

/// The model *said* it did something ("Opening your browser now, Sir.", "Powering down.") but
/// called no tool. Returns the tool call that keeps its word, when Sir's request asked for it.
pub fn claimed_action(reply: &str, request: &str) -> Option<(&'static str, Value)> {
    let said = reply.to_lowercase();
    let asked = normalize(request);
    if CLAIM_SHUTDOWN.is_match(&said) && (ASKED_QUIT.is_match(&asked) || is_self_shutdown(&asked)) {
        return Some(("shutdown_jarvis", json!({})));
    }
    if let Some(c) = CLAIM_OPEN.captures(&said) {
        let app = strip_article(c.get(1)?.as_str().trim());
        if ASKED_OPEN.is_match(&asked) && (known_app(&app) || app.contains("browser")) {
            let name = if app.contains("browser") { "browser".to_string() } else { app };
            return Some(("open_application", json!({ "app_name": name })));
        }
    }
    if let Some(c) = CLAIM_CLOSE.captures(&said) {
        let app = strip_article(c.get(1)?.as_str().trim());
        if ASKED_CLOSE.is_match(&asked) && known_app(&app) {
            return Some(("close_application", json!({ "app_name": app, "force": true })));
        }
    }
    // Windows, monitors, music, protocols: what the reply announced ("I'm creating a new window
    // showing your PC stats"), run exactly as the fast path would have run Sir's words.
    if requested_action(request) || is_affirmation(request) {
        if let Some(a) = reply_actions(reply).into_iter().find(|a| !a.offer) {
            return Some((a.name, placed_by_sir(a.args, request)));
        }
    }
    None
}

/// Built-in alias or an installed application with (almost) exactly this name.
fn known_app(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && n.split_whitespace().count() <= 4 && (crate::apps::find_spec(n).is_some() || crate::appindex::is_known(n))
}

/// Words a sentence does not end on when Sir is still thinking ("open the …", "… weil").
const CONNECTORS: &[&str] = &[
    "and", "or", "but", "because", "to", "for", "with", "of", "in", "on", "at", "from", "into", "the", "a", "an", "my", "your",
    "that", "so", "then", "if", "like", "about", "und", "oder", "aber", "weil", "dass", "wenn", "denn", "mit", "für", "fuer",
    "zu", "zum", "zur", "der", "die", "das", "den", "dem", "ein", "eine", "einen", "mein", "meine", "meinen", "auf", "an", "von",
    "nach", "bei", "also", "dann", "um", "ob", "als", "wie",
];

/// A transcript that sounds unfinished: trailing "…", a dangling connector, or a short fragment
/// that is not a complete command on its own. Jarvis waits a little longer for the rest.
pub fn looks_incomplete(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if t.ends_with("...") || t.ends_with('…') || t.ends_with(',') || t.ends_with('-') || t.ends_with(" -") {
        return true;
    }
    let last = t
        .split_whitespace()
        .last()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .unwrap_or_default();
    if CONNECTORS.contains(&last.as_str()) {
        return true;
    }
    if t.ends_with('?') || t.ends_with('!') {
        return false;
    }
    let words = t.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric)).count();
    words < 4 && parse(t).is_none() && resolve_fuzzy(t).is_none() && crate::modality::parse_command(t).is_none()
}

fn strip_article(s: &str) -> String {
    let mut t = s.trim();
    for art in ["the ", "my ", "den ", "die ", "das ", "dem ", "mein ", "meinen ", "meine ", "ein ", "eine ", "a "] {
        if let Some(rest) = t.strip_prefix(art) {
            t = rest.trim();
        }
    }
    // Polite tails and places Sir adds ("Discord for me", "Chrome on my PC", "VS Code real quick").
    for _ in 0..2 {
        for suffix in [
            " for me", " on my pc", " on my computer", " on the pc", " on the computer", " on this pc", " real quick", " quickly", " right now",
            " für mich", " auf meinem pc", " auf dem pc", " auf meinem rechner", " app", " application", " programm", " program", " window", " fenster",
        ] {
            if let Some(rest) = t.strip_suffix(suffix) {
                t = rest.trim();
            }
        }
    }
    t.to_string()
}

#[cfg(test)]
mod board_tests {
    use super::*;

    fn board(text: &str, active: bool, spotlight: bool) -> Option<Value> {
        match board_command(text, active, spotlight)? {
            Intent::Tool { name: "hud_board", args } => Some(args),
            Intent::Silent(SilentAction { tool: "hud_board", args, .. }) => Some(args),
            _ => None,
        }
    }

    #[test]
    fn the_film_phrases_drive_the_board() {
        assert_eq!(board("Hey Jarvis, are you there?", false, false), Some(json!({"action": "wake"})));
        assert_eq!(board("Can you do a state of VR report for me?", false, false), Some(json!({"action": "research", "topic": "state of VR"})));
        assert_eq!(board("Yes please, present the report.", false, false), Some(json!({"action": "present"})));
        assert_eq!(board("Bring up that VR report you gave me earlier", false, false), Some(json!({"action": "recall", "topic": "vr"})));
        assert_eq!(board("Can you maximize the chart please?", true, false), Some(json!({"action": "maximize", "target": "chart"})));
        assert_eq!(board("Maximize that for me", true, false), Some(json!({"action": "maximize", "target": ""})));
        assert_eq!(board("Back to board", true, true), Some(json!({"action": "minimize"})));
        assert_eq!(board("Thanks Jarvis, you can go to sleep again.", true, false), Some(json!({"action": "sign_off"})));
        assert_eq!(board("Sign off", false, false), Some(json!({"action": "sign_off"})));
        // "Clear the board" takes the blocks away; it is no sign-off (the model stays loaded).
        for text in ["Clear the board", "Jarvis, clear the HUD", "Clear view", "Board leeren"] {
            assert_eq!(board(text, true, false), Some(json!({"action": "clear"})), "{text}");
        }
        assert_eq!(board("Clear my calendar", true, false), None);
        assert_eq!(board("Show me Tulleys Farm on the map", false, false), Some(json!({"action": "show_map", "place": "Tulleys Farm"})));
        assert_eq!(board("next note", true, false), Some(json!({"action": "flip_note", "direction": "next"})));
        // Voice pagination of a stacked block, and closing the board by name (not as a program).
        for text in ["Jarvis, next page", "Next slide", "Next result", "Go forward", "Nächste Seite"] {
            assert_eq!(board(text, true, false), Some(json!({"action": "flip_note", "direction": "next"})), "{text}");
        }
        for text in ["Previous page", "Go back", "previous result", "Zurück"] {
            assert_eq!(board(text, true, false), Some(json!({"action": "flip_note", "direction": "previous"})), "{text}");
        }
        assert_eq!(board("Page 3", true, false), Some(json!({"action": "flip_note", "page": 3})));
        assert_eq!(board("go to page two", true, false), Some(json!({"action": "flip_note", "page": 2})));
        for (text, page) in [("Show result 3", 3), ("Show me result 3", 3), ("Result two", 2), ("The second result", 2), ("Go to the third page", 3), ("Seite zwei", 2)] {
            assert_eq!(board(text, true, false), Some(json!({"action": "flip_note", "page": page})), "{text}");
        }
        for text in ["Show me the next result", "The next one", "Flip to the next card"] {
            assert_eq!(board(text, true, false), Some(json!({"action": "flip_note", "direction": "next"})), "{text}");
        }
        assert_eq!(board("Next page", false, false), None, "no board up");
        for text in ["Close the HUD board, please.", "Close the HUD board", "Close web search", "Jarvis close to web search.", "Schließe das HUD Board", "Mach das Board zu", "HUD Board schließen"] {
            assert_eq!(board(text, true, false), Some(json!({"action": "close"})), "{text}");
            assert!(matches!(parse(text), Some(Intent::Silent(SilentAction { tool: "hud_board", .. }))), "never close_application: {text}");
        }
        // One of Jarvis' own blocks by name: that block closes, the board stays.
        for (text, target) in [("Close the feature lab", "feature lab"), ("Close telemetry", "telemetry"), ("Hide the feature lab window", "feature lab"), ("Close the theme studio", "theme studio")] {
            assert_eq!(board(text, true, false), Some(json!({"action": "close", "target": target})), "{text}");
            assert!(matches!(parse(text), Some(Intent::Silent(SilentAction { tool: "hud_board", .. }))), "never close_application: {text}");
        }
        // Programs and HUD panels are someone else's.
        for text in ["Close Spotify", "Hide the system panel", "Close the settings"] {
            assert_eq!(board(text, true, false), None, "{text}");
        }
    }

    #[test]
    fn board_phrases_leave_windows_and_long_missions_alone() {
        // No board up: "maximise that" is a window command.
        assert_eq!(board("Maximize that", false, false), None);
        // With a board up, a named app is still a window.
        assert_eq!(board("Maximize Spotify", true, false), None);
        // A multi-hour mission belongs to the Deep Research Engine.
        assert_eq!(board("Research solid-state batteries for three hours", false, false), None);
        assert_eq!(board("research quantum computing and bring up the windows", false, false), None, "the knowledge workspace");
        assert_eq!(board("Don't sign off", false, false), None);
        assert!(matches!(parse("Sign off"), Some(Intent::Tool { name: "hud_board", .. })), "sign off no longer quits Jarvis");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(text: &str) -> Option<(&'static str, Value)> {
        match parse(text) {
            Some(Intent::Tool { name, args }) => Some((name, args)),
            _ => None,
        }
    }

    #[test]
    fn stop_and_time() {
        assert_eq!(parse("Stop!"), Some(Intent::Stop));
        assert_eq!(parse("Jarvis, sei still."), Some(Intent::Stop));
        assert_eq!(parse("What time is it?"), Some(Intent::Time));
        assert_eq!(parse("Wie spät ist es?"), Some(Intent::Time));
        assert_eq!(parse("Welcher Tag ist heute?"), Some(Intent::Date));
    }

    #[test]
    fn volume_and_media() {
        assert_eq!(tool("Volume 50%").unwrap().1["level"], 50);
        assert_eq!(tool("Set the volume to 30 percent please").unwrap().1["level"], 30);
        assert_eq!(tool("Lautstärke auf 70").unwrap().1["level"], 70);
        assert_eq!(tool("Stell die Lautstärke auf 25 Prozent").unwrap().1["level"], 25);
        assert_eq!(tool("40% volume").unwrap().1["level"], 40);
        assert_eq!(tool("Mute").unwrap().1["action"], "mute");
        assert_eq!(tool("Ton aus").unwrap().1["action"], "mute");
        assert_eq!(tool("Nächstes Lied").unwrap().1["action"], "next_track");
        assert_eq!(tool("Pause the music"), Some(("control_music", json!({"action": "pause"}))));
        assert_eq!(tool("Mach lauter").unwrap().1["action"], "volume_up");
        assert_eq!(tool("Speak louder").unwrap().1["action"], "jarvis_louder");
    }

    #[test]
    fn apps_and_windows() {
        assert_eq!(tool("Open Spotify"), Some(("open_application", json!({"app_name": "spotify"}))));
        assert_eq!(tool("Jarvis, öffne bitte Discord."), Some(("open_application", json!({"app_name": "discord"}))));
        assert_eq!(tool("Mach mal Spotify auf"), Some(("open_application", json!({"app_name": "spotify"}))));
        assert_eq!(tool("Starte den Browser"), Some(("open_application", json!({"app_name": "browser"}))));
        assert_eq!(tool("VS Code öffnen"), Some(("open_application", json!({"app_name": "vs code"}))));
        assert_eq!(tool("Close Discord"), Some(("close_application", json!({"app_name": "discord", "force": true}))));
        assert_eq!(tool("Schließe Spotify"), Some(("close_application", json!({"app_name": "spotify", "force": true}))));
        assert_eq!(tool("Mach Steam zu"), Some(("close_application", json!({"app_name": "steam", "force": true}))));
        assert_eq!(tool("Minimize all windows").unwrap().1["action"], "minimize_all");
        assert_eq!(tool("Snap Chrome to the left").unwrap().1, json!({"action": "snap_left", "target_window": "chrome"}));
        assert_eq!(tool("Maximize Spotify").unwrap().1, json!({"action": "maximize", "target_window": "spotify"}));
        assert_eq!(tool("Spotify minimieren").unwrap().1, json!({"action": "minimize", "target_window": "spotify"}));
    }

    #[test]
    fn memory_commands_are_detected() {
        assert!(is_memory_command("Merk dir bitte, dass ich meinen Kaffee schwarz trinke."));
        assert!(is_memory_command("Jarvis, remember that my sister is called Anna"));
        assert!(is_memory_command("Mein Name ist Max"));
        assert!(is_memory_command("Vergiss, dass ich Kaffee trinke"));
        assert!(!is_memory_command("Erinnere mich morgen an das Meeting"));
        assert!(!is_memory_command("What do you remember about physics?"));
        assert_eq!(parse("Vergiss es"), Some(Intent::Stop));
        assert_eq!(parse("Never mind"), Some(Intent::Stop));
    }

    #[test]
    fn live_information_is_searched_first() {
        assert!(live_info_query("Wie ist das Wetter gerade in Berlin?").is_some());
        assert!(live_info_query("What's the latest news about SpaceX").is_some());
        assert!(live_info_query("Wer hat gestern in der Bundesliga gewonnen?").is_some());
        assert!(live_info_query("What is the Bitcoin price today").is_some());
        assert!(live_info_query("Tell me a joke").is_none());
        assert!(live_info_query("Weather").is_none(), "too vague on its own");
    }

    #[test]
    fn shutting_jarvis_down() {
        let quits = |text: &str| tool(text).map(|(name, _)| name) == Some("shutdown_jarvis");
        // Phrases from the log that used to fail ("close_application yourself/your").
        for text in [
            "Jarvis, please close yourself.",
            "bitte fahre Jarvis herunter",
            "Jarvis, kill your app.",
            "Shut down.",
            "Jarvis, shut yourself down",
            "Close Jarvis",
            "Quit",
            "Turn yourself off",
            "Beende dich",
            "Jarvis beenden",
            "Schalte dich aus",
            "Fahr herunter",
        ] {
            assert!(quits(text), "should quit: {text}");
        }
        for text in ["Shut down the computer", "Fahr den PC herunter", "Turn down", "Turn off the lights", "Close Spotify"] {
            assert!(!quits(text), "should not quit Jarvis: {text}");
        }
        assert_eq!(parse("Stop, Jarvis"), Some(Intent::Stop), "stop only silences");
    }

    #[test]
    fn custom_themes_monitors_and_the_orb_by_voice() {
        let hud = |text: &str| match tool(text) {
            Some(("configure_interface", args)) => Some(args),
            other => panic!("{text}: {other:?}"),
        };
        // STT hears "theme" as seam / steam / team / scheme.
        for text in ["Can you make your seam yellow?", "Make the steam yellow", "Change your team to yellow", "Make your colour scheme yellow"] {
            assert_eq!(hud(text), Some(json!({"accent_color": "yellow"})), "{text}");
        }
        assert_eq!(
            hud("Create a custom theme called Cyberpunk with neon yellow borders and electric purple core"),
            Some(json!({"create_theme": {"name": "Cyberpunk", "description": "neon yellow borders and electric purple core"}}))
        );
        assert_eq!(hud("Design a dark stealth theme"), Some(json!({"create_theme": {"description": "dark stealth"}})));
        assert_eq!(hud("Save this theme as Night Ops"), Some(json!({"save_theme": "Night Ops"})));
        assert_eq!(hud("Save this seam as night ops"), Some(json!({"save_theme": "Night Ops"})));
        assert_eq!(hud("Load my Cyberpunk theme"), Some(json!({"load_theme": "Cyberpunk"})));
        for text in ["Overlap the windows", "Stack them", "Let the windows overlap"] {
            assert_eq!(hud(text), Some(json!({"allow_window_overlap": true})), "{text}");
        }
        assert_eq!(hud("Keep the windows apart"), Some(json!({"allow_window_overlap": false})));
        for text in ["Use all monitors", "Span all my screens", "Stretch yourself across all monitors", "Use both screens"] {
            assert_eq!(hud(text), Some(json!({"span_all_monitors": true})), "{text}");
        }
        assert_eq!(hud("Use only one monitor"), Some(json!({"span_all_monitors": false})));
        assert_eq!(hud("Move the orb to the top left"), Some(json!({"orb_board_position": "top left"})));
        assert_eq!(hud("Put the emblem on the right side"), Some(json!({"orb_board_position": "right side"})));
        assert_eq!(hud("Move the orb to bottom center"), Some(json!({"orb_board_position": "bottom center"})));
        // The screenshot bug: "back" between the orb and the place fell through to the model.
        for (text, spot) in [
            ("Jarvis, move your orb back to the middle.", "middle"),
            ("Move your orb to the top right", "top right"),
            ("Move your orb left", "left"),
            ("Move the orb slightly left", "slightly left"),
            ("Bewege deinen Orb in die Mitte", "mitte"),
            ("Schieb den Orb nach links", "links"),
        ] {
            assert_eq!(tool(text), Some(("configure_interface", json!({"orb_board_position": spot}))), "{text}");
        }
        // "Move to my 3rd monitor": Jarvis himself.
        for (text, n) in [("Move to my 3rd monitor", 3), ("Jarvis, go to monitor 2", 2), ("Move over to the second screen", 2)] {
            assert_eq!(tool(text), Some(("move_window_to_monitor", json!({"app_or_window_name": "jarvis", "target_monitor": n}))), "{text}");
        }
    }

    #[test]
    fn restyling_the_hud() {
        let hud = |text: &str| match tool(text) {
            Some(("configure_interface", args)) => Some(args),
            _ => None,
        };
        assert_eq!(hud("Jarvis, make the interface red"), Some(json!({"accent_color": "red"})));
        assert_eq!(hud("Mach das Interface rot"), Some(json!({"accent_color": "rot"})));
        assert_eq!(hud("Ändere die Farbe zu Grün"), Some(json!({"accent_color": "grün"})));
        assert_eq!(hud("Switch to the Iron Man theme"), Some(json!({"theme": "iron man"})));
        assert_eq!(hud("Dark mode"), Some(json!({"theme": "dark"})));
        assert_eq!(hud("Compact mode"), Some(json!({"layout": "compact"})));
        assert_eq!(hud("Kompaktmodus"), Some(json!({"layout": "compact"})));
        assert_eq!(hud("Make the background dark blue"), Some(json!({"background_color": "dark blue"})));
        assert_eq!(hud("Roter Hintergrund"), Some(json!({"background_color": "roter"})));
        assert_eq!(hud("Hide the system panel"), Some(json!({"hide_panels": ["system panel"]})));
        assert_eq!(hud("Close the chat"), Some(json!({"hide_panels": ["chat"]})));
        assert_eq!(hud("Show me the tool log again"), Some(json!({"show_panels": ["tool log"]})));
        assert_eq!(hud("Blende das Gedächtnis aus"), Some(json!({"hide_panels": ["gedächtnis"]})));
        assert_eq!(hud("Skills einblenden"), Some(json!({"show_panels": ["skills"]})));
        assert_eq!(hud("Make the text bigger"), Some(json!({"scale": "bigger"})));
        assert_eq!(hud("Zoom out"), Some(json!({"scale": "smaller"})));
        assert_eq!(hud("Set the zoom to 125 percent"), Some(json!({"scale": 1.25})));
        assert_eq!(hud("Make the window more transparent"), Some(json!({"opacity": "-15"})));
        assert_eq!(hud("Transparenz auf 30 Prozent"), Some(json!({"transparency": 30})));
        assert_eq!(hud("Stay on top"), Some(json!({"always_on_top": true})));
        assert_eq!(hud("Don't stay on top"), Some(json!({"always_on_top": false})));
        assert_eq!(hud("Immer im Vordergrund"), Some(json!({"always_on_top": true})));
        assert_eq!(hud("Fullscreen yourself"), Some(json!({"fullscreen": true})));
        assert_eq!(hud("Make the HUD full screen"), Some(json!({"fullscreen": true})));
        // "Just the orb" is pure orb mode now (the floating orb, not the orb layout in a window).
        assert_eq!(tool("Just the orb"), Some(("control_orb_mode", json!({"enabled": true}))));
        assert_eq!(hud("Nur den Reaktor"), Some(json!({"layout": "orb"})));
        assert_eq!(hud("Hide everything"), Some(json!({"hide_panels": ["everything"]})));
        assert_eq!(hud("Reset the interface"), Some(json!({"reset": true})));
        assert_eq!(hud("Show yourself"), Some(json!({"window": "show"})));
        assert_eq!(hud("Versteck dich"), Some(json!({"window": "hide"})));
        assert_eq!(hud("Minimize yourself"), Some(json!({"window": "minimize"})));
        assert_eq!(hud("Move yourself to the top right corner"), Some(json!({"window": "top_right"})));
        assert_eq!(hud("Schieb dich nach links"), Some(json!({"window": "left"})));
    }

    #[test]
    fn layout_goals_and_panel_requests_act_at_once() {
        let ui = |text: &str| ui_layout_command(text);
        assert_eq!(ui("Just make the overall interface less full."), Some(json!({"action": "declutter"})));
        assert_eq!(ui("Can you make the interface cleaner?"), Some(json!({"action": "declutter"})));
        assert_eq!(ui("Clean up the UI"), Some(json!({"action": "declutter"})));
        assert_eq!(ui("Optimise my layout"), Some(json!({"action": "declutter"})));
        assert_eq!(
            ui("Just leave the normal chat window, but remove the Audio Devices tab and the Skills tab, please."),
            Some(json!({"action": "hide_panel", "target_panel": ["audio_devices", "skills"]}))
        );
        assert_eq!(ui("Jarvis, can you disable the Audio Devices tab from the interface?"), Some(json!({"action": "hide_panel", "target_panel": ["audio_devices"]})));
        assert_eq!(ui("Bring back the Audio settings"), Some(json!({"action": "show_panel", "target_panel": ["audio_devices"]})));
        assert_eq!(ui("Turn off the telemetry"), Some(json!({"action": "hide_panel", "target_panel": ["telemetry"]})));
        assert_eq!(ui("Make the orb bigger"), Some(json!({"action": "orb_bigger"})));
        assert_eq!(ui("Make the sphere smaller"), Some(json!({"action": "orb_smaller"})));
        assert_eq!(ui("Set the orb size to 150 percent"), Some(json!({"action": "set_orb_scale", "scale_value": 1.5})));
        assert_eq!(ui("Zen mode"), Some(json!({"action": "zen_mode_on"})));
        assert_eq!(ui("Exit zen mode"), Some(json!({"action": "zen_mode_off"})));
        assert_eq!(ui("Show all panels again"), Some(json!({"action": "restore_all"})));
        for text in ["Open Spotify", "Close Discord", "Turn off the microphone", "Show me the weather", "Clean up my downloads folder", "Remove the reminder"] {
            assert_eq!(ui(text), None, "{text}");
        }
        assert_eq!(tool("Pick some good music"), Some(("control_music", json!({"action": "pick_music"}))));
        assert_eq!(tool("Play something"), Some(("control_music", json!({"action": "pick_music"}))));
    }

    #[test]
    fn follow_ups_act_on_the_focus_never_on_jarvis() {
        let window = |text: &str| match tool(text) {
            Some(("window_management", args)) if args.get("target_window").is_none() => args["action"].as_str().map(String::from),
            other => panic!("{text}: {other:?}"),
        };
        for text in ["Full screen", "fullscreen", "Jarvis, go full screen", "Make it full screen", "Put that in fullscreen", "Vollbild", "Mach es auf Vollbild", "full screen please"] {
            assert_eq!(window(text).as_deref(), Some("fullscreen"), "{text}");
        }
        for text in ["Exit full screen", "Vollbild beenden", "Leave fullscreen mode"] {
            assert_eq!(window(text).as_deref(), Some("exit_fullscreen"), "{text}");
        }
        assert_eq!(window("Maximize").as_deref(), Some("maximize"));
        assert_eq!(window("Maximise it").as_deref(), Some("maximize"));
        assert_eq!(window("Minimize that").as_deref(), Some("minimize"));
        assert_eq!(window("Es minimieren").as_deref(), Some("minimize"));
        assert_eq!(window("Restore it").as_deref(), Some("restore"));
        assert_eq!(window("Move it to the left").as_deref(), Some("snap_left"));
        assert_eq!(window("Schieb es nach rechts").as_deref(), Some("snap_right"));
        assert_eq!(window("Move it to the top").as_deref(), Some("snap_top"));
        assert_eq!(window("Bring it back").as_deref(), Some("focus"));
        assert_eq!(window("Close the tab").as_deref(), Some("close_tab"));
        for text in ["Close it", "close that", "Kill it", "Mach es zu", "Schließ das"] {
            assert_eq!(tool(text), Some(("close_application", json!({"app_name": "it", "force": true}))), "{text}");
        }
        // Jarvis himself only when Sir names him.
        assert_eq!(tool("Maximize Jarvis"), Some(("configure_interface", json!({"window": "maximize"}))));
        assert_eq!(tool("Jarvis, minimise yourself"), Some(("configure_interface", json!({"window": "minimize"}))));
        assert_eq!(tool("Make yourself full screen"), Some(("configure_interface", json!({"fullscreen": true}))));
        assert_eq!(tool("Close yourself").map(|t| t.0), Some("shutdown_jarvis"));
        // Named apps keep working.
        assert_eq!(tool("Maximize Spotify").unwrap().1, json!({"action": "maximize", "target_window": "spotify"}));
        assert_eq!(tool("Close Discord"), Some(("close_application", json!({"app_name": "discord", "force": true}))));
    }

    #[test]
    fn websites_channels_and_platform_searches() {
        let web = |text: &str| match tool(text) {
            Some(("open_website", a)) => (
                a["platform_or_url"].as_str().unwrap_or("").to_string(),
                a["action_type"].as_str().unwrap_or("").to_string(),
                a["target_or_query"].as_str().unwrap_or("").to_string(),
            ),
            other => panic!("{text}: {other:?}"),
        };
        let t = |p: &str, m: &str, q: &str| (p.to_string(), m.to_string(), q.to_string());
        assert_eq!(web("Open BastiGHG on YouTube"), t("youtube", "channel_or_profile", "BastiGHG"), "the handle keeps its capitals");
        assert_eq!(web("Jarvis, go to BastiGHG's YouTube channel"), t("youtube", "channel_or_profile", "BastiGHG"));
        assert_eq!(web("Open the YouTube channel of BastiGHG"), t("youtube", "channel_or_profile", "BastiGHG"));
        assert_eq!(web("Öffne BastiGHGs YouTube-Kanal"), t("youtube", "channel_or_profile", "BastiGHG"));
        assert_eq!(web("Open BastiGHG on Twitch"), t("twitch", "channel_or_profile", "BastiGHG"));
        assert_eq!(web("Show me BastiGHG on YouTube"), t("youtube", "channel_or_profile", "BastiGHG"));
        assert_eq!(web("Zeig mir den YouTube-Kanal von BastiGHG"), t("youtube", "channel_or_profile", "BastiGHG"));
        assert_eq!(web("Open torvalds on GitHub"), t("github", "channel_or_profile", "torvalds"));
        assert_eq!(web("Open r/rust"), t("reddit", "channel_or_profile", "rust"));
        assert_eq!(web("Open YouTube and search for Minecraft"), t("youtube", "site_search", "Minecraft"));
        assert_eq!(web("Search YouTube for Minecraft"), t("youtube", "site_search", "Minecraft"));
        assert_eq!(web("Search for RTX 3060 Ti on Kleinanzeigen"), t("kleinanzeigen", "site_search", "RTX 3060 Ti"));
        assert_eq!(web("Suche auf Amazon nach USB-C Kabel"), t("amazon", "site_search", "USB-C Kabel"));
        assert_eq!(web("Look up Albert Einstein on Wikipedia"), t("wikipedia", "site_search", "Albert Einstein"));
        assert_eq!(tool("Suche nach Albert Einstein auf Wikipedia").unwrap().1["lang"], "de");
        assert_eq!(web("Google rust tutorials"), t("google", "site_search", "rust tutorials"));
        assert_eq!(web("Open YouTube"), t("youtube", "direct_url", ""));
        assert_eq!(web("Go to Twitch"), t("twitch", "direct_url", ""));
        assert_eq!(web("Open github.com"), t("github", "direct_url", ""));
        // Real apps stay apps; unknown platforms are not websites.
        assert_eq!(tool("Open Spotify"), Some(("open_application", json!({"app_name": "spotify"}))));
        assert_eq!(tool("Open Discord").map(|t| t.0), Some("open_application"));
        assert!(tool("Open Spotify on my second monitor").is_none_or(|(n, _)| n != "open_website"));
    }

    #[test]
    fn file_follow_ups_and_window_styles() {
        assert_eq!(tool("Undo that edit"), Some(("undo_last_file_edit", json!({}))));
        assert_eq!(tool("Mach die Änderung rückgängig"), Some(("undo_last_file_edit", json!({}))));
        assert_eq!(tool("Open that file in VS Code"), Some(("open_application", json!({"open_last_file": true, "with": "vscode"}))));
        assert_eq!(tool("Show me that file in the explorer").unwrap().1["with"], "explorer");
        assert_eq!(tool("Open that file").unwrap().1["with"], "default");
        let style = |text: &str| match tool(text) {
            Some(("customize_hud_and_windows", a)) => a,
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(style("Make that window transparent"), json!({"opacity": 0.6, "target": "focus"}));
        assert_eq!(style("Make the second window borderless"), json!({"borderless": true, "target": "focus"}));
        assert_eq!(style("Make that window red"), json!({"accent_color": "red", "target": "focus"}));
        assert_eq!(style("Make that window ultron"), json!({"theme": "ultron", "target": "focus"}));
        assert_eq!(style("Pin that window"), json!({"always_on_top": true, "target": "focus"}));
        assert_eq!(style("Unpin that window"), json!({"always_on_top": false, "target": "focus"}));
        // "The window" / "your window" is still the HUD.
        assert_eq!(tool("Make the window more transparent"), Some(("configure_interface", json!({"opacity": "-15"}))));
    }

    #[test]
    fn hud_rules_leave_other_requests_alone() {
        assert_eq!(tool("Turn off the microphone"), None, "muting is not hiding the mic panel");
        assert_eq!(tool("Close Discord"), Some(("close_application", json!({"app_name": "discord", "force": true}))));
        assert_eq!(tool("Switch to Spotify").unwrap().1, json!({"action": "focus", "target_window": "spotify"}));
        assert_eq!(tool("Snap Chrome to the left").unwrap().1, json!({"action": "snap_left", "target_window": "chrome"}));
        assert_eq!(tool("Show the desktop").unwrap().1["action"], "minimize_all");
        assert_eq!(tool("Minimize everything").unwrap().1["action"], "minimize_all");
        assert_eq!(tool("Turn it down").unwrap().1["action"], "volume_down");
        for text in ["Make a skill that checks my battery", "Change the language to German", "Set a timer"] {
            assert!(tool(text).is_none_or(|(name, _)| name != "configure_interface"), "{text}");
        }
    }

    fn fuzzy(text: &str) -> Option<(&'static str, Value)> {
        match parse(text).or_else(|| resolve_fuzzy(text)) {
            Some(Intent::Tool { name, args }) => Some((name, args)),
            _ => None,
        }
    }

    #[test]
    fn natural_self_shutdown_variants_execute_the_tool() {
        for text in [
            "You can't assist me right now, can you please close yourself?",
            "close ur self",
            "Shut ur self down",
            "Mach dich aus",
            "Mach dich zu",
            "Go away",
            "Exit",
            "Kill process",
            "Jarvis, turn yourself off now",
            "Could you power yourself down for today",
        ] {
            assert_eq!(fuzzy(text).map(|(n, _)| n), Some("shutdown_jarvis"), "{text}");
        }
        for text in ["Don't close yourself", "Close Spotify", "Turn off the lights", "Shut down the computer"] {
            assert_ne!(fuzzy(text).map(|(n, _)| n), Some("shutdown_jarvis"), "{text}");
        }
    }

    #[test]
    fn browser_and_apps_from_natural_or_misheard_requests() {
        let browser = Some(("open_application", json!({"app_name": "browser"})));
        assert_eq!(fuzzy("Nana, open my browser."), browser);
        assert_eq!(fuzzy("Help my Browser."), browser);
        assert_eq!(fuzzy("Open browser"), browser);
        let web = fuzzy("Launch web").unwrap().1;
        assert_eq!(crate::apps::find_spec(web["app_name"].as_str().unwrap()).map(|s| s.id), Some("browser"));
        assert_eq!(fuzzy("Could you maybe open the web browser for me?"), browser);
        assert_eq!(fuzzy("To open my browser and search for whatever I want to."), browser);
        assert_eq!(
            fuzzy("Open my browser and search for rust tutorials"),
            Some(("open_application", json!({"app_name": "https://www.google.com/search?q=rust+tutorials"})))
        );
        assert_eq!(fuzzy("Would you mind opening Spotify"), Some(("open_application", json!({"app_name": "spotify"}))));
        assert_eq!(fuzzy("I'm starting to think about dinner"), None);
        assert_eq!(fuzzy("Can you start a timer"), None);
    }

    #[test]
    fn windows_widgets_and_lifecycle() {
        assert_eq!(fuzzy("Put that in a second window").map(|(n, _)| n), Some("spawn_native_window"));
        assert_eq!(fuzzy("Zeig das in einem neuen Fenster").map(|(n, _)| n), Some("spawn_native_window"));
        assert_eq!(fuzzy("Open a new window").map(|(n, _)| n), Some("spawn_native_window"));
        assert_eq!(fuzzy("Put that information into a widget").map(|(n, _)| n), Some("create_or_update_widget"));
        let gauge = fuzzy("Create a live CPU gauge widget").unwrap();
        assert_eq!((gauge.0, gauge.1["type"].as_str(), gauge.1["source"].as_str()), ("create_or_update_widget", Some("MetricGauge"), Some("cpu")));
        assert_eq!(fuzzy("Add a GPU temperature chart").unwrap().1["source"], "gpu_temp");
        assert_eq!(fuzzy("Show my system status in a new window").unwrap().1["live"], "pc_stats");
        assert_eq!(fuzzy("Close that window").unwrap().1["id"], "last");
        assert_eq!(fuzzy("Close the second window").unwrap().1["id"], "last");
        assert_eq!(fuzzy("Close all your windows").unwrap().1["id"], "all");
        assert_eq!(fuzzy("Close your window"), Some(("configure_interface", json!({"window": "hide"}))));
        assert_eq!(fuzzy("Go to sleep").unwrap().1["action"], "sleep_mode");
        assert_eq!(fuzzy("Restart yourself").unwrap().1["action"], "restart_jarvis");
        assert_eq!(fuzzy("Mute your voice").unwrap().1["action"], "mute_tts");
    }

    #[test]
    fn assistant_abilities_are_instant() {
        let name = |text: &str| tool(text).map(|(n, _)| n);
        assert_eq!(name("Good morning, Jarvis"), Some("run_executive_briefing"));
        assert_eq!(name("Brief me"), Some("run_executive_briefing"));
        assert_eq!(name("Was steht heute an?"), Some("run_executive_briefing"));
        assert_eq!(name("Jarvis, give me my morning briefing"), Some("run_executive_briefing"));
        assert_eq!(name("Update me on my day"), Some("run_executive_briefing"));
        assert_eq!(tool("Give me my evening briefing").unwrap().1["time_of_day"], "evening");
        let (_, r) = tool("Remind me in 10 minutes to check the oven").unwrap();
        assert_eq!((r["action"].as_str(), r["text"].as_str()), (Some("set"), Some("check the oven")));
        assert_eq!(tool("What are my reminders?").unwrap().1["action"], "list");
        assert_eq!(tool("Cancel all reminders").unwrap().1["id"], "all");
        let timer = tool("Set a timer for 5 minutes").unwrap().1;
        assert_eq!((timer["type"].as_str(), timer["duration_secs"].as_f64()), (Some("CountdownOrStopwatch"), Some(300.0)));
        assert_eq!(tool("Start a timer for ten minutes").unwrap().1["duration_secs"], 600.0);
        assert_eq!(tool("Stell einen Timer auf 3 Minuten").unwrap().1["duration_secs"], 180.0);
        assert_eq!(tool("Focus mode for 45 minutes").unwrap().1, json!({"action": "start", "minutes": 45.0}));
        assert_eq!(tool("Engage focus mode").unwrap().1["minutes"], 25.0);
        assert_eq!(tool("Stop focus mode").unwrap().1["action"], "stop");
        assert_eq!(name("Copy that"), Some("clipboard"));
        assert_eq!(name("Am I online?"), Some("network_status"));
        assert_eq!(name("What's my IP address"), Some("network_status"));
        assert_eq!(name("What's slowing down my PC?"), Some("top_processes"));
        assert_eq!(name("Warum ist mein PC so langsam?"), Some("top_processes"));
        assert_eq!(tool("Speak a bit faster").unwrap().1["action"], "jarvis_faster");
        assert_eq!(tool("Find my tax return pdf").unwrap().1, json!({"query": "tax return pdf"}));
        assert_eq!(tool("Open budget.xlsx").unwrap().1, json!({"query": "budget.xlsx", "open": true}));
        assert_eq!(name("Find a good restaurant nearby"), None, "not a file");
        assert_eq!(parse("What's 15 percent of 240?"), Some(Intent::Answer("15 percent of 240 is 36, Sir.".into())));
        assert!(matches!(parse("Convert 5 miles to km"), Some(Intent::Answer(_))));
        assert_eq!(upfront_tool("What's on my screen?"), Some(("inspect_screen_or_selection", json!({"mode": "monitor", "target_monitor": "primary"}))));
        assert_eq!(upfront_tool("Can you explain this error?"), Some(("inspect_screen_or_selection", json!({"mode": "active_window"}))));
        assert_eq!(upfront_tool("Jarvis, what does this error mean?").unwrap().1["mode"], "active_window");
        assert_eq!(upfront_tool("Look at my second monitor"), Some(("inspect_screen_or_selection", json!({"mode": "monitor", "target_monitor": 2}))));
        assert_eq!(upfront_tool("Summarize what's on monitor 1").unwrap().1["target_monitor"], 1);
        assert_eq!(upfront_tool("Refactor the code I selected"), Some(("inspect_screen_or_selection", json!({"mode": "selection"}))));
        assert_eq!(upfront_tool("Translate the highlighted text").unwrap().1["mode"], "selection");
        assert_eq!(upfront_tool("Erklär mir den markierten Code").unwrap().1["mode"], "selection");
        assert_eq!(upfront_tool("Summarise what I copied").map(|t| t.0), Some("clipboard"));
        let recall = upfront_tool("What did we talk about yesterday?").unwrap();
        assert_eq!((recall.0, recall.1["days_back"].as_u64()), ("search_conversation", Some(2)));
        assert_eq!(upfront_tool("What did I say about Pepper?").unwrap().1["query"], "pepper");
        assert_eq!(upfront_tool("Open Spotify"), None);
    }

    #[test]
    fn claimed_actions_are_carried_out() {
        assert_eq!(
            claimed_action("Very good, Sir. Shutting down now.", "you can't help me, please go"),
            None,
            "no quit request"
        );
        assert_eq!(claimed_action("As you wish, Sir. Powering down.", "Jarvis you should exit now").map(|(n, _)| n), Some("shutdown_jarvis"));
        assert_eq!(
            claimed_action("Right away, Sir. Opening your browser now.", "could you bring my browser up"),
            Some(("open_application", json!({"app_name": "browser"})))
        );
        assert_eq!(
            claimed_action("Of course, Sir. Opening Spotify.", "please open up spotify for the party"),
            Some(("open_application", json!({"app_name": "spotify"})))
        );
        assert_eq!(claimed_action("Closing Discord for you, Sir.", "close discord please").map(|(n, _)| n), Some("close_application"));
        assert_eq!(claimed_action("Opening the pod bay doors is beyond me, Sir.", "open the pod bay doors"), None);
        assert_eq!(claimed_action("The weather is lovely.", "open spotify"), None);
    }

    #[test]
    fn live_windows_are_opened_not_talked_about() {
        // The exact requests from the log that only got a spoken "I'm creating a window".
        let live = |text: &str| match tool(text) {
            Some(("spawn_native_window", a)) => a,
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(live("Jarvis, can you create a new window please that shows me my PC stats?"), json!({"live": "pc_stats", "separate_window": true}));
        assert_eq!(live("Please create a new window showing my remaining space on my drives."), json!({"live": "drive_space", "separate_window": true}));
        assert_eq!(live("Show my CPU and GPU usage in a new window on my second monitor"), json!({"live": "pc_stats", "monitor": 2, "separate_window": true}));
        assert_eq!(live("Open a window with my drives and processes")["add_content"], json!(["processes"]));
        assert_eq!(live("Zeig mir meinen Speicherplatz in einem neuen Fenster")["live"], "drive_space");
        assert_eq!(live("Create a 800x600 window with my network traffic"), json!({"live": "network", "width": 800, "height": 600}));
        // Not live views: these stay what they were.
        assert_eq!(tool("Close that window"), Some(("close_native_window", json!({"id": "last", "fallback_to_active_window": true}))));
        assert!(tool("Open a new window").is_some_and(|(_, a)| a.get("live").is_none()));
    }

    #[test]
    fn jarvis_windows_are_resized_restyled_and_extended() {
        let edit = |text: &str| tool(text).unwrap_or_else(|| panic!("{text}"));
        assert_eq!(edit("Make the drive space window smaller"), ("update_native_window", json!({"id": "drive space", "size": "smaller"})));
        assert_eq!(edit("Resize it to 800 by 500"), ("update_native_window", json!({"id": "focus", "width": 800, "height": 500})));
        assert_eq!(edit("Make the PC stats window much bigger").1["size"], "much bigger");
        assert_eq!(edit("Make it borderless"), ("customize_hud_and_windows", json!({"borderless": true, "target": "focus"})));
        assert_eq!(edit("Pin it always on top"), ("customize_hud_and_windows", json!({"always_on_top": true, "target": "focus"})));
        assert_eq!(edit("Add CPU temperature to that window too"), ("update_native_window", json!({"id": "focus", "add_content": ["cpu temperature"]})));
        assert_eq!(edit("Add the processes to the PC stats window").1, json!({"id": "pc stats", "add_content": ["processes"]}));
        // The HUD keeps its own commands.
        assert_eq!(tool("Make yourself bigger"), Some(("configure_interface", json!({"window": "bigger"}))));
        assert_eq!(tool("Make the text bigger"), Some(("configure_interface", json!({"scale": "bigger"}))));
    }

    #[test]
    fn windows_move_across_monitors() {
        let mv = |text: &str| match tool(text) {
            Some(("move_window_to_monitor", a)) => (a["app_or_window_name"].as_str().unwrap_or("").to_string(), a["target_monitor"].clone()),
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(mv("Move Sonora to monitor 2"), ("sonora".into(), json!(2)));
        assert_eq!(mv("Put Discord on my second screen"), ("discord".into(), json!(2)));
        assert_eq!(mv("Move the PC stats window to monitor 1"), ("pc stats window".into(), json!(1)));
        assert_eq!(mv("Move it to the left monitor"), ("it".into(), json!("left")));
        assert_eq!(mv("Verschieb Spotify auf den zweiten Bildschirm"), ("spotify".into(), json!(2)));
        assert_eq!(mv("Move yourself to monitor 2"), ("yourself".into(), json!(2)));
        assert_eq!(tool("Open Spotify on my second monitor"), Some(("place_application_window", json!({"app_name": "spotify", "monitor": 2}))));
        // Snapping stays snapping.
        assert_eq!(tool("Move it to the left").unwrap().1["action"], "snap_left");
    }

    #[test]
    fn music_requests_play_and_control_the_player() {
        let m = |text: &str| match tool(text) {
            Some(("control_music", a)) => a,
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(m("Play my playlist JaysRock"), json!({"action": "play", "playlist_name": "jaysrock"}));
        assert_eq!(m("Spiel meine Playlist Jays Rock"), json!({"action": "play", "playlist_name": "jays rock"}));
        assert_eq!(m("Play the JaysRock playlist"), json!({"action": "play", "playlist_name": "jaysrock"}));
        assert_eq!(m("Play Back in Black by AC/DC"), json!({"action": "play", "song_name": "back in black", "artist_name": "ac dc"}));
        assert_eq!(m("Play the song Thunderstruck"), json!({"action": "play", "song_name": "thunderstruck"}));
        assert_eq!(m("Play something by Queen"), json!({"action": "play", "artist_name": "queen"}));
        assert_eq!(m("Play my playlist JaysRock on Spotify")["player_preference"], "spotify");
        assert_eq!(m("Pause"), json!({"action": "pause"}));
        assert_eq!(m("Resume the music"), json!({"action": "resume"}));
        assert_eq!(m("Next song"), json!({"action": "next_track"}));
        assert_eq!(m("What's playing?"), json!({"action": "what_is_playing"}));
        assert_eq!(m("Was läuft gerade?"), json!({"action": "what_is_playing"}));
        // Games and websites are not music.
        assert!(tool("Play Minecraft").is_none_or(|(n, _)| n != "control_music"));
        assert_eq!(tool("Play Minecraft on YouTube").map(|t| t.0), Some("open_website"));
        // Names Sir's library knows, without saying "playlist" or "song".
        use crate::music::{ItemKind, LibraryItem};
        let item = |kind, name: &str| LibraryItem { kind, name: name.into(), uri: String::new(), artist: String::new(), source: "test" };
        crate::music::set_snapshot(vec![item(ItemKind::Playlist, "Jays Rock"), item(ItemKind::Track, "Back In Black"), item(ItemKind::Playlist, "Jays Focus")]);
        assert_eq!(m("Play Back in Black in JaysRock"), json!({"action": "play", "song_name": "back in black", "playlist_name": "jaysrock"}));
        assert_eq!(m("Play back in black from my Jays Rock playlist"), json!({"action": "play", "song_name": "back in black", "playlist_name": "jays rock"}));
        assert_eq!(m("Play JaysRock"), json!({"action": "play", "playlist_name": "jaysrock"}));
        assert_eq!(m("Play Back in Black"), json!({"action": "play", "song_name": "back in black"}));
        assert!(tool("Start Discord").is_some_and(|(n, _)| n == "open_application"), "start never means music");
    }

    #[test]
    fn protocols_capture_and_engage() {
        let _store = crate::protocols::ProtocolStore::in_memory();
        assert_eq!(
            tool("Jarvis, save my current setup as Gaming Protocol"),
            Some(("manage_stark_protocol", json!({"action": "capture_current_workspace", "protocol_name": "gaming"})))
        );
        assert_eq!(tool("Save this layout as movie night").unwrap().1["protocol_name"], "movie night");
        assert_eq!(tool("Initiate coding protocol"), Some(("manage_stark_protocol", json!({"action": "execute_protocol", "protocol_name": "coding"}))));
        assert_eq!(tool("Run Clean Slate Protocol").unwrap().1["protocol_name"], "clean slate");
        assert_eq!(tool("Engage the Music & Chill Protocol").unwrap().1["protocol_name"], "music and chill");
    }

    #[test]
    fn confirmations_and_reply_claims() {
        for yes in ["Yeah, do that please?", "do it", "Yes please", "Yes", "mach das", "Ja, bitte", "Go ahead", "Sure, Jarvis"] {
            assert!(is_affirmation(yes), "{yes}");
        }
        for not in ["Yes, but what about the weather?", "Do it later maybe tomorrow", "Open Spotify"] {
            assert!(!is_affirmation(not), "{not}");
        }
        assert!(is_negation("No thanks") && is_negation("Nein") && !is_negation("Now open it"));
        assert!(is_termination("Terminate background tasks") && is_termination("Kill it") && !is_termination("Kill time"));
        // What the model said instead of acting.
        let claimed = |reply: &str, request: &str| claimed_action(reply, request);
        assert_eq!(
            claimed("Very good, Sir. I'm creating a new window showing your PC stats.", "can you make me something that shows my pc stats in a window"),
            Some(("spawn_native_window", json!({"live": "pc_stats"})))
        );
        assert_eq!(
            claimed("Certainly, Sir. Opening a new window with the remaining space on your drives.", "yeah do that please").map(|t| t.1["live"].clone()),
            Some(json!("drive_space"))
        );
        assert_eq!(claimed("Moving Discord to monitor 2, Sir.", "could you get discord over to the other screen").map(|t| t.0), Some("move_window_to_monitor"));
        assert_eq!(claimed("You can open a window with your stats by asking me.", "how do I see my stats in a window"), None, "advice is not a claim");
        assert_eq!(
            offered_action("Pardon me, Sir. Shall I open a live telemetry window?"),
            Some(("spawn_native_window", json!({"live": "pc_stats"})))
        );
        assert_eq!(offered_action("Would you like me to move Spotify to monitor 2?").map(|t| t.1["target_monitor"].clone()), Some(json!(2)));
        assert_eq!(offered_action("The weather is fine, Sir."), None);
    }

    #[test]
    fn incomplete_thoughts_are_detected() {
        assert!(looks_incomplete("Set the color to..."));
        assert!(looks_incomplete("Open Spotify and"));
        assert!(looks_incomplete("Ich brauche das, weil"));
        assert!(looks_incomplete("Change it to"));
        assert!(looks_incomplete("Default."));
        assert!(!looks_incomplete("Open Spotify"), "a complete short command");
        assert!(!looks_incomplete("Stop!"));
        assert!(!looks_incomplete("What is the weather like in Berlin today?"));
        assert!(!looks_incomplete("Make the interface green."));
    }

    #[test]
    fn real_questions_go_to_the_llm() {
        for text in [
            "Open the door to a new conversation about physics",
            "Wie ist das Wetter in Berlin?",
            "Create a skill that checks my battery",
            "What do you think about Spotify?",
            "Erinnere mich morgen an das Meeting",
            "Open the file report.pdf and summarise it",
        ] {
            assert!(parse(text).is_none(), "should not fast-path: {text}");
        }
    }

    #[test]
    fn one_command_among_small_talk_is_found() {
        assert_eq!(
            parse_sentences("What's up? Can you change the interface to red?").and_then(|i| match i {
                Intent::Tool { name, args } => Some((name, args)),
                _ => None,
            }),
            Some(("configure_interface", json!({"accent_color": "red"})))
        );
        assert!(parse_sentences("Open Spotify. No wait, don't.").is_none(), "a taken-back command");
        assert!(parse_sentences("Open Spotify. Then open Discord.").is_none(), "two commands are the compound parser's");
        assert!(parse_sentences("Open Spotify").is_none(), "one sentence is parse()'s");
        assert_eq!(tool("Open the interface settings"), Some(("configure_interface", json!({"open_settings": true}))));
        assert_eq!(tool("Einstellungen öffnen"), Some(("configure_interface", json!({"open_settings": true}))));
        assert_eq!(silent_quick_command("Weitermachen.").map(|a| a.args), Some(json!({"action": "resume"})));
    }

    #[test]
    fn misheard_core_routes_to_the_core_sphere_never_a_vehicle() {
        let routed = |heard: &str| parse(&crate::typos::normalize_user_input_typos(heard));
        let close = Some(Intent::Tool { name: "configure_interface", args: json!({"core_view": false}) });
        let open = Some(Intent::Tool { name: "configure_interface", args: json!({"core_view": true}) });
        // The transcript in Sir's screenshot.
        for heard in ["Close your car.", "No. Close your car.", "Right, hide your call.", "Hide your for C-O-R-E.", "Hide your cord", "Jarvis, hide your core", "close your c o r e", "Hide core", "Verstecke deinen Kern"] {
            assert_eq!(routed(heard), close, "{heard}");
        }
        for heard in ["Show your car", "Open your call", "Jarvis, show me your core", "Show your corps", "open your kor", "Zeig mir deinen Kern"] {
            assert_eq!(routed(heard), open, "{heard}");
        }
        // The fast path wins before the layout classifier or the model.
        assert_eq!(hud_view_command("Hide your core"), close);
        assert_eq!(hud_view_command(&crate::typos::normalize_user_input_typos("Close your car.")), close);
        // Pure orb mode is untouched.
        assert!(!matches!(routed("only show the orb"), Some(Intent::Tool { name: "configure_interface", .. })));
    }

    #[test]
    fn walk_me_through_it_narrates_the_board() {
        let walk = Some(Intent::Tool { name: "hud_board", args: json!({"action": "walkthrough"}) });
        for text in ["Walk me through it", "Yes please, put it on screen and walk me through it", "walk me through the report", "go through the board with me"] {
            assert_eq!(parse(text), walk, "{text}");
        }
        assert_eq!(parse("Please present the report."), Some(Intent::Tool { name: "hud_board", args: json!({"action": "present"}) }));
    }

    #[test]
    fn going_out_is_routed_to_the_venue_guide() {
        let venue = parse("Hey Jarvis, where can I take my fiancée for a super serious Halloween night?");
        assert!(matches!(&venue, Some(Intent::Tool { name: "venue_guide", args }) if args["action"] == "recommend"), "{venue:?}");
        assert_eq!(parse("Yeah, okay let's go here."), Some(Intent::Tool { name: "venue_guide", args: json!({"action": "go_here"}) }));
        // Location questions stay location questions.
        assert!(!matches!(parse("Where is the nearest gas station?"), Some(Intent::Tool { name: "venue_guide", .. })));
    }

    #[test]
    fn shadow_memory_and_server_deck_by_voice() {
        let tool = |text: &str| match parse(text) {
            Some(Intent::Tool { name, args }) => Some((name, args)),
            _ => None,
        };
        assert_eq!(tool("What did I say ten minutes ago?"), Some(("shadow_memory", json!({"action": "said_ago", "minutes": 10}))));
        assert_eq!(tool("Was habe ich vor fünf Minuten gesagt?"), Some(("shadow_memory", json!({"action": "said_ago", "minutes": 5}))));
        assert_eq!(tool("Turn my last 5 thoughts into a note block"), Some(("shadow_memory", json!({"action": "to_note", "count": 5}))));
        assert_eq!(tool("search my last half hour for Kaufland"), Some(("shadow_memory", json!({"action": "search", "query": "kaufland"}))));
        assert_eq!(tool("check server play.example.net"), Some(("server_deck", json!({"hosts": ["play.example.net"]}))));
        assert_eq!(tool("is my minecraft server online"), Some(("server_deck", json!({}))));
        assert_eq!(tool("ping vps my.server.com"), Some(("server_deck", json!({"hosts": ["vps:my.server.com"]}))));
    }

    #[test]
    fn discord_call_commands_in_english_and_german() {
        let call = |text: &str| match parse(text) {
            Some(Intent::Tool { name: "discord_call", args }) => args["action"].as_str().map(String::from),
            _ => None,
        };
        assert_eq!(call("Jarvis, listen to my Discord call").as_deref(), Some("listen"));
        assert_eq!(call("Hör im Discord mit").as_deref(), Some("listen"));
        assert_eq!(call("you can talk to them").as_deref(), Some("converse"));
        assert_eq!(call("Du darfst mit ihnen reden").as_deref(), Some("converse"));
        assert_eq!(call("don't talk in the call").as_deref(), Some("quiet"));
        assert_eq!(call("stop listening to the call").as_deref(), Some("off"));
        assert_eq!(call("Hör auf, Discord mitzuhören").as_deref(), Some("off"));
        assert_eq!(call("what's the call status").as_deref(), Some("status"));
        assert_eq!(call("are you listening to the call?").as_deref(), Some("status"), "a question never switches it on");
        assert_eq!(call("Bist du im Call?").as_deref(), Some("status"));
        assert_eq!(call("open Discord"), None, "an app, not the call");
        assert_eq!(call("listen to some music"), None);
    }

    #[test]
    fn colour_requests_retheme_the_whole_hud() {
        // Every colour request, English or German, reaches the fast path and ends on the palette.
        let palette = |text: &str| match parse(text) {
            Some(Intent::Tool { name: "configure_interface", args }) => crate::interface::configure(&crate::interface::InterfaceSettings::default(), &args).map(|d| d.settings.hud_palette).unwrap_or_default(),
            other => panic!("{text}: {other:?}"),
        };
        for (text, want) in [("change your color to red", "crimson_overdrive"), ("Jarvis, make the interface gold", "stark_gold"), ("change your colour to blue", "arc_cyan"), ("Ändere deine Farbe auf Grün", "emerald_matrix"), ("Mach dich lila", "ultraviolet"), ("change your colour to orange", "solar_orange")] {
            assert_eq!(palette(text), want, "{text}");
        }
        // The colour lands on the whole HUD's palette.
        let done = crate::interface::configure(&crate::interface::InterfaceSettings::default(), &json!({"accent_color": "gold"})).unwrap();
        assert_eq!(done.settings.hud_palette, "stark_gold");
        let done = crate::interface::configure(&crate::interface::InterfaceSettings::default(), &json!({"accent_color": "rot"})).unwrap();
        assert_eq!(done.settings.hud_palette, "crimson_overdrive");
        let done = crate::interface::configure(&crate::interface::InterfaceSettings::default(), &json!({"accent_color": "#8B4513"})).unwrap();
        assert_eq!(done.settings.hud_palette, "custom:#8B4513");
        // Presets by name.
        assert_eq!(hud_view_command("switch to the arc cyan theme"), Some(Intent::Tool { name: "configure_interface", args: json!({"hud_palette": "arc_cyan"}) }));
        assert_eq!(hud_view_command("ultraviolet theme"), Some(Intent::Tool { name: "configure_interface", args: json!({"hud_palette": "ultraviolet"}) }));
    }

    #[test]
    fn a_misheard_wake_word_still_reaches_the_hud_command() {
        // Sir's 23:37 voice turns that ended in the fallback line.
        assert_eq!(parse_misheard("Now this opens your settings."), Some(Intent::Tool { name: "configure_interface", args: json!({"open_settings": true}) }));
        assert!(matches!(parse_misheard("Now the scan you fit, change the interface color to red."), Some(Intent::Tool { name: "configure_interface", .. })));
        // Never an app action from a garbled sentence, and nothing from plain chatter.
        assert_eq!(parse_misheard("Now this opens the door to spotify"), None);
        assert_eq!(parse_misheard("well that closes the deal for me"), None);
        assert_eq!(parse_misheard("closes"), None, "one word is no sentence to repair");
        assert_eq!(parse_misheard("I think the weather is nice today"), None);
        assert_eq!(third_person_to_imperative("opens your settings"), "open your settings");
        assert_eq!(third_person_to_imperative("change it"), "change it");
    }

    #[test]
    fn screen_restore_docking_and_detaching_by_voice() {
        let board = |args: Value| Some(Intent::Tool { name: "hud_board", args });
        let iface = |args: Value| Some(Intent::Tool { name: "configure_interface", args });
        for text in ["Jarvis, bring back the screen", "bring the screen back", "restore the screen", "screen back on", "back to the board"] {
            assert_eq!(parse(text), board(json!({"action": "restore"})), "{text}");
        }
        assert_eq!(parse("Dock all windows back inside Jarvis"), iface(json!({"dock_windows": true})));
        assert_eq!(parse("bring all the windows back in"), iface(json!({"dock_windows": true})));
        assert_eq!(parse("Detach the telemetry window"), iface(json!({"detach_window": "telemetry"})));
        assert_eq!(parse("pop out the function plotter"), iface(json!({"detach_window": "function plotter"})));
        assert_eq!(parse("go to the sim lab"), iface(json!({"hud_view": "sim_lab"})));
        assert_eq!(parse("home view"), iface(json!({"hud_view": "home"})));
        // In the HUD by default; an OS window only when he says so.
        assert_eq!(internal_view_command("open my system stats"), Some(json!({"view": "system_stats"})));
        assert_eq!(internal_view_command("open my system stats in a new window"), Some(json!({"view": "system_stats", "separate_window": true})));
        assert_eq!(internal_view_command("open the simulation lab in a separate window"), Some(json!({"view": "simulation_lab", "separate_window": true})));
    }

    #[test]
    fn core_view_and_llm_reboot_by_voice() {
        let open = Some(("configure_interface", json!({"core_view": true})));
        for text in ["Jarvis, show me your core", "Open your core", "Core view", "Zeig mir deinen Kern"] {
            assert_eq!(tool(text), open, "{text}");
        }
        assert_eq!(tool("Close your core"), Some(("configure_interface", json!({"core_view": false}))));
        assert_eq!(tool("Make your core bigger"), Some(("configure_interface", json!({"core_scale": "bigger"}))));
        assert_eq!(tool("Set your core to 200 percent"), Some(("configure_interface", json!({"core_scale": 200.0}))));
        assert_eq!(tool("Rotate your core"), Some(("configure_interface", json!({"core_spin": "faster"}))));
        assert_eq!(tool("Spin faster"), Some(("configure_interface", json!({"core_spin": "faster"}))));
        for text in ["Jarvis, reboot your language model", "Restart LLM", "Reboot brain", "Starte dein Sprachmodell neu"] {
            assert_eq!(tool(text).map(|t| t.0), Some("reboot_language_model"), "{text}");
        }
        // "Restart Spotify" is still an app.
        assert_ne!(tool("Restart Spotify").map(|t| t.0), Some("reboot_language_model"));
    }

    #[test]
    fn text_console_and_windowed_mode_by_voice() {
        let open = Some(("configure_interface", json!({"text_console": true})));
        let close = Some(("configure_interface", json!({"text_console": false})));
        for text in ["Open text interface", "Jarvis, open the text interface.", "Show the text console", "Pop up the text field", "Textkonsole öffnen"] {
            assert_eq!(tool(text), open, "{text}");
        }
        for text in ["Close text interface", "Close the console", "Hide the text console"] {
            assert_eq!(tool(text), close, "{text}");
        }
        for text in ["Windowed mode", "Switch to windowed mode", "Fenstermodus"] {
            assert_eq!(tool(text), Some(("configure_interface", json!({"fullscreen": false}))), "{text}");
        }
    }

    #[test]
    fn silent_quick_commands() {
        let quick = |text: &str| silent_quick_command(text).map(|a| (a.tool, a.args));
        // Stop / pause: everything that plays, at once.
        for text in ["Pause", "Jarvis, pause.", "Stop", "Stopp!", "Halt", "stop music", "Pause the music", "Jarvis stop", "Musik pausieren", "stop playing"] {
            assert_eq!(quick(text), Some(("control_music", json!({"action": "stop_all"}))), "{text}");
            assert!(is_media_stop(text), "{text}");
        }
        assert!(!is_media_stop("Stop the timer") && !is_media_stop("What does stop mean") && !is_media_stop("weiter"));
        assert_eq!(quick("weiter"), Some(("control_music", json!({"action": "resume"}))));
        assert_eq!(quick("Next"), Some(("control_music", json!({"action": "next_track"}))));
        assert_eq!(quick("Mute"), Some(("control_audio_mixer", json!({"action": "mute_master"}))));
        assert_eq!(quick("stumm"), Some(("control_audio_mixer", json!({"action": "mute_master"}))));
        assert_eq!(quick("Unmute"), Some(("control_audio_mixer", json!({"action": "unmute_master"}))));
        assert_eq!(quick("Louder"), Some(("control_audio_mixer", json!({"action": "change_master", "delta": 10}))));
        assert_eq!(quick("leiser"), Some(("control_audio_mixer", json!({"action": "change_master", "delta": -10}))));
        assert_eq!(quick("Volume 40"), Some(("control_audio_mixer", json!({"action": "set_master", "level": 40}))));
        assert_eq!(quick("Lautstärke 30"), Some(("control_audio_mixer", json!({"action": "set_master", "level": 30}))));
        assert_eq!(quick("Orb mode"), Some(("control_orb_mode", json!({"enabled": true}))));
        assert_eq!(quick("Normal mode"), Some(("control_orb_mode", json!({"enabled": false}))));
        assert_eq!(quick("Close"), Some(("configure_interface", json!({"window": "hide"}))));
        assert_eq!(quick("Minimize"), Some(("window_management", json!({"action": "minimize"}))));
        // Whole short commands only.
        assert!(quick("Pause the video I was watching on YouTube please").is_none());
        assert!(quick("What does pause mean").is_none());
        assert!(quick("Play Highway to Hell").is_none());
    }

    #[test]
    fn screen_questions_are_not_calendar_questions() {
        for text in ["What's on my screen right now?", "What do you see?", "Was ist auf meinem Bildschirm?", "Can you look at my screen", "describe my monitor"] {
            assert!(is_screen_question(text), "{text}");
            assert_eq!(upfront_tool(text).map(|(t, _)| t), Some("inspect_screen_or_selection"), "{text}");
        }
        for text in ["What's on my calendar today?", "move Discord to my second screen", "show desktop", "what's playing"] {
            assert!(!is_screen_question(text) || upfront_tool(text).is_none_or(|(t, _)| t != "inspect_screen_or_selection"), "{text}");
        }
    }

    #[test]
    fn darker_reset_and_close_never_misfire() {
        let hud = |text: &str| match tool(text) {
            Some(("configure_interface", args)) => Some(args),
            _ => None,
        };
        assert_eq!(hud("Make it darker"), Some(json!({"background_shade": 0.15})));
        assert_eq!(hud("Make the interface a lot darker"), Some(json!({"background_shade": 0.3})));
        assert_eq!(hud("mach es dunkler"), Some(json!({"background_shade": 0.15})));
        assert_eq!(hud("A bit lighter"), Some(json!({"background_shade": -0.15})));
        for text in ["Change the interface back to default", "Change it back to those default", "back to the defaults", "Reset the interface", "Stell alles wieder auf Standard"] {
            assert_eq!(hud(text), Some(json!({"reset": true})), "{text}");
        }
        // A bare "close" hides the HUD; quitting needs Jarvis named.
        assert_eq!(tool("Close"), Some(("configure_interface", json!({"window": "hide"}))));
        assert_eq!(tool("Close Jarvis").map(|(n, _)| n), Some("shutdown_jarvis"));
        assert_eq!(tool("Close yourself").map(|(n, _)| n), Some("shutdown_jarvis"));
    }

    #[test]
    fn modes_and_the_mixer_by_voice() {
        assert_eq!(tool("Switch to orb mode"), Some(("control_orb_mode", json!({"enabled": true}))));
        assert_eq!(tool("Only show the Orb"), Some(("control_orb_mode", json!({"enabled": true}))));
        assert_eq!(tool("Go minimal orb"), Some(("control_orb_mode", json!({"enabled": true}))));
        let placed = tool("Put the orb in the top right of monitor 2 at 300 pixels").unwrap().1;
        assert_eq!((placed["screen_position"].as_str(), placed["monitor"].as_str(), placed["orb_size_px"].as_f64()), (Some("top-right"), Some("2"), Some(300.0)));
        assert_eq!(placed["enabled"], true);
        assert_eq!(tool("Switch to hands-free mode"), Some(("set_listening_mode", json!({"mode": "hands_free"}))));
        assert_eq!(tool("Freisprechmodus aktivieren"), Some(("set_listening_mode", json!({"mode": "hands_free"}))));
        assert_eq!(tool("Switch to wake word mode"), Some(("set_listening_mode", json!({"mode": "wake_word"}))));
        assert_eq!(tool("Only listen when I say Jarvis"), Some(("set_listening_mode", json!({"mode": "wake_word"}))));
        let mix = tool("Set Sonora to 30% and Discord to 100%").unwrap();
        assert_eq!(mix.0, "control_audio_mixer");
        assert_eq!(mix.1["apps"], json!([{"app": "sonora", "level": 30}, {"app": "discord", "level": 100}]));
        assert_eq!(tool("Mach Spotify auf 20 Prozent").unwrap().1["apps"], json!([{"app": "spotify", "level": 20}]));
        assert_eq!(tool("Mute Chrome"), Some(("control_audio_mixer", json!({"action": "mute_app", "app": "chrome"}))));
        assert_eq!(tool("Lower the volume by 5%"), Some(("control_audio_mixer", json!({"action": "change_master", "delta": -5}))));
        assert_eq!(tool("Set it back to 100%"), Some(("control_audio_mixer", json!({"action": "set_master", "level": 100}))));
        // Not mixer commands.
        assert_ne!(tool("Set the zoom to 125 percent").map(|(n, _)| n), Some("control_audio_mixer"));
        assert_eq!(tool("Mute your voice").unwrap().1["action"], "mute_tts");
    }

    #[test]
    fn a_song_from_my_playlist_is_never_an_artist() {
        let play = tool("Play Thunderstruck from my playlist JaysRock").unwrap();
        assert_eq!(play.0, "control_music");
        assert_eq!((play.1["song_name"].as_str(), play.1["playlist_name"].as_str()), (Some("thunderstruck"), Some("jaysrock")));
        assert!(play.1.get("artist_name").is_none());
        let play = tool("Play Back in Black from my JaysRock playlist").unwrap();
        assert_eq!((play.1["song_name"].as_str(), play.1["playlist_name"].as_str()), (Some("back in black"), Some("jaysrock")));
        assert_eq!(tool("Play Thunderstruck by AC/DC").unwrap().1["artist_name"], "ac dc");
    }

    #[test]
    fn everyday_questions_stay_instant() {
        for text in ["What's the date today?", "What is today's date?", "What day is today?", "Welches Datum ist heute?"] {
            assert_eq!(parse(text), Some(Intent::Date), "{text}");
        }
        for text in ["What reminders do I have?", "Do I have any reminders?", "Which reminders do I have today?", "Show me my reminders"] {
            assert_eq!(tool(text).map(|(n, a)| (n, a["action"].clone())), Some(("reminders", json!("list"))), "{text}");
        }
        for text in ["Which apps are playing audio right now?", "Show me the volume mixer", "What's playing audio?", "Welche Apps spielen gerade Ton?"] {
            assert_eq!(tool(text), Some(("control_audio_mixer", json!({"action": "list"}))), "{text}");
        }
        assert_eq!(tool("Make the accents gold"), Some(("configure_interface", json!({"theme": "gold"}))));
        assert_eq!(tool("Find my jarvis config file").unwrap().1["query"], "jarvis config");
        assert_eq!(tool("Find my tax pdf").unwrap().1["query"], "tax pdf");
        // "What's playing" is the song, not the mixer.
        assert_eq!(tool("What's playing?").map(|(n, _)| n), Some("control_music"));
        // A size alone resizes the orb that is shown; a place means the floating orb.
        assert_eq!(tool("Make the orb 300 pixels"), Some(("control_orb_mode", json!({"orb_size_px": 300.0}))));
    }

    #[test]
    fn jarvis_views_open_as_windows_never_as_programs() {
        let view = |text: &str| internal_view_command(text).and_then(|a| a["view"].as_str().map(String::from));
        // Screenshots 3 and 4, and Whisper's version of the second.
        for text in ["open my system stats in a new window", "Open my system stats, please.", "Open message from stats please.", "show my system stats", "Zeig mir meine Systemstatistik"] {
            assert_eq!(view(text).as_deref(), Some("system_stats"), "{text}");
        }
        assert_eq!(internal_view_command("open my telemetry on monitor 2").unwrap()["monitor"], 2);
        assert_eq!(view("open my calendar").as_deref(), Some("schedule"));
        assert_eq!(view("show my calendar in a new window").as_deref(), Some("schedule"));
        assert_eq!(view("show my calendar"), None, "read out in the chat, not a window");
        assert_eq!(view("open the simulation lab").as_deref(), Some("simulation_lab"));
        assert_eq!(view("show the QR code").as_deref(), Some("mobile_qr"));
        assert_eq!(view("open my gym logs").as_deref(), Some("workout_finance"));
        assert_eq!(view("open my contacts").as_deref(), Some("contacts"));
        // HUD panels and real programs stay what they are.
        for text in ["show the system panel", "hide my system stats", "open discord", "open spotify please", "bring back the system stats"] {
            assert_eq!(view(text), None, "{text}");
        }
        assert_eq!(tool("Open message from stats please.").map(|(n, _)| n), Some("open_jarvis_view"));
        assert_eq!(strip_article("discord for me"), "discord");
        assert_eq!(strip_article("chrome on my pc"), "chrome");
    }

    #[test]
    fn every_way_to_say_the_system_volume() {
        let set = |text: &str| match tool(text) {
            Some(("media_and_volume_control", a)) => a["level"].as_i64(),
            Some(("control_audio_mixer", a)) if a["action"] == "set_master" => a["level"].as_i64(),
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(set("Make my system volume 100%."), Some(100));
        assert_eq!(set("Make my system 100% loudness."), Some(100));
        assert_eq!(set("set volume to 40%"), Some(40));
        assert_eq!(set("mach die lautstärke auf 40"), Some(40));
        assert_eq!(set("set the master volume to 25 percent"), Some(25));
        assert_eq!(mixer_command("turn sonora down to 20%"), Some(json!({"action": "set_apps", "apps": [{"app": "sonora", "level": 20}]})));
        assert_eq!(mixer_command("mach discord auf 80 prozent"), Some(json!({"action": "set_apps", "apps": [{"app": "discord", "level": 80}]})));
        assert_eq!(mixer_command("mute chrome"), Some(json!({"action": "mute_app", "app": "chrome"})));
        // Reflex words.
        for (text, action) in [("paus", "stop_all"), ("weiter", "resume"), ("skip", "next_track"), ("zurück", "previous_track")] {
            assert_eq!(silent_quick_command(text).map(|a| a.args["action"].clone()), Some(json!(action)), "{text}");
        }
        for (text, action) in [("silence", "mute_master"), ("stumm", "mute_master"), ("ton an", "unmute_master"), ("lauter", "change_master"), ("leiser", "change_master")] {
            assert_eq!(silent_quick_command(text).map(|a| a.args["action"].clone()), Some(json!(action)), "{text}");
        }
    }
}
