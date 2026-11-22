//! Instant-command fast path (< 1 ms, no LLM round-trip) for obvious requests in English,
//! German or Denglisch: stop, quitting JAI, mute/volume, media keys, restyling the HUD,
//! open/close apps, window actions, time/date and system status. Everything else goes to the
//! LLM with native tool calling.

use crate::interface::{self, HudLayout, WindowCommand};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// "Stop", "Sei still", "Halt" – silence JAI, nothing else.
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
        .filter(|w| !matches!(*w, "jai" | "please" | "bitte" | "mal" | "now" | "jetzt" | "sir" | "the" | "die" | "den" | "das"))
        .collect();
    kept.join(" ")
}

/// "Pause", "mute", "louder", "volume 40", "lautstärke 30", "next", "orb mode" …: carried out in
/// milliseconds without speech or the language model. Only whole, short utterances qualify.
/// How long "go on" brings back what a stop cut off (Sir, 2026-11-22: stopped mid-sentence,
/// "continue" or the like goes on with it).
const RESUME_WITHIN: std::time::Duration = std::time::Duration::from_secs(120);

static GO_ON: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:jai|jay|ok|okay|sorry|yes|yeah|alright|right|ja|gut|na gut|so)\s+)*(?:(?:you can|you may|please|bitte|du kannst|now)\s+)?(?:continue|go on|go ahead|keep going|carry on|resume|go on talking|keep talking|continue talking|finish what you were saying|finish your sentence|what were you saying|where were we|weiter|mach weiter|weitermachen|weiter machen|sprich weiter|red weiter|rede weiter|erzähl weiter|fahr fort|fahre fort|weiterreden)(?:\s+(?:please|bitte|then|now|jetzt|sir|jai|jay|talking|where you left off|from where you stopped))*$")
});

/// "Continue", "go on", "weiter" right after Sir stopped JAI mid-sentence: what was cut off is
/// wanted, not the music (so the media keys and the wake-word gate let it through to `parse`).
pub fn resumes_speech(text: &str) -> bool {
    resumes(text, crate::tts::was_cut(RESUME_WITHIN))
}

fn resumes(text: &str, cut: bool) -> bool {
    cut && GO_ON.is_match(&normalize(text))
}

pub fn silent_quick_command(text: &str) -> Option<SilentAction> {
    if resumes_speech(text) {
        return None;
    }
    let s = quick_words(text);
    if s.is_empty() || s.split_whitespace().count() > 4 {
        return None;
    }
    let act = |tool: &'static str, args: Value, label: &str| Some(SilentAction { tool, args, label: label.to_string() });
    let music = |action: &str, label: &str| act("control_music", json!({ "action": action }), label);
    let mixer = |args: Value, label: &str| act("control_audio_mixer", args, label);
    // "More", "a bit less", "same for Firefox", while a volume change is fresh.
    if let Some(args) = crate::audio_mixer::last_step().and_then(|last| volume_follow_up(last, &s)) {
        let label = args["delta"].as_i64().map_or_else(|| "Volume".to_string(), |d| format!("{d:+} %"));
        return mixer(args, &label);
    }
    static VOLUME: LazyLock<Regex> =
        LazyLock::new(|| re(r"^(?:set\s+)?(?:volume|lautstärke|lautstaerke|sound|ton)\s+(?:to\s+|auf\s+|at\s+)?(\d{1,3})\s*(?:%|percent|prozent)?$|^(\d{1,3})\s*(?:%|percent|prozent)$"));
    if let Some(c) = VOLUME.captures(&s) {
        let level: u64 = c.get(1).or(c.get(2))?.as_str().parse().ok()?;
        let level = level.min(100);
        return mixer(json!({ "action": "set_master", "level": level }), &format!("{level} %"));
    }
    // "Volume forty", "Lautstärke fünfzig": the level as Whisper writes it when it spells it out.
    static VOLUME_WORDS: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:set\s+)?(?:volume|lautstärke|lautstaerke|sound|ton)\s+(?:to\s+|auf\s+|at\s+)?([a-zäöüß]+(?:[\s-][a-zäöüß]+)?)(?:\s+(?:percent|prozent))?$"));
    if let Some(level) = VOLUME_WORDS.captures(&s).and_then(|c| spoken_level(&c[1])) {
        return mixer(json!({ "action": "set_master", "level": level }), &format!("{level} %"));
    }
    match s.as_str() {
        // Whatever is playing — every media session at once — and JAI's own voice.
        "pause" | "pause music" | "pause music please" | "pausieren" | "pausiere" | "musik pause" | "musik pausieren" | "pause it" | "pause playback"
        | "pause everything" | "pause all" | "paus" | "pos" | "pause musik" | "pausiere musik" | "musik anhalten" | "halte musik an" => music("stop_all", "Paused"),
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
        "louder" | "lauter" | "volume up" | "mach lauter" => mixer(json!({ "action": "change_master", "delta": 10 }), "+10 %"),
        "quieter" | "leiser" | "volume down" | "mach leiser" | "softer" => {
            mixer(json!({ "action": "change_master", "delta": -10 }), "−10 %")
        }
        // ("It" is what the last request was about; the mixer takes the master when there was none.)
        "turn it up" => mixer(json!({ "action": "change_app", "app": "it", "delta": 10 }), "+10 %"),
        "turn it down" | "keep it down" => mixer(json!({ "action": "change_app", "app": "it", "delta": -10 }), "−10 %"),
        "a bit louder" | "a little louder" => mixer(json!({ "action": "change_master", "delta": 5 }), "+5 %"),
        "a bit quieter" | "a little quieter" => mixer(json!({ "action": "change_master", "delta": -5 }), "−5 %"),
        // A maximised board block goes back to the board first.
        "minimize" | "minimise" | "minimieren" if crate::hud_board::spotlight_active() => {
            act("hud_board", json!({ "action": "minimize" }), "Back to the board")
        }
        "minimize" | "minimise" | "minimieren" => act("window_management", json!({ "action": "minimize" }), "Minimised"),
        // What is up on his board goes first; only with nothing up is it the HUD itself that
        // goes to the tray. (Seen live 2026-11-12: "close" with the briefing on the board sent
        // the whole HUD away.)
        "close" | "schließen" | "schliessen" => {
            let (tool, args, label) = close_alone(crate::hud_board::is_active() || !crate::hud_dock::open_blocks().is_empty());
            act(tool, args, label)
        }
        "orb mode" | "orb" | "orb only" | "only orb" | "orbmodus" | "orb modus" | "pure orb" => act("control_orb_mode", json!({ "enabled": true }), "Orb mode"),
        "normal mode" | "full mode" | "show chat" | "full view" | "normaler modus" | "normalmodus" | "exit orb mode" => {
            act("control_orb_mode", json!({ "enabled": false }), "Full HUD")
        }
        _ => None,
    }
}

/// What a bare "close" does: with something of his up on the board, that goes; with nothing up,
/// the HUD itself goes to the tray.
fn close_alone(something_up: bool) -> (&'static str, Value, &'static str) {
    if something_up {
        ("close_all_secondary_windows", json!({}), "Closed")
    } else {
        ("configure_interface", json!({ "window": "hide" }), "To the tray")
    }
}

/// The short answers to how a volume change sounds, said right after it (`last`): the same way
/// again ("more", "a bit more", "noch mehr"), a little back ("less", "too much"), past where it
/// was ("the other way"), or the same to another app ("same for Firefox", "and Discord too").
/// `s`: `quick_words`. None for anything else.
fn volume_follow_up(last: crate::audio_mixer::Step, s: &str) -> Option<Value> {
    static SAME_FOR: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:and\s+|und\s+|also\s+|auch\s+)?(?:do\s+|mach\s+)?(?:same|that|it|this|dasselbe|gleiche|selbe)\s+(?:for|with|to|für|fuer|bei|mit)\s+(.{3,24}?)(?:\s+too|\s+as\s+well|\s+auch)?$|^(?:and|und|also|auch)\s+(.{3,24}?)(?:\s+too|\s+as\s+well|\s+auch)?$|^(.{3,24}?)\s+(?:too|as\s+well|auch)$")
    });
    // (A level has no way of its own: more of it is louder.)
    let way = last.delta.map_or(1, i64::signum);
    let moved = last.delta.is_some() || last.level.is_some();
    let step = |delta: i64| Some(if last.app { json!({ "action": "change_app", "app": "it", "delta": delta }) } else { json!({ "action": "change_master", "delta": delta }) });
    match s {
        "more" | "again" | "once more" | "same again" | "keep going" | "mehr" | "noch mehr" | "nochmal" | "noch mal" | "noch einmal" if moved => step(way * last.delta.map_or(10, i64::abs)),
        "bit more" | "a bit more" | "little more" | "a little more" | "slightly more" | "noch etwas" | "noch ein bisschen" | "noch ein wenig" | "ein bisschen mehr" | "etwas mehr" if moved => step(way * 5),
        "much more" | "a lot more" | "lot more" | "way more" | "viel mehr" | "noch viel mehr" if moved => step(way * 20),
        "less" | "bit less" | "a bit less" | "little less" | "a little less" | "too much" | "weniger" | "etwas weniger" | "ein bisschen weniger" | "zu viel" if moved => step(-way * 5),
        "other way" | "other way round" | "other way around" | "wrong way" | "opposite" | "andersrum" | "anders herum" | "andere richtung" => step(-2 * last.delta?),
        // Bare directions mean the volume only right after one moved.
        "up" | "higher" | "hoch" | "höher" | "rauf" if moved => step(10),
        "down" | "lower" | "runter" | "tiefer" if moved => step(-10),
        "up a bit" | "a bit up" | "bit up" | "a little up" | "up a little" | "etwas hoch" | "etwas höher" | "ein bisschen höher" if moved => step(5),
        "down a bit" | "a bit down" | "bit down" | "a little down" | "down a little" | "etwas runter" | "etwas tiefer" | "ein bisschen tiefer" if moved => step(-5),
        // Louder or quieter after an app's change: that app, not the master volume.
        "louder" | "lauter" | "mach lauter" | "mach es lauter" if last.app => step(10),
        "quieter" | "leiser" | "mach leiser" | "mach es leiser" | "softer" if last.app => step(-10),
        "a bit louder" | "a little louder" | "bit louder" | "etwas lauter" | "ein bisschen lauter" if last.app => step(5),
        "a bit quieter" | "a little quieter" | "bit quieter" | "etwas leiser" | "ein bisschen leiser" if last.app => step(-5),
        _ => {
            let c = SAME_FOR.captures(s)?;
            let app = app_list(c.iter().skip(1).flatten().next()?.as_str().trim())?;
            Some(match (last.delta, last.level, last.mute) {
                (Some(delta), ..) => json!({ "action": "change_app", "app": app, "delta": delta }),
                (None, Some(level), _) => json!({ "action": "set_apps", "apps": app.split(" and ").map(|a| json!({ "app": a, "level": level })).collect::<Vec<_>>() }),
                (None, None, Some(mute)) => json!({ "action": if mute { "mute_app" } else { "unmute_app" }, "app": app }),
                (None, None, None) => return None,
            })
        }
    }
}

/// "Stop", "pause", "stopp", "halt", "stop music", "JAI, pause" (and Whisper's "post" / "paws",
/// already corrected to "pause"): stop everything that is playing, at once and silently.
pub fn is_media_stop(text: &str) -> bool {
    // (Not "stop the music on Max's PC": that is Max's PC's to stop.)
    crate::fleet::command(text).is_none() && silent_quick_command(text).is_some_and(|a| a.tool == "control_music" && a.args["action"] == "stop_all")
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

/// The tools a garbled sentence may still reach (`parse_misheard`): JAI's own HUD, board, views
/// and media — never an app launch or close, which a misheard sentence must not trigger.
const MISHEARD_SAFE_TOOLS: &[&str] = &[
    "configure_interface", "control_jai_interface", "hud_board", "open_jai_view", "control_orb_mode", "media_and_volume_control", "control_music", "set_listening_mode",
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

/// JAI's own UI commands behind a garbled opening — the wake word misheard as "now this",
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

/// "Switch to hands-free mode", "Freisprechmodus aktivieren", "only listen when I say JAI":
/// `set_listening_mode` arguments.
pub fn listening_mode_command(text: &str) -> Option<Value> {
    // (His name as Sir says it, where it is the sentence's object: "... when I say Jay".)
    let s = raw_words(text).split_whitespace().map(|w| if w == "jay" { "jai" } else { w }).collect::<Vec<_>>().join(" ");
    let hands_free = ["hands free mode", "handsfree mode", "hands free", "handsfree", "freisprechmodus", "freisprech modus", "listen to everything", "always listen", "hör immer zu", "immer zuhören"];
    let wake = ["wake word mode", "wakeword mode", "wake word", "only listen when i say jai", "only when i say jai", "only respond when i say jai", "only listen to jai", "aktivierungswort", "nur wenn ich jai sage", "weckwort"];
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
    // "Deactivate orb mode", "turn orb mode off", "get out of orb mode", "Orbmodus beenden"
    // (2026-11-07 "deactivate Orb mode" switched it on).
    static ORB_OFF: LazyLock<Regex> = LazyLock::new(|| {
        re(r"\b(?:exit|leave|quit|get out of|come out of|out of|no more|verlasse|verlass|beende|beend)\b.{0,24}\b(?:orb|kugel)|\b(?:deactivate|disable|turn off|switch off|shut off|stop|end|cancel|close|deaktiviere|deaktivier)\b.{0,24}\b(?:orb ?mod(?:e|us)|pure orb|orb only)|\b(?:orb ?mod(?:e|us)|pure orb|orb only)\b.{0,16}\b(?:off|aus|beenden|deaktivieren|verlassen)\b")
    });
    let off = ORB_OFF.is_match(&s);
    if !off
        && ["orb mode", "orb modus", "orbmodus", "only show the orb", "only the orb", "just the orb", "minimal orb", "pure orb", "orb only", "nur den orb", "nur noch den orb", "nur die kugel"]
            .iter()
            .any(|p| s.contains(p))
    {
        args.insert("enabled".into(), json!(true));
    }
    if off {
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
    if let Some(call) = mix_command(text) {
        return Some(call);
    }
    // "No, don't, don't mute it, make it 50 % louder": what Sir does not want is said first;
    // the request is the last clause.
    let clauses: Vec<&str> = text.split([',', ';', '.', '!', '?']).map(str::trim).filter(|c| !c.is_empty()).collect();
    if let [refused @ .., wanted] = clauses.as_slice() {
        static REFUSAL: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:no|nope|nah|nein|not|nicht|wait|stop|don'?t|do not|doch nicht)\b"));
        if !refused.is_empty() && refused.iter().all(|c| REFUSAL.is_match(&c.to_lowercase().replace('’', "'"))) {
            return mixer_command(wanted);
        }
    }
    // Lower-case words with the per cent sign kept ("30%").
    let s = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '%' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // "Which apps are playing audio?", "read the mixer": read the real mixer (never guess).
    static LIST: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:jai )?(?:(?:which|what) (?:apps?|programs?|applications?) (?:are|is) (?:playing|making|using) (?:any )?(?:audio|sound|noise)(?: right now| now| at the moment)?|(?:what s|whats|what is) playing (?:any )?(?:audio|sound|noise)|(?:what s|whats|what is|who s|whos|who is) making (?:that |this |the |so much |all that )?(?:sound|noise)|(?:read|check)(?: me)? (?:the |my )?(?:volume |audio )?mixer|(?:what are )?(?:the |my )?app volumes|welche apps (?:spielen|machen|nutzen) (?:gerade )?(?:audio|ton|sound)|was spielt (?:gerade |hier |da )?(?:ton|sound|audio)(?: ab)?|(?:was|wer) macht (?:hier |da |gerade )*(?:so einen |solchen |diesen )?(?:lärm|krach|ton|geräusche?|sound))$")
    });
    if LIST.is_match(&s) {
        return Some(json!({ "action": "list" }));
    }
    // A possessive is no part of a name ("my browser's volume" is the browser's).
    let s = s.split(' ').filter(|w| *w != "s").collect::<Vec<_>>().join(" ");
    // "Mute Discord for 10 minutes", "schalte Discord für 10 Minuten stumm": the mute, and when
    // it ends by itself. (Anything else with a time in it is not this.)
    static FOR_A_WHILE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(.+?)\s+(?:for|für|fuer)\s+((?:half\s+an|a|an|one|two|three|five|ten|fifteen|twenty|thirty|\d{1,3}|eine|einer|einen|zwei|drei|fünf|fuenf|zehn|fünfzehn|zwanzig|dreißig|dreissig|(?:eine\s+)?halbe)\s+(?:seconds?|minutes?|mins?|hours?|sekunden?|minuten?|stunden?))(\s+(?:stumm|stummschalten|aus))?$")
    });
    if let Some(c) = FOR_A_WHILE.captures(&s) {
        let base = format!("{}{}", &c[1], c.get(3).map_or("", |m| m.as_str()));
        let call = mixer_command(&base).or_else(|| MUTE.is_match(&base).then(|| json!({ "action": "mute_master" })));
        if let (Some(mut call), Some(span)) = (call.filter(|call| matches!(call["action"].as_str(), Some("mute_app" | "mute_master"))), crate::listening_lock::duration_in(&c[2])) {
            call["seconds"] = json!(span.as_secs());
            return Some(call);
        }
    }
    // "Which audio device am I using?", "where is the sound going?": read, like the levels.
    static READ_DEVICES: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:what|what is|whats|which)\s+(?:is\s+)?(?:my\s+|the\s+)?(?:audio\s+|sound\s+|output\s+)+(?:device|devices|output)?(?:\s+(?:am\s+i\s+using|do\s+i\s+use|is\s+active|is\s+in\s+use|do\s+i\s+have|are\s+there))?$|^list\s+(?:my\s+|the\s+)?(?:audio\s+|sound\s+|output\s+)+devices?$|^where\s+(?:is\s+)?(?:the\s+|my\s+)?(?:sound|audio)\s+(?:going|coming\s+out|playing)$|^(?:welche|welches)\s+(?:audio|ton|sound|ausgabe)\s*(?:gerät|geräte)(?:\s+(?:nutze\s+ich|benutze\s+ich|ist\s+aktiv|habe\s+ich))?$|^wo\s+(?:geht|kommt|läuft)\s+(?:der\s+|mein\s+)?(?:ton|sound)(?:\s+(?:hin|raus))?$")
    });
    if READ_DEVICES.is_match(&s) {
        // ("List my audio devices" wants them all, "which device am I using?" the one.)
        let all = s.contains("devices") || s.contains("geräte");
        return Some(json!({ "action": if all { "devices" } else { "device" } }));
    }
    // "Switch to my speakers": the one thing about the sound that is not his to do, so he says so
    // instead of leaving it to a model that would agree.
    static SWITCH_DEVICE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:switch|change)\s+(?:the\s+|my\s+)?(?:output|output\s+device|audio\s+device|audio\s+output|sound\s+output|sound\s+device)(?:\s+to\s+.{2,30})?$|^(?:switch|change|move|put|send|route)\s+(?:the\s+|my\s+)?(?:sound|audio)\s+(?:to|over\s+to|through)\s+(?:the\s+|my\s+)?(?:speakers?|headset|headphones?|boxes|monitors?|tv|bluetooth)$|^(?:switch\s+to|switch\s+over\s+to|use)\s+(?:the\s+|my\s+)(?:speakers?|headset|headphones?)$|^(?:wechsel|wechsle|schalte|schalt|stell|stelle|leg|lege)\s+(?:den\s+ton\s+)?(?:auf|zu|um\s+auf)\s+(?:die\s+|den\s+|das\s+|meine\s+|mein\s+|meinen\s+)?(?:kopfhörer|lautsprecher|headset|boxen)(?:\s+um)?$")
    });
    if SWITCH_DEVICE.is_match(&s) {
        return Some(json!({ "action": "switch_device" }));
    }
    // "What's my volume?", "how loud is Discord?", "is the browser muted?": read off the mixer,
    // never guessed. (`s`: "what's" lost its s above.)
    static READ_MASTER: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:what|what is|whats)\s+(?:the\s+|my\s+)?(?:current\s+|system\s+|master\s+|pc\s+)*volume(?:\s+(?:level|at|right now|now))?$|^how\s+loud\s+(?:is\s+)?(?:it|the\s+pc|my\s+pc|the\s+volume)(?:\s+(?:right now|now))?$|^wie\s+laut\s+(?:ist\s+)?(?:es|der\s+pc|mein\s+pc|die\s+lautstärke)(?:\s+gerade|\s+jetzt)?$|^wie\s+hoch\s+ist\s+die\s+lautstärke$|^(?:was|wie)\s+ist\s+(?:die|meine)\s+(?:aktuelle\s+)?lautstärke$")
    });
    if READ_MASTER.is_match(&s) {
        return Some(json!({ "action": "get_master" }));
    }
    static READ_MUTED: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:is\s+)?(?:anything|something)\s+muted$|^what(?:\s+is)?\s+muted$|^was\s+ist\s+(?:gerade\s+)?stumm(?:geschaltet)?$|^(?:ist\s+)?(?:etwas|irgendwas)\s+stumm(?:geschaltet)?$|^is\s+(?:the\s+|my\s+)?(?:sound|audio|volume)\s+(?:on|off|muted)$|^is\s+(?:it|everything|the\s+pc|my\s+pc)\s+muted$|^ist\s+(?:der\s+|mein\s+)?(?:ton|sound)\s+(?:an|aus|stumm(?:geschaltet)?)$"));
    if READ_MUTED.is_match(&s) {
        return Some(json!({ "action": "muted" }));
    }
    // "Make it quiet": a level to go down to (never up to), not a step. ("It's very quiet" is an
    // observation, and a different sentence.)
    static MAKE_QUIET: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:make|turn)\s+(?:it|this|that|everything|the\s+sound|the\s+volume)\s+(?:(very|really|super|real)\s+)?quiet$|^(very|really|super)\s+quiet$|^(?:mach|mache|stell|stelle)\s+(?:es|das|alles|den\s+ton)\s+(?:(ganz|sehr|richtig)\s+)?leise$|^(ganz|sehr)\s+leise$")
    });
    if let Some(c) = MAKE_QUIET.captures(&s) {
        return Some(json!({ "action": "quiet", "level": if c.iter().skip(1).flatten().next().is_some() { 10 } else { 20 } }));
    }
    static MAKE_LOUD: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:make|turn)\s+(?:it|this|that|everything|the\s+sound|the\s+volume)\s+(?:(very|really|super|real)\s+)?loud$|^(?:mach|mache|stell|stelle)\s+(?:es|das|alles|den\s+ton)\s+(?:(ganz|sehr|richtig)\s+)?laut$"));
    if let Some(c) = MAKE_LOUD.captures(&s) {
        return Some(json!({ "action": "loud", "level": if c.iter().skip(1).flatten().next().is_some() { 100 } else { 80 } }));
    }
    // "It's very quiet", "das ist laut": how it sounds, so one step the other way.
    static HEARD: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:it|its|it is|this is|that is|that|thats|das ist|es ist)\s+(?:(?:very|so|really|super|sehr|echt|richtig)\s+)?(loud|laut|quiet|leise)$"));
    if let Some(c) = HEARD.captures(&s) {
        return Some(json!({ "action": "change_app", "app": "it", "delta": if matches!(&c[1], "loud" | "laut") { -10 } else { 10 } }));
    }
    // "Not loud enough", "Discord isn't loud enough", "das ist nicht leise genug": the same.
    static NOT_ENOUGH: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:it|this|that|thats|es|das)\s+)?(?:is\s+|ist\s+)?(?:still\s+|immer\s+noch\s+)?(?:not|isn\s+t|isnt|nicht)\s+(loud|laut|quiet|leise)\s+(?:enough|genug)$")
    });
    static APP_NOT_ENOUGH: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:the\s+|my\s+|der\s+|die\s+|das\s+|mein\s+|meine\s+)?([a-z][a-z0-9äöüß ]{1,24}?)\s+(?:is\s+|ist\s+)?(?:still\s+|immer\s+noch\s+)?(?:not|isn\s+t|isnt|nicht)\s+(loud|laut|quiet|leise)\s+(?:enough|genug)$")
    });
    let louder = |word: &str| if matches!(word, "loud" | "laut") { 10 } else { -10 };
    if let Some(c) = NOT_ENOUGH.captures(&s) {
        return Some(json!({ "action": "change_app", "app": "it", "delta": louder(&c[1]) }));
    }
    // "Make it not so loud", "mach den Ton nicht so laut": a step the other way.
    static NOT_SO: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:make|mach|mache)\s+(?:it|this|that|the\s+sound|es|das|den\s+ton)\s+(?:not\s+so|nicht\s+so)\s+(loud|laut|quiet|leise)$")
    });
    if let Some(c) = NOT_SO.captures(&s) {
        return Some(json!({ "action": "change_app", "app": "it", "delta": -louder(&c[1]) }));
    }
    if let Some(c) = APP_NOT_ENOUGH.captures(&s) {
        if let Some(app) = app_list(c[1].trim()) {
            return Some(json!({ "action": "change_app", "app": app, "delta": louder(&c[2]) }));
        }
    }
    // "Why is it so quiet?", "warum ist Discord stumm?": the levels are the answer, and a
    // question moves nothing.
    static WHY: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^why\s+is\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)\s+(?:so\s+)?(?:quiet|low|silent|loud|muted)$|^warum\s+ist\s+(?:der\s+|die\s+|das\s+|mein\s+|meine\s+)?([a-z][a-z0-9äöüß ]{1,24}?)\s+(?:so\s+)?(?:leise|laut|stumm)$")
    });
    if let Some(c) = WHY.captures(&s) {
        let named = c.iter().skip(1).flatten().next()?.as_str().trim();
        if matches!(named, "it" | "everything" | "sound" | "audio" | "volume" | "es" | "alles" | "ton" | "sound gerade") {
            return Some(json!({ "action": "list" }));
        }
        if let Some(app) = app_list(named) {
            return Some(json!({ "action": "get_app", "app": app }));
        }
    }
    // "I can barely hear it", "ich höre Discord kaum": there is sound, just not enough of it.
    static BARELY: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:i\s+)?(?:can\s+)?(?:barely|hardly|scarcely)\s+hear\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24})$|^ich\s+(?:höre?|hoere?)\s+(?:den\s+|die\s+|das\s+|mein\s+|meine\s+|meinen\s+)?([a-z0-9äöüß ]{2,24}?)\s+kaum(?:\s+noch)?$|^ich\s+kann\s+(?:den\s+|die\s+|das\s+|mein\s+|meine\s+|meinen\s+)?([a-z0-9äöüß ]{2,24}?)\s+kaum\s+(?:noch\s+)?(?:hören|hoeren|verstehen)$")
    });
    if let Some(c) = BARELY.captures(&s) {
        let named = c.iter().skip(1).flatten().next()?.as_str().trim();
        if matches!(named, "it" | "that" | "this" | "es" | "anything") {
            return Some(json!({ "action": "change_app", "app": "it", "delta": 10 }));
        }
        if let Some(app) = app_list(named) {
            return Some(json!({ "action": "change_app", "app": app, "delta": 10 }));
        }
    }
    // "Make Firefox as loud as Discord", "mach Firefox leiser als Discord": a level read off the
    // other app, not a guess.
    static SAME_AS: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:set|make|put|turn|bring)\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)\s+(?:to\s+|at\s+|on\s+)?(?:the\s+)?same\s+(?:level|volume)\s+as\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24})$|^(?:make|turn|set)\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)\s+(?:just\s+)?as\s+(?:loud|quiet)\s+as\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24})$|^(?:mach|mache|stell|stelle)\s+(?:den\s+|die\s+|das\s+|mein\s+|meine\s+|meinen\s+)?([a-z][a-z0-9äöüß ]{1,24}?)\s+(?:genauso|so|gleich)\s+(?:laut|leise)\s+wie\s+(?:den\s+|die\s+|das\s+|mein\s+|meine\s+|meinen\s+)?([a-z][a-z0-9äöüß ]{1,24})$")
    });
    static THAN: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:make|turn|set)\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)\s+(louder|quieter|softer)\s+than\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24})$|^(?:mach|mache|stell|stelle)\s+(?:den\s+|die\s+|das\s+|mein\s+|meine\s+|meinen\s+)?([a-z][a-z0-9äöüß ]{1,24}?)\s+(lauter|leiser)\s+als\s+(?:den\s+|die\s+|das\s+|mein\s+|meine\s+|meinen\s+)?([a-z][a-z0-9äöüß ]{1,24})$")
    });
    let compared: Option<(String, String, i64)> = if let Some(c) = SAME_AS.captures(&s) {
        let named: Vec<&str> = c.iter().skip(1).flatten().map(|m| m.as_str().trim()).collect();
        Some((named[0].to_string(), named[1].to_string(), 0))
    } else {
        THAN.captures(&s).map(|c| {
            let named: Vec<&str> = c.iter().skip(1).flatten().map(|m| m.as_str().trim()).collect();
            (named[0].to_string(), named[2].to_string(), if matches!(named[1], "louder" | "lauter") { 10 } else { -10 })
        })
    };
    if let Some((app, other, offset)) = compared {
        if let (Some(app), Some(other)) = (app_list(&app), app_list(&other)) {
            return Some(json!({ "action": "match_app", "app": app, "to": other, "offset": offset }));
        }
    }
    // "Fade the music out", "fade in Discord", "blende die Musik aus": over a few seconds.
    // "Turn it down slowly": a step taken the same way.
    static FADE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^fade\s+(out|in)(?:\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}))?$|^fade\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)\s+(?:back\s+)?(out|in)$|^(?:blende|blend)\s+(?:die\s+|den\s+|das\s+|meine\s+|mein\s+)?([a-z0-9äöüß ]{2,24}?)\s+(?:wieder\s+)?(aus|ein)$")
    });
    static SLOWLY: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^slowly\s+turn\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)\s+(up|down)$|^turn\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)\s+(up|down)\s+(?:slowly|gradually|bit\s+by\s+bit)$|^(?:dreh|drehe|mach|mache)\s+(?:die\s+|den\s+|das\s+|meine\s+|mein\s+)?([a-z0-9äöüß ]{2,24}?)\s+langsam\s+(runter|hoch|leiser|lauter)$")
    });
    let is_way = |g: &str| matches!(g, "out" | "in" | "aus" | "ein" | "up" | "down" | "runter" | "hoch" | "leiser" | "lauter");
    if let Some(c) = FADE.captures(&s).or_else(|| SLOWLY.captures(&s)) {
        let groups: Vec<&str> = c.iter().skip(1).flatten().map(|m| m.as_str().trim()).collect();
        let way = groups.iter().copied().find(|g| is_way(g))?;
        let named = groups.iter().copied().find(|g| !is_way(g)).unwrap_or("it");
        let app = match named {
            "it" | "that" | "this" | "es" | "das" => Some(Some("it".to_string())),
            "everything" | "alles" | "sound" | "audio" | "volume" | "ton" | "lautstärke" => Some(None),
            other => app_list(other).map(Some),
        };
        if let Some(app) = app {
            let mut call = match way {
                "out" | "aus" => json!({ "action": "fade", "way": "out" }),
                "in" | "ein" => json!({ "action": "fade", "way": "in" }),
                "up" | "hoch" | "lauter" => json!({ "action": "fade", "delta": 20 }),
                _ => json!({ "action": "fade", "delta": -20 }),
            };
            if let Some(app) = app {
                call["app"] = json!(app);
            }
            return Some(call);
        }
    }
    static READ_APP: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^how\s+loud\s+is\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24})$|^(?:what is|whats|what)\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)\s+(?:volume|level)(?:\s+at)?$|^(?:what is|whats|what|where is|where)\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)\s+(?:at|set\s+to)$|^is\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)\s+muted$|^wie\s+laut\s+ist\s+(?:der\s+|die\s+|das\s+|mein\s+|meine\s+)?([a-z][a-z0-9äöüß ]{1,24})$|^ist\s+(?:der\s+|die\s+|das\s+|mein\s+|meine\s+)?([a-z][a-z0-9äöüß ]{1,24}?)\s+stumm$")
    });
    if let Some(c) = READ_APP.captures(&s) {
        let named = c.iter().skip(1).flatten().next()?.as_str().trim();
        if let Some(app) = app_list(named) {
            return Some(json!({ "action": "get_app", "app": app }));
        }
    }
    // "I can't hear anything", "there's no sound", "ich höre nichts": what stands between him and
    // the sound is looked for, not guessed at. (`s`: "can't" is "can t", "there's" lost its s.)
    static NO_SOUND: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:i\s+)?(?:can\s+t|cant|cannot|can\s+not|couldn\s+t|don\s+t|dont|do\s+not)\s+hear\s+(?:anything|a\s+thing|any\s+sound|any\s+audio|sound|audio|the\s+sound|the\s+audio|you)(?:\s+(?:at\s+all|any\s*more|right\s+now|now))?$|^i\s+hear\s+nothing$|^why\s+(?:can\s+t|cant|don\s+t|dont)\s+i\s+hear\s+anything$|^(?:why\s+is\s+there\s+|there\s+is\s+|there\s+|i\s+have\s+|i\s+got\s+|we\s+have\s+)?no\s+(?:sound|audio)(?:\s+(?:at\s+all|any\s*more|right\s+now|now))?$|^(?:the\s+|my\s+)?(?:sound|audio)\s+(?:is\s+)?(?:gone|broken|not\s+working|isn\s+t\s+working|isnt\s+working|stopped|stopped\s+working|doesn\s+t\s+work|doesnt\s+work|does\s+not\s+work|died)$|^(?:ich\s+)?(?:höre?|hoere?)\s+nichts(?:\s+mehr)?$|^ich\s+kann\s+nichts\s+(?:mehr\s+)?hören$|^warum\s+(?:höre?|hoere?)\s+ich\s+nichts$|^(?:es\s+gibt\s+|da\s+ist\s+|ich\s+habe\s+|ich\s+hab\s+|warum\s+ist\s+)?kein(?:en)?\s+(?:ton|sound)(?:\s+(?:da|mehr))?$|^(?:der\s+|mein\s+)?(?:ton|sound)\s+(?:ist\s+weg|ist\s+kaputt|geht\s+nicht(?:\s+mehr)?|funktioniert\s+nicht(?:\s+mehr)?)$")
    });
    if NO_SOUND.is_match(&s) {
        return Some(json!({ "action": "no_sound" }));
    }
    static NO_SOUND_FROM: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:i\s+)?(?:can\s+t|cant|cannot|can\s+not|couldn\s+t|don\s+t|dont|do\s+not)\s+hear\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)(?:\s+(?:at\s+all|any\s*more|right\s+now|now))?$|^no\s+(?:sound|audio)\s+(?:from|on|in)\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24})$|^(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)\s+has\s+no\s+(?:sound|audio)$|^ich\s+(?:höre?|hoere?)\s+(?:den\s+|die\s+|das\s+|mein\s+|meine\s+|meinen\s+)?([a-z0-9äöüß ]{2,24}?)\s+nicht(?:\s+mehr)?$|^ich\s+kann\s+(?:den\s+|die\s+|das\s+|mein\s+|meine\s+|meinen\s+)?([a-z0-9äöüß ]{2,24}?)\s+nicht\s+(?:mehr\s+)?hören$")
    });
    if let Some(c) = NO_SOUND_FROM.captures(&s) {
        let named = c.iter().skip(1).flatten().next()?.as_str().trim();
        if let Some(app) = app_list(named) {
            return Some(json!({ "action": "no_sound", "app": app }));
        }
    }
    static BY: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(lower|reduce|decrease|drop|raise|increase|turn up|turn down)\s+(?:the\s+)?(?:system\s+|master\s+)?(?:volume|sound|lautstärke)\s+by\s+(\d{1,3})(?:\s*(?:%|percent|prozent))?|(?:volume|sound|lautstärke)\s+(up|down)\s+by\s+(\d{1,3})(?:\s*(?:%|percent|prozent))?)$")
    });
    if let Some(c) = BY.captures(&s) {
        let n: i64 = c.get(2).or(c.get(4))?.as_str().parse().ok()?;
        let down = c.get(1).map(|m| matches!(m.as_str(), "lower" | "reduce" | "decrease" | "drop" | "turn down")).unwrap_or_else(|| c.get(3).is_some_and(|m| m.as_str() == "down"));
        return Some(json!({ "action": "change_master", "delta": if down { -n } else { n } }));
    }
    static BACK: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:set|put|turn)\s+(it|the volume|the sound|volume)\s+(?:back\s+)?(?:to|on)\s+(?:a\s+)?(\d{1,3})\s*(?:%|percent|prozent)?$"));
    if let Some(c) = BACK.captures(&s) {
        let n: u64 = c[2].parse().ok()?;
        // "It" is what the last volume request was about (the browser he just turned down);
        // the mixer takes the master volume when there was none.
        return Some(if &c[1] == "it" { json!({ "action": "set_app", "app": "it", "level": n.min(100) }) } else { json!({ "action": "set_master", "level": n.min(100) }) });
    }
    // "Make everything loud again", "put the volume back", "restore the volume", "mach alles
    // wieder laut": every level the mixer changed goes back to what it was.
    static RESTORE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:and |now |ok |okay |jai )*(?:make|turn|set|put|get)\s+(?:everything|it all|all of it|the volume|the sound|all the volumes|the volumes)\s+(?:loud|normal|back)(?:\s+again)?(?:\s+to\s+(?:normal|how it was|where it was))?|(?:everything|volume|sound|audio|all)\s+back\s+to\s+normal|(?:restore|reset|undo)\s+(?:the\s+|my\s+|all\s+|all the\s+)?(?:volume|volumes|sound|audio|mixer|that volume|the volume)(?:\s+(?:change|changes|levels?))?|(?:put|set|turn)\s+(?:the\s+|all\s+|all the\s+)?(?:volume|volumes|sound)\s+back(?:\s+to\s+(?:normal|how it was|where it was))?|(?:mach|mache|stell|stelle)\s+(?:alles|die lautstärke)\s+wieder\s+(?:laut|normal|zurück|her)|lautstärke\s+(?:zurücksetzen|wiederherstellen))$")
    });
    if RESTORE.is_match(&s) {
        return Some(json!({ "action": "restore" }));
    }
    // "Make Discord loud again", "Discord wieder laut": the same for one app.
    static RESTORE_APP: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:make|turn|set|put|get|bring)\s+)?(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)\s+(?:loud\s+again|back\s+to\s+normal|normal\s+again)$|^(?:(?:mach|mache|stell|stelle)\s+)?(?:den\s+|die\s+|das\s+|mein\s+|meine\s+|meinen\s+)?([a-z][a-z0-9äöüß ]{1,24}?)\s+wieder\s+(?:laut|normal)$")
    });
    if let Some(c) = RESTORE_APP.captures(&s) {
        if let Some(app) = app_list(c.iter().skip(1).flatten().next()?.as_str().trim()) {
            return Some(json!({ "action": "restore", "app": app }));
        }
    }
    // "Mute everything except Discord", "silence all but the browser", "alles außer Discord
    // stumm": every other app's own volume is muted (the master volume is left alone).
    static OTHERS: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:mute|silence|stumm)\s+(?:everything|everyone|all|all apps|all other apps|every app|alles)\s+(?:except|but|except for|other than|apart from|besides|außer|ausser|bis auf)\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,40}?)$|^(?:alles|alle)\s+(?:außer|ausser|bis auf)\s+([a-z][a-z0-9 ]{1,40}?)\s+(?:stumm|stummschalten|muten)$|^(?:(?:mach|mache|schalte|schalt|stell|stelle)\s+)?alles\s+(?:stumm|aus)\s+(?:außer|ausser|bis auf)\s+(?:den\s+|die\s+|das\s+|mein\s+|meine\s+|meinen\s+)?([a-z][a-z0-9 ]{1,40}?)$|^(?:only|nur)\s+([a-z][a-z0-9 ]{1,24}?)(?:\s+(?:audio|sound|ton))$|^solo\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24})$")
    });
    if let Some(c) = OTHERS.captures(&s) {
        let names = c.iter().skip(1).flatten().next()?.as_str().trim();
        if app_list(names).is_some() {
            return Some(json!({ "action": "mute_others", "app": names }));
        }
    }
    // Per-application: "<app> to N %" pairs, or "mute / unmute <app>".
    static PAIR: LazyLock<Regex> = LazyLock::new(|| re(r"([a-z][a-z0-9 ]{1,24}?)\s+(?:to|auf|at)\s+(\d{1,3})\s*(?:%|percent|prozent)"));
    let apps: Vec<Value> = PAIR
        .captures_iter(&s)
        .flat_map(|c| {
            let raw = c[1].trim().to_string();
            let level: u64 = c[2].parse().unwrap_or(0);
            // "Turn Sonora down to 20 %", "mach Discord leiser auf 30 Prozent": the direction is
            // no name. "Discord and Spotify to 40 %": both get the level; "… and Discord to
            // 100 %" after another pair: the app right before the level.
            let named: Vec<String> = raw
                .split(" and ")
                .flat_map(|p| p.split(" und "))
                .map(|part| {
                    part.split_whitespace()
                        .filter(|w| {
                            !matches!(
                                *w,
                                "set" | "make" | "mach" | "mache" | "stell" | "stelle" | "put" | "turn" | "bring" | "the" | "and" | "und" | "volume" | "of" | "lautstärke" | "von"
                                    | "down" | "up" | "runter" | "hoch" | "leiser" | "lauter" | "die" | "den" | "my" | "meine" | "mein"
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .filter(|name| !name.is_empty())
                .collect();
            // The app right before the level must be one; the ones before it share its level.
            let ok = named.last().is_some_and(|last| crate::audio_mixer::is_app_name(last));
            named.into_iter().filter(move |name| ok && crate::audio_mixer::is_app_name(name)).map(move |name| json!({ "app": name, "level": level.min(100) })).collect::<Vec<_>>()
        })
        .collect();
    if !apps.is_empty() {
        return Some(json!({ "action": "set_apps", "apps": apps }));
    }
    static MUTE_APP: LazyLock<Regex> = LazyLock::new(|| re(r"^(mute|unmute|stumm schalten|stummschalten)\s+([a-z][a-z0-9 ]{1,24}?)(?:\s+until\s+i\s+(?:say\s+so|tell\s+you(?:\s+otherwise)?))?$|^([a-z][a-z0-9 ]{1,24}?)\s+(stumm\s+schalten|stummschalten|stumm|muten|unmuten)$"));
    if let Some(c) = MUTE_APP.captures(&s) {
        let verb = c.get(1).or(c.get(4))?.as_str();
        // ("Mute my browser", "mute Discord and Spotify": the apps as the mixer knows them.)
        if let Some(app) = app_list(c.get(2).or(c.get(3))?.as_str().trim()) {
            let mute = !verb.starts_with("unmute");
            return Some(json!({ "action": if mute { "mute_app" } else { "unmute_app" }, "app": app }));
        }
    }
    volume_phrase(&s)
}

/// "Save this mix as gaming", "load my gaming mix", "which mixes do I have", "delete the gaming
/// mix", and a saved mix's name alone ("gaming mix"). A playlist is played or put on, never
/// loaded: "put on my classic rock mix" stays the music's.
fn mix_command(text: &str) -> Option<Value> {
    static SAVE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:save|store|remember|keep|speicher|speichere|merk dir)\s+(?:this|that|the|my|these|diesen|den|die|meinen)?\s*(?:current\s+|aktuellen\s+)?(?:audio\s+|sound\s+|volume\s+)?(?:mix|volumes?|levels?|mischung|lautstärken?)\s+(?:as|called|named|als|unter)\s+(?:my\s+|the\s+|mein\s+|meinen\s+)?([a-z0-9äöüß][a-z0-9äöüß ]{0,30}?)(?:\s+mix)?$")
    });
    static LOAD: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:load|apply|use|switch to|change to|go to|activate|lade|lad|nimm|aktiviere|wechsel zu|wechsle zu)\s+(?:my\s+|the\s+|mein\s+|meinen\s+|den\s+|die\s+|zum\s+|zu\s+)?([a-z0-9äöüß][a-z0-9äöüß ]{0,30}?)\s+(?:mix|mischung)$")
    });
    static LIST: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:which|what)\s+(?:audio\s+|sound\s+|volume\s+)?mixes\s+(?:do i have|have i (?:got|saved)|are (?:there|saved)|do we have)$|^(?:list|show)\s+(?:me\s+)?(?:my\s+|the\s+)?(?:saved\s+|audio\s+)?mixes$|^welche\s+(?:mixe|mischungen)\s+(?:habe ich|hab ich|gibt es)$|^(?:zeig|zeige)\s+(?:mir\s+)?(?:meine\s+)?(?:mixe|mischungen)$")
    });
    static DELETE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:delete|remove|forget|erase|lösche|lösch)\s+(?:my\s+|the\s+|mein\s+|meinen\s+|den\s+|die\s+)?([a-z0-9äöüß][a-z0-9äöüß ]{0,30}?)\s+(?:mix|mischung)$")
    });
    static ALONE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:my\s+|the\s+)?([a-z0-9äöüß][a-z0-9äöüß ]{0,30}?)\s+mix$"));
    // (Cheap for everything else: the word is there or none of these can match.)
    let lower = text.to_lowercase();
    if !lower.contains("mix") && !lower.contains("mischung") && !lower.contains("lautstärken") && !(lower.contains(" as ") && (lower.contains("volumes") || lower.contains("levels"))) {
        return None;
    }
    let s = normalize(text);
    if LIST.is_match(&s) {
        return Some(json!({ "action": "list_mixes" }));
    }
    if let Some(c) = SAVE.captures(&s) {
        return Some(json!({ "action": "save_mix", "name": c[1].trim() }));
    }
    if let Some(c) = LOAD.captures(&s) {
        return Some(json!({ "action": "load_mix", "name": c[1].trim() }));
    }
    if let Some(c) = DELETE.captures(&s) {
        return Some(json!({ "action": "delete_mix", "name": c[1].trim() }));
    }
    let alone = ALONE.captures(&s)?;
    crate::audio_mixes::find(&alone[1]).map(|mix| json!({ "action": "load_mix", "name": mix.name }))
}

/// The apps a phrase names ("discord", "the browser and spotify", "discords"): each one known
/// to the mixer, as Sir would say them. None when any part is no app.
fn app_list(phrase: &str) -> Option<String> {
    let mut names: Vec<String> = Vec::new();
    for part in phrase.split(" and ").flat_map(|p| p.split(" und ")) {
        let part = part.trim().trim_start_matches("the ").trim_start_matches("my ").trim();
        // Whisper writes the possessive without its apostrophe ("discords volume"): a name
        // Sir uses for an app first, as it stands or without that s; then any installed app.
        let forms = [Some(part), part.strip_suffix('s')];
        let name = forms.into_iter().flatten().find(|n| crate::audio_mixer::is_alias(n)).or_else(|| forms.into_iter().flatten().find(|n| crate::audio_mixer::is_app_name(n)))?;
        names.push(name.to_string());
    }
    (!names.is_empty()).then(|| names.join(" and "))
}

/// A level in a volume request: "50", "50%", "fifty", "one hundred", "max", "half". (level,
/// the words it took.)
fn level_at(words: &[&str]) -> Option<(i64, usize)> {
    let first = *words.first()?;
    if let Ok(n) = first.trim_end_matches('%').parse::<i64>() {
        return (first.len() <= 4 && (0..=999).contains(&n)).then_some((n.min(100), 1));
    }
    if let [a, b, ..] = words {
        if matches!(*a, "one" | "a" | "ein" | "eine") && matches!(*b, "hundred" | "hundert") {
            return Some((100, 2));
        }
        if let Some(n) = spoken_level(&format!("{a} {b}")) {
            return Some((n as i64, 2));
        }
    }
    match first {
        "einhundert" => Some((100, 1)),
        "zero" | "null" | "minimum" | "min" => Some((0, 1)),
        // ("One" to "nine" alone are too easily something else: "turn one down".)
        _ => spoken_level(first).filter(|n| *n >= 10).map(|n| (n as i64, 1)),
    }
}

/// One volume request in Sir's words, whatever their order: "lower my browser's volume by 50 %",
/// "make Discord's volume 50 %", "makes system volume one hundred percent", "turn the music
/// down a bit", "make it 50 % louder", "mute it", "mach Discord 20 Prozent leiser". `s`: lower
/// case words, per cent signs kept. None for anything that is no volume request or names no app
/// the mixer knows (the brightness, the lights, a timer).
fn volume_phrase(s: &str) -> Option<Value> {
    let mut words: Vec<&str> = s.split_whitespace().collect();
    // What the older rules of `parse` answer stays theirs (the plain master volume).
    if words.len() > 14 || [&*VOLUME_SET, &*VOLUME_SET_REV, &*VOLUME_SET_PC, &*VOLUME_UP, &*VOLUME_DOWN, &*MUTE, &*UNMUTE].iter().any(|r| r.is_match(s)) {
        return None;
    }
    // Lead-ins: "and you make …", "can you …", "I want you to …", "kannst du mal …".
    const LEAD: &[&str] = &["jai", "hey", "ok", "okay", "please", "can", "could", "would", "will", "you", "and", "now", "then", "also", "just", "yes", "well", "so", "i", "want", "need", "id", "like", "bitte", "kannst", "könntest", "du", "und", "dann", "mal", "ich", "will", "möchte"];
    while words.first().is_some_and(|w| LEAD.contains(w) || (*w == "to" && words.len() > 2)) {
        words.remove(0);
    }
    // A question reads the mixer (above) or is none of this: "is the sound on?" unmuted it.
    if matches!(words.first().copied(), Some("is" | "isnt" | "are" | "ist" | "sind" | "does" | "did" | "why" | "warum" | "wieso" | "what" | "whats" | "which" | "how" | "wie" | "was" | "when" | "wann" | "who" | "wer")) {
        return None;
    }
    // "It's too loud", "Discord is way too quiet", "das ist zu leise": one step the other way.
    let judged = words.windows(2).position(|w| matches!(w[0], "too" | "zu") && matches!(w[1], "loud" | "laut" | "quiet" | "low" | "soft" | "leise")).map(|at| {
        let loud = matches!(words[at + 1], "loud" | "laut");
        words.drain(at..at + 2);
        if loud {
            -1
        } else {
            1
        }
    });
    // "It's a bit quiet", "das ist ziemlich laut": a smaller step (the "bit" says so below).
    let judged = judged.or_else(|| {
        let at = words.windows(2).position(|w| matches!(w[0], "bit" | "little" | "tad" | "kinda" | "etwas" | "bisschen" | "ziemlich" | "quite" | "rather") && matches!(w[1], "loud" | "laut" | "quiet" | "leise"))?;
        let loud = matches!(words[at + 1], "loud" | "laut");
        words.remove(at + 1);
        Some(if loud { -1 } else { 1 })
    });
    // "Turn it all the way up", "dreh die Musik ganz runter": the end of the scale ("ganz leise"
    // stays just audible).
    let mut floor = 0;
    let mut extreme = false;
    if let Some(at) = words.windows(3).position(|w| w == ["all", "the", "way"]) {
        words.drain(at..at + 3);
        extreme = true;
    } else if let Some(at) = words.iter().position(|w| *w == "ganz") {
        match words.get(at + 1).copied() {
            Some("leise") => {
                floor = 10;
                words[at + 1] = "runter";
            }
            Some("laut") => words[at + 1] = "hoch",
            _ => {}
        }
        words.remove(at);
        extreme = true;
    }
    // "Set every volume back to 100 %", "all volumes to 50", "every app to 80": each app's own
    // level (the master stays where it is).
    let every = words.windows(2).any(|w| {
        matches!(w[0], "every" | "each" | "all" | "alle" | "jede" | "jedes" | "jeder")
            && matches!(w[1], "volume" | "volumes" | "app" | "apps" | "program" | "programs" | "programme" | "lautstärke" | "lautstärken")
    }) || words.windows(3).any(|w| w[0] == "all" && w[1] == "the" && matches!(w[2], "volumes" | "apps" | "programs"));
    if every {
        words.retain(|w| !matches!(*w, "every" | "each" | "jede" | "jedes" | "jeder" | "app" | "apps" | "program" | "programs" | "programme"));
    }
    let mut level: Option<i64> = None;
    let (mut relative, mut absolute) = (false, false);
    let mut direction = 0i64;
    let mut step: Option<i64> = None;
    let mut mute: Option<bool> = None;
    let (mut cue, mut turn, mut percent) = (false, false, false);
    // "Turn off the sound", "turn the sound back on": said of the sound, a mute in other words.
    let mut switched: Option<bool> = None;
    // "Dreh die Musik auf": without a level, the German verb for up.
    let mut auf = false;
    // ("Crank", "blast off" on their own are about no sound.)
    let mut cranked = false;
    let mut target: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        match w {
            "volume" | "volumes" | "sound" | "audio" | "lautstärke" | "lautstaerke" | "ton" | "loudness" | "systemlautstärke" | "gesamtlautstärke" => cue = true,
            "mute" | "silence" | "stumm" | "stummschalten" | "muten" | "lautlos" => mute = Some(true),
            "unmute" | "unmuten" | "entstummen" => mute = Some(false),
            "louder" | "lauter" | "higher" | "höher" | "raise" | "increase" | "boost" | "erhöhe" | "erhöh" | "hoch" => {
                direction = 1;
                cue |= matches!(w, "louder" | "lauter");
            }
            "quieter" | "leiser" | "softer" | "reduce" | "decrease" | "drop" | "runter" | "verringere" | "senke" | "senk" => {
                direction = -1;
                cue |= matches!(w, "quieter" | "leiser" | "softer");
            }
            // "Lower the browser" (a verb) and "50 % lower" (after an amount) both mean down.
            "lower" | "niedriger" => direction = -1,
            // ("Bring up the browser" names no volume and no turn: it does not qualify below.)
            "up" | "down" => direction = if w == "up" { 1 } else { -1 },
            "turn" | "dreh" | "drehe" | "switch" | "schalte" | "schalt" => turn = true,
            // "Crank the music", "pump up the volume", "blast it": up, unless he says down.
            "crank" | "pump" | "blast" => {
                turn = true;
                cranked = true;
                if direction == 0 {
                    direction = 1;
                }
            }
            "by" | "um" => relative = true,
            "to" | "at" => absolute = true,
            "on" => {
                absolute = true;
                switched = Some(true);
            }
            "auf" => {
                absolute = true;
                auf = true;
            }
            "off" | "aus" => switched = Some(false),
            "an" => switched = Some(true),
            "bit" | "little" | "slightly" | "tad" | "touch" | "etwas" | "bisschen" | "wenig" | "leicht" => step = Some(5),
            "lot" | "much" | "way" | "far" | "viel" | "deutlich" => step = Some(20),
            "%" | "percent" | "prozent" | "points" | "punkte" => percent = true,
            "max" | "maximum" | "full" | "voll" | "volle" | "maximal" if level.is_none() => level = Some(100),
            "half" | "halb" | "halbe" | "hälfte" if level.is_none() => level = Some(50),
            // Words that carry nothing here.
            "set" | "make" | "makes" | "put" | "bring" | "change" | "adjust" | "get" | "keep" | "have" | "mach" | "mache" | "machen" | "stell" | "stelle" | "stellen" | "setz" | "setze" | "the" | "my"
            | "your" | "a" | "die" | "der" | "das" | "den" | "dem" | "mein" | "meine" | "meinen" | "of" | "for" | "von" | "für" | "again" | "back" | "level" | "all" | "alle" | "more"
            | "noch" | "wieder" | "bitte" | "please" | "ein" | "eine" | "is" | "ist" | "be" | "its" | "s" | "in" | "even" | "still" => {}
            _ => match level_at(&words[i..]).filter(|_| level.is_none()) {
                Some((n, took)) => {
                    level = Some(n);
                    percent |= w.ends_with('%');
                    i += took - 1;
                }
                None => target.push(w),
            },
        }
        i += 1;
    }
    let named = target.join(" ");
    // "Everything" is the master volume, and so are all the apps.
    let everything = every || matches!(named.as_str(), "everything" | "alles" | "apps" | "programs" | "programme");
    // "Discord to half", "put Discord at 50": a level for an app, by a name Sir uses for it.
    let alias = crate::audio_mixer::is_alias(&named) || named.strip_suffix('s').is_some_and(crate::audio_mixer::is_alias);
    percent |= absolute && level.is_some() && alias;
    if auf && turn && level.is_none() && direction == 0 {
        direction = 1;
    }
    // "Too loud", "dreh auf" name nothing: they are about what the last request was about.
    // ("Even louder", "noch leiser": louder than what was just changed.)
    let compared = direction != 0 && words.iter().any(|w| matches!(*w, "even" | "noch" | "still"));
    let about_last = named.is_empty() && (compared || (!cue && !cranked && (judged.is_some() || (turn && direction != 0))));
    // "Music up", "Discord etwas runter", "Spotify 30": the name and the way or the level, and
    // nothing else. (Not a small digit: "Discord 2" is more likely a name.)
    let said = words.iter().filter(|w| !matches!(**w, "the" | "my" | "die" | "meine" | "a" | "bit" | "little" | "etwas" | "bisschen" | "ein" | "more" | "noch" | "much" | "lot" | "viel")).count();
    let bare = alias && (direction != 0 || level.is_some_and(|l| l >= 10)) && said == target.len() + 1;
    if let Some(way) = judged {
        direction = way;
        cue = true;
    }
    turn |= extreme;
    if cue && mute.is_none() && level.is_none() && direction == 0 {
        mute = switched.map(|on| !on);
    }
    // A volume request names the volume, a loudness or a mute, or gives something a share
    // ("Sonora 30 %") or a turn ("turn Discord down"). Anything else is none, and is not worth
    // looking a name up in the list of installed apps for (that costs a millisecond).
    if !(cue || mute.is_some() || percent || bare || (turn && direction != 0) || (direction != 0 && (level.is_some() || everything)) || (everything && absolute && level.is_some())) {
        return None;
    }
    // …and asks for something: a level, a step or a mute ("turn off the sound cues" does not).
    if level.is_none() && direction == 0 && mute.is_none() {
        return None;
    }
    // Whom it is about.
    let app: Option<String> = match named.as_str() {
        "" if about_last => Some("it".to_string()),
        // ("My PC's volume" arrives as "pcs".)
        "" | "system" | "master" | "pc" | "pcs" | "computer" | "computers" | "systems" | "laptop" | "laptops" | "overall" | "main" | "windows" | "everything" | "rechner" | "gesamt" | "alles" | "whole" | "whole system" | "whole pc" | "apps" | "programs" | "programme" => None,
        "it" | "that" | "this" | "them" | "those" | "es" | "das" | "ihn" | "sie" => Some("it".to_string()),
        other => Some(app_list(other)?),
    };
    // A volume request names the volume, a loudness or a mute, or gives an app a share ("Sonora
    // 30 %") or a turn ("turn Discord down").
    let qualifies = cue || mute.is_some() || everything || bare || (app.is_some() && (percent || (turn && direction != 0) || (direction != 0 && level.is_some())));
    if !qualifies {
        return None;
    }
    if let Some(on) = mute {
        return Some(match app {
            Some(app) => json!({ "action": if on { "mute_app" } else { "unmute_app" }, "app": app }),
            None => json!({ "action": if on { "mute_master" } else { "unmute_master" } }),
        });
    }
    // The end of the scale is a level, not a step.
    if extreme && direction != 0 {
        level = Some(if direction > 0 { 100 } else { floor });
        direction = 0;
    }
    // A step: "by 50 %", "50 % louder", "a bit quieter", "turn it down".
    if direction != 0 && (relative || !absolute || level.is_none()) && !(absolute && level.is_some()) {
        let delta = direction * level.or(step).unwrap_or(10);
        return Some(match app {
            Some(app) => json!({ "action": "change_app", "app": app, "delta": delta }),
            None => json!({ "action": "change_master", "delta": delta }),
        });
    }
    let level = level?;
    if relative && direction == 0 {
        // "Volume by 20" says no direction.
        return None;
    }
    Some(match app {
        Some(app) if app == "it" => json!({ "action": "set_app", "app": "it", "level": level }),
        Some(app) => json!({ "action": "set_apps", "apps": app.split(" and ").map(|a| json!({ "app": a, "level": level })).collect::<Vec<_>>() }),
        // "Set all apps to 100 %" is about their own levels: the master would jump with it.
        None if every || matches!(named.as_str(), "apps" | "programs" | "programme") => json!({ "action": "set_all_apps", "level": level }),
        None => json!({ "action": "set_master", "level": level }),
    })
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
    re(r"^(?:(?:set|start|create|make)\s+(?:a\s+|an\s+)?)?(?:(?P<n1>[a-z]+)\s+)?(?:timer|countdown)\s+(?:for\s+|of\s+)?(\d+|a|an|one|two|three|four|five|ten|fifteen|twenty|thirty|forty|fifty|sixty)\s+(seconds?|minutes?|mins?|hours?)$|^(?:(?:set|start)\s+)?(?:a\s+|an\s+)?(\d+)[\s-]*(second|minute|min|hour)s?\s+(?:(?P<n2>[a-z]+)\s+)?(?:timer|countdown)$|^(?:stell|stelle|starte|start)\s+(?:einen\s+|ein\s+)?(?:(?P<n3>[a-zäöüß]+?)[\s-]?)?(?:timer|countdown)\s+(?:auf|für|fuer|von)\s+(\d+|eine|einen|zwei|drei|fünf|zehn|fünfzehn|zwanzig|dreißig)\s+(sekunden?|minuten?|stunden?)$|^timer\s+(\d+)\s+(sekunden?|minuten?|stunden?|seconds?|minutes?)$")
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
/// "Copy the text of this window", "kopier den Text vom Bildschirm": read by OCR onto the clipboard.
static COPY_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:copy (?:the |all the )?text (?:of|from|in|on) (?:this|the|my) (window|screen|monitor)|kopier(?:e)? (?:den |alle[nr]? )?text (?:aus dem|aus diesem|vom|von diesem|auf dem|auf meinem) (fenster|bildschirm|monitor))$")
});
static NETWORK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:am i online|are we online|are we connected|is (?:my |the )?internet (?:working|down|up|ok|okay)|check (?:my |the )?(?:internet|connection|network|wifi|wlan)(?: connection)?|hows? my (?:internet|connection|network|wifi)|how is my (?:internet|connection|network)|what(?:s| is) my (?:public )?ip(?: address)?|ping|(?:(?:show|check|give|tell)(?: me)? (?:the |my )?|(?:whats|what is) (?:the |my )?)?(?:network|internet|connection) status|bin ich online|sind wir online|geht (?:mein |das )?internet|internet check|(?:zeig(?:e)?(?: mir)? (?:den |meinen )?)?netzwerkstatus|wie ist meine verbindung|wie ist mein internet)$")
});
static PROCESSES: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:whats slowing (?:down )?my (?:pc|computer|system|machine|laptop)(?: down)?|what is slowing (?:down )?my (?:pc|computer|system|machine|laptop)(?: down)?|why is my (?:pc|computer|system|machine|laptop) (?:so )?(?:slow|laggy|sluggish)|which (?:app|program|process|application)s? (?:uses|use|is using|are using) (?:the most |so much )?(?:cpu|memory|ram|resources)(?: the most)?|what(?:s| is) using (?:my |all my |so much |the most )?(?:cpu|memory|ram|resources)|top processes|show (?:me )?the top processes|was (?:verbraucht|frisst|bremst) (?:so viel )?(?:ram|cpu|speicher|leistung|meinen pc)|warum ist mein (?:pc|rechner|computer|laptop) so langsam)$")
});
/// "List my skills", "what skills have you learned", "welche Skills hast du": read out.
static LIST_SKILLS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:list|show|read|tell)(?: me)? (?:all )?(?:my |your |the )?(?:learned |custom )?skills|(?:what|which) (?:learned |custom )?skills (?:do (?:i|you) have|have you learned|have you got|did you learn|are (?:there|installed))|(?:zeig|zeige)(?: mir)? (?:alle )?(?:deine |meine |die )?skills|welche skills (?:hast du|habe ich|kennst du|gibt es))$")
});
/// "What are my protocols?", "list my protocols", "welche Protokolle habe ich": read out.
static LIST_PROTOCOLS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:list|show|read|tell)(?: me)? (?:all )?(?:my |your |the )?(?:stark )?protocols|(?:what|which) (?:stark )?protocols (?:do (?:i|you) have|are there|have i got|can you run)|what are (?:my|your|the) (?:stark )?protocols|(?:zeig|zeige)(?: mir)? (?:alle )?(?:meine |deine |die )?protokolle|welche protokolle (?:habe ich|hast du|gibt es))$")
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
    re(r"\b(?:look at|check|read|scan|what(?:s| is) on|whats happening on|what (?:can|do) you see on|what(?:s| is) (?:visible|shown|showing|open) on|summari[sz]e(?: what(?:s| is) on)?|describe|schau (?:dir )?|schau auf|was ist auf|was passiert auf|was siehst du auf|lies)\s+((?:the\s+|my\s+|den\s+|meinen\s+|dem\s+|meinem\s+)?(?:(?:first|second|third|other|left|right|primary|main|erste[nm]?|zweite[nm]?|dritte[nm]?|andere[nm]?|linke[nm]?|rechte[nm]?|haupt)\s*(?:monitor|screen|display|bildschirm)|(?:monitor|screen|display|bildschirm)\s*(?:\d|one|two|three|eins|zwei|drei)))\b")
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

/// A level from 0 to 100 spoken as words, English or German: "forty", "forty five",
/// "twenty-five", "fünfzig", "hundert". None for anything that is not a number.
fn spoken_level(words: &str) -> Option<u64> {
    let unit = |w: &str| -> Option<u64> {
        Some(match w {
            "zero" | "null" => 0,
            "one" | "eins" => 1,
            "two" | "zwei" => 2,
            "three" | "drei" => 3,
            "four" | "vier" => 4,
            "five" | "fünf" => 5,
            "six" | "sechs" => 6,
            "seven" | "sieben" => 7,
            "eight" | "acht" => 8,
            "nine" | "neun" => 9,
            _ => return None,
        })
    };
    let tens = |w: &str| -> Option<u64> {
        Some(match w {
            "ten" | "zehn" => 10,
            "eleven" | "elf" => 11,
            "twelve" | "zwölf" => 12,
            "fifteen" | "fünfzehn" => 15,
            "twenty" | "zwanzig" => 20,
            "thirty" | "dreißig" | "dreissig" => 30,
            "forty" | "fourty" | "vierzig" => 40,
            "fifty" | "fünfzig" => 50,
            "sixty" | "sechzig" => 60,
            "seventy" | "siebzig" => 70,
            "eighty" | "achtzig" => 80,
            "ninety" | "neunzig" => 90,
            "hundred" | "hundert" | "full" | "max" | "maximum" => 100,
            "half" | "halb" => 50,
            _ => return None,
        })
    };
    let parts: Vec<&str> = words.split([' ', '-']).filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [one] => tens(one).or_else(|| unit(one)),
        // "Forty five": a round ten and a unit.
        [ten, one] => match (tens(ten), unit(one)) {
            (Some(t), Some(u)) if (20..=90).contains(&t) && t % 10 == 0 => Some(t + u),
            _ => None,
        },
        _ => None,
    }
}

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
    // "a 10 minute pasta timer", "stell einen Nudeltimer auf 10 Minuten": the timer's own name.
    let named = ["n1", "n2", "n3"].iter().find_map(|n| c.name(n)).filter(|n| !matches!(n.as_str(), "a" | "an" | "the" | "my" | "new" | "another" | "kitchen" | "egg" | "countdown" | "den" | "der" | "die" | "das" | "ein" | "einen" | "mein" | "meinen" | "neuen"));
    let groups: Vec<&str> = c.iter().skip(1).flatten().filter(|m| !["n1", "n2", "n3"].iter().any(|n| c.name(n).is_some_and(|g| g.range() == m.range()))).map(|m| m.as_str()).collect();
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
    let (id, label) = match named {
        Some(name) => (format!("timer_{}", name.as_str()), format!("{} timer", name.as_str())),
        None => (format!("timer_{}", secs as u64), label),
    };
    Some(json!({
        "id": id,
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
    // "Leave the news out of my briefing", "put the free games back in".
    if let Some(parts) = crate::briefing::part_order(s) {
        return tool("run_executive_briefing", parts);
    }
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
    if let Some(c) = COPY_TEXT.captures(s) {
        let window = c.get(1).or(c.get(2)).is_some_and(|m| matches!(m.as_str(), "window" | "fenster"));
        return tool("inspect_screen_or_selection", if window { json!({ "mode": "copy_text" }) } else { json!({ "mode": "copy_text", "target_monitor": "primary" }) });
    }
    if NETWORK.is_match(s) {
        return tool("network_status", json!({}));
    }
    if PROCESSES.is_match(s) {
        return tool("top_processes", json!({}));
    }
    if LIST_SKILLS.is_match(s) {
        return tool("list_skills", json!({}));
    }
    if LIST_PROTOCOLS.is_match(s) {
        return tool("manage_stark_protocol", json!({ "action": "list" }));
    }
    if VOICE_FASTER.is_match(s) {
        return tool("media_and_volume_control", json!({ "action": "jai_faster" }));
    }
    if VOICE_SLOWER.is_match(s) {
        return tool("media_and_volume_control", json!({ "action": "jai_slower" }));
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
            // "my jai config file" searches for "jai config" (the generic word is no name).
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

/// "Can you make it green" heard as "And you made it green" (2026-11-09, four times): the "and"
/// is gone already; "you made it green" → "make it green". None unless a command verb follows.
pub(crate) fn without_lost_can(s: &str) -> Option<String> {
    const VERBS: &[&str] = &[
        "make", "made", "change", "give", "show", "turn", "put", "open", "close", "move", "zoom", "spin", "rotate", "paint", "colour", "color", "hide", "remove", "detach",
        "dock", "save", "export", "play", "stop", "switch", "set", "bring", "take", "find", "fly", "render", "build",
    ];
    let rest = s.strip_prefix("you ").filter(|r| r.split(' ').next().is_some_and(|v| VERBS.contains(&v)))?;
    Some(rest.strip_prefix("made ").map_or_else(|| rest.to_string(), |r| format!("make {r}")))
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
    // His name as a sentence of its own, as Sir types it in the console ("JAI. Span over all
    // three monitors."): the full stop kept it from being taken for the address it is.
    for lead in ["hey jai. ", "ok jai. ", "okay jai. ", "jai. "] {
        if let Some(rest) = s.strip_prefix(lead) {
            s = rest.trim_start().to_string();
            break;
        }
    }
    // Polite wrappers and spoken interjections ("Nana, open my browser") that don't change the
    // command.
    for _ in 0..4 {
        let before = s.clone();
        for prefix in [
            "hey jai ", "jai ", "please ", "bitte ", "can you ", "could you ", "would you ", "kannst du ",
            "könntest du ", "würdest du ", "mal ", "doch mal ", "ok ", "okay ", "so ", "nana ", "na ", "nah ", "no ", "yeah ",
            "yes ", "yep ", "um ", "umm ", "uh ", "uhm ", "äh ", "ähm ", "öhm ", "hmm ", "also ", "well ", "alright ", "right ",
            "and ", "und ", "now ", "then ", "dann ", "ja ", "nein ", "oh ", "ah ", "actually ", "eigentlich ", "hey ",
            "wait ", "warte ", "maybe ", "vielleicht ", "sorry ", "oops ", "ups ", "hoppla ", "uhh ", "hm ", "ähh ",
        ] {
            if let Some(rest) = s.strip_prefix(prefix) {
                s = rest.to_string();
            }
        }
        if let Some(rest) = without_lost_can(&s) {
            s = rest;
        }
        // (" right now" before " now": the other way round left "… right".)
        for suffix in [" please", " bitte", " jai", " for me", " für mich", " right now", " now", " jetzt", " mal", " sofort"] {
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

/// Spoken layout commands for the HUD (`control_jai_interface` arguments), classified on Sir's
/// own words: "make the orb bigger", "the orb at 150 percent".
pub fn ui_layout_command(text: &str) -> Option<Value> {
    let s = normalize(text);
    if s.is_empty() || s.split_whitespace().count() > 32 {
        return None;
    }
    // A feature asked for in its own words is that feature's ("hide the words beside the orb"
    // hid the orb's panel, seen live 2026-11-14: these are asked before the fast path).
    if crate::hud_feel::command(&s).is_some() {
        return None;
    }
    interface::layout_command(&s)
}

/// "Open my system stats (in a new window)", "show my telemetry on monitor 2", "open my calendar
/// in a new window", "open the simulation lab", "show the QR code", "open my gym logs", Whisper's
/// "open message from stats": `open_jai_view` arguments. JAI's own views are never programs
/// for the app launcher. Panel words ("hide the system panel", "show the telemetry tab") stay HUD
/// layout commands.
pub fn internal_view_command(text: &str) -> Option<Value> {
    // The network radar, the disk map and the Earth ("check the network", "show me the earth").
    if let Some(args) = panel_request(text) {
        return Some(args);
    }
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
    if let Some(args) = random_views(c.get(2)?.as_str()) {
        return Some(args);
    }
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
    if let Some(m) = monitor {
        args["monitor"] = m;
    }
    Some(args)
}

/// The HUD's own views, before the layout classifier or the model can guess at them: the core
/// sphere (also as Whisper mishears it, see `typos::core_homophones`), the board restore ("bring
/// back the screen"), and the navigation dock's tabs.
pub fn hud_view_command(text: &str) -> Option<Intent> {
    // The 3D studio ("render me an Iron Man suit in 3D"; its edits only while it is open). The
    // houses of a town in 3D are the Earth's (live, 2026-11-19: the studio took "the houses of
    // Berlin in 3D" for a thing to model).
    let houses = || panel_request(text).is_some_and(|args| args["action"] == "show_buildings");
    if let Some(intent) = crate::model3d::intent(text).filter(|_| !houses()) {
        return Some(intent);
    }
    // Pictures, live cameras and street view ("show me a picture of the Earth", "Zeig mir eine
    // Webcam in Stuttgart", "street view of the Eiffel Tower"): their phrases live there.
    if let Some(intent) = crate::image_viewer::voice_intent(text) {
        return Some(intent);
    }
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

/// "Bring back my windows", "reopen the windows from last time", "restore my last session",
/// "open everything I had open again", "hol meine Fenster von vorhin zurück": what was open
/// last comes back, also after a restart. ("Bring all the windows back in" docks them.)
static WINDOWS_BACK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:bring|get|put|pull)\s+(?:back\s+)?(?:up\s+)?(?:all\s+)?(?:(?:of\s+)?(?:the|my|your|those)\s+)?(?:recent(?:ly)?(?:\s+(?:opened|open|closed))?\s+|last\s+|previous\s+|old\s+|closed\s+)?(?:windows and widgets|widgets and windows|windows|widgets)(?:\s+back)?(?:\s+up)?(?:\s+(?:again|from\s+(?:last\s+time|before|earlier|yesterday|the\s+last\s+session|last\s+session)|i\s+had\s+open(?:\s+(?:before|last\s+time|earlier))?))?|(?:reopen|restore|re-open)\s+(?:all\s+)?(?:(?:of\s+)?(?:the|my|your)\s+)?(?:recent(?:ly)?(?:\s+(?:opened|open|closed))?\s+|last\s+|previous\s+|closed\s+)?(?:windows and widgets|windows|widgets|session)(?:\s+(?:again|from\s+(?:last\s+time|before|earlier|the\s+last\s+session|last\s+session)|i\s+had\s+open(?:\s+(?:before|last\s+time|earlier))?))?|(?:open|show)\s+(?:all\s+)?(?:(?:the|my)\s+)?(?:recent(?:ly)?(?:\s+(?:opened|closed))?\s+|last\s+|previous\s+)?(?:windows|widgets)\s+(?:again|from\s+(?:last\s+time|before|earlier|the\s+last\s+session|last\s+session))|(?:bring|open|put)\s+(?:back\s+)?(?:everything|all)\s+(?:back\s+)?(?:i|that\s+i|that)\s+(?:had|was)\s+open(?:\s+(?:again|before|last\s+time|earlier))?(?:\s+back)?(?:\s+up)?|(?:hol|hole|bring|bringe)\s+(?:mir\s+)?(?:meine|die|alle\s+meine)\s+(?:letzten\s+|alten\s+)?(?:fenster|widgets)\s+(?:von\s+(?:vorhin|gestern|letztes\s+mal|letztem\s+mal)\s+)?(?:zurück|wieder|wieder\s+her)|(?:öffne|oeffne|stell|stelle)\s+(?:meine|die)\s+(?:letzten\s+)?(?:fenster|widgets)\s+(?:wieder\s+her|wieder\s+auf|wieder))$")
});
/// "… the next time I start you", "… at startup", "… when you start", "… beim nächsten Start".
static NEXT_START: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\s+(?:again\s+)?(?:(?:the\s+)?next\s+time(?:\s+(?:i\s+start\s+(?:you|jai)(?:\s+up)?|you\s+start(?:\s+up)?|i\s+open\s+you))?|(?:at|on|for|after)\s+(?:the\s+|every\s+|each\s+)?(?:next\s+)?(?:start|startup|start-up|launch|boot|restart)|when(?:ever)?\s+(?:you|i)\s+(?:start|restart|boot|launch)(?:\s+(?:you|jai|up|again))*(?:\s+(?:the\s+)?next\s+time)?|(?:beim|für\s+den|zum|nach\s+dem)\s+nächsten\s+start)(?:\s+(?:again|wieder))?(?:\s+any\s?more)?$")
});
static KEEP_VERB: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(dont\s+|do\s+not\s+|never\s+|no\s+longer\s+|stop\s+)?(?:open|opening|show|showing|keep|keeping|bring\s+up|start\s+with|load|display|pin|have|öffne|oeffne|zeig|zeige|behalte|behalt|lade)\s+(.+)$")
});

/// "Open that widget again the next time I start you", "keep the CPU gauge for the next
/// start", "don't open it at startup any more": `create_or_update_widget` arguments with
/// `keep_at_start` (he starts with an empty panel otherwise). The widget is "last", a name, or
/// "window" for the last window.
fn keep_at_start_command(s: &str) -> Option<Value> {
    if s.split_whitespace().count() > 18 {
        return None;
    }
    let tail = NEXT_START.find(s)?;
    let c = KEEP_VERB.captures(s[..tail.start()].trim())?;
    // ("Keep that window for next time": the tail starts after "for".)
    let named: Vec<&str> = c.get(2)?.as_str().split_whitespace().filter(|w| !matches!(*w, "that" | "this" | "it" | "the" | "my" | "one" | "again" | "open" | "up" | "there" | "for" | "das" | "dieses" | "es" | "wieder" | "mein" | "für")).collect();
    let window = named.iter().any(|w| matches!(*w, "window" | "fenster"));
    let name = named.into_iter().filter(|w| !matches!(*w, "widget" | "widgets" | "window" | "fenster" | "block")).collect::<Vec<_>>().join(" ");
    let target = match (name.is_empty(), window) {
        (true, true) => "window".to_string(),
        (true, false) => "last".to_string(),
        // "The CPU gauge" is called what `live_gauge` called it.
        (false, _) => live_gauge(name.trim_end_matches(" gauge")).map_or(name.clone(), |(_, title, _)| title.to_string()),
    };
    Some(json!({ "id": target, "keep_at_start": c.get(1).is_none() }))
}

/// "Open the feature lab", "show me your features window", "feature lab": his own list of
/// everything he can do (never a program for the app launcher).
static FEATURE_LAB: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:open|show|show me|bring up|pull up|display|launch|start|go to|öffne|oeffne|zeig|zeige|zeig mir)\s+(?:up\s+)?(?:the\s+|your\s+|my\s+|das\s+|dein\s+|die\s+)?)?(?:feature lab|features lab|feature laboratory|featurelab|feature window|features window|feature list window|feature labor)$")
});
/// "Open the theme studio", "create a new theme", "erstelle ein neues Theme": the block where
/// he designs one (never a program for the app launcher).
static THEME_STUDIO: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:open|show|show me|bring up|pull up|display|launch|start|go to|öffne|oeffne|zeig|zeige|zeig mir)\s+(?:up\s+)?(?:the\s+|your\s+|my\s+|das\s+|dein\s+|die\s+)?)?(?:theme studio|themestudio|theme editor|theme designer|theme creator)$|^(?:create|make|design|build|erstelle|erstell|mach|mache|bau)\s+(?:me\s+|mir\s+)?(?:a\s+|an\s+|ein\s+|eine\s+)?(?:new\s+|own\s+|custom\s+|neues\s+|eigenes\s+)?theme$")
});
/// "Open the mixer", "show me the volume mixer", "zeig den Mixer": the block with a fader for
/// every app ("which apps are playing audio?" is read out, not opened).
static MIXER_VIEW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:open|show|show me|bring up|pull up|display|öffne|oeffne|zeig|zeige|zeig mir)\s+(?:up\s+)?(?:the\s+|my\s+|den\s+|meinen\s+)?(?:(?:volume|audio|sound|lautstärke)\s*)?mixer|(?:the\s+|my\s+)?(?:volume|audio|sound)\s+mixer|mixer\s+(?:öffnen|zeigen))$")
});
/// "Show me my drive space", "how much space do I have left", "zeig mir meinen Speicherplatz":
/// the live window with every drive in it.
static DRIVE_SPACE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:show|give|open|pull up|bring up|display|check)(?: me)?\s+(?:my\s+|the\s+)?(?:(?:drive|disk|ssd|hdd)\s+(?:space|usage|storage)|free\s+space|storage(?:\s+space)?)|how much (?:(?:drive|disk|storage|free)\s+)?space (?:do i have(?: left)?|is left|have i got(?: left)?|is free)|(?:zeig|zeige)(?: mir)?\s+(?:meinen\s+|den\s+)?speicherplatz|wie viel speicher(?:platz)? (?:habe ich|hab ich|ist)(?: noch)?(?: frei| übrig)?)$")
});
/// "Start a stopwatch", "Stoppuhr starten".
static STOPWATCH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:start|begin|run|open|make|create|add|give me|show me|show|starte|öffne|oeffne|zeig mir|zeig|mach|mache|erstelle|erstell)\s+(?:me\s+|mir\s+)?(?:(?:a|an|the|my|eine|die|meine)\s+)?(?:new\s+|neue\s+)?)?(?:stopwatch|stoppuhr)(?:\s+(?:starten|start|an))?$")
});
/// "Stop the stopwatch", "pause the timer", "resume the timer", "reset the stopwatch", "Timer
/// anhalten", "setz die Stoppuhr zurück".
static TIMER_CONTROL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(stop|pause|hold|halt|freeze|resume|continue|unpause|restart|reset|stoppe|stopp|pausiere|halte|setze|setz)\s+(?:(?:the|my|die|den|meine|meinen)\s+)?(stopwatch|stoppuhr|timer|countdown)(?:\s+(?:an|fort|zurück|weiter))?$|^(stopwatch|stoppuhr|timer|countdown)\s+(stoppen|anhalten|pausieren|fortsetzen|zurücksetzen|stop|pause|resume|reset|restart)$")
});
/// "Put a note on the board saying buy milk", "add a note that says call Tom", "schreib eine
/// Notiz: Milch kaufen". ("Make a note that …" is something to remember, not to look at.)
static NOTE_ON_PANEL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:put|add|write|leave|pin|stick|post|create|make|take)\s+(?:me\s+|up\s+)?(?:(?:a|an|another)\s+)?(?:new\s+|quick\s+|little\s+)?note\s+(?:(?:on|to|onto|in)\s+(?:the\s+|my\s+|your\s+)?(?:board|panel|hud|screen|widgets\s+panel)\s+)?(?:saying|that says|which says|that reads|reading|says|with the text|with)\s+(.+)$|^(?:put|add|write|leave|pin|stick|post|create|make|take)\s+(?:me\s+|up\s+)?(?:(?:a|an|another)\s+)?(?:new\s+|quick\s+|little\s+)?note\s+(?:on|to|onto|in)\s+(?:the\s+|my\s+|your\s+)?(?:board|panel|hud|screen|widgets\s+panel)\s+(.+)$|^(?:schreib|schreibe|mach|mache|leg|lege|erstelle|erstell|pack|setz|setze|häng|hänge)\s+(?:mir\s+)?(?:(?:eine|ne)\s+)?(?:neue\s+|kurze\s+)?notiz\s+(?:(?:auf|an|in)\s+(?:das|die|den|dem|mein|meine)\s+(?:board|tafel|panel|hud)\s+)?(?:mit\s+(?:dem\s+text\s+)?|dass\s+)?(.+)$")
});
/// "… in a new window" after a note's text: a window of its own for it.
static NOTE_WINDOW_TAIL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\s+(?:in|into|as|on)\s+(?:a\s+|an\s+|its\s+own\s+|one\s+|the\s+)?(?:new|separate|second|own|extra|another|real)\s+(?:native\s+|os\s+)?window$|\s+(?:in|als)\s+(?:einem\s+|ein\s+)?(?:neuen|neues|eigenen|separaten|extra)\s+fenster$")
});
/// The way a note that was asked for last names its place ("… on the board").
static NOTE_PLACE_TAIL: LazyLock<Regex> =
    LazyLock::new(|| re(r"\s+(?:on|onto|to|in|auf|an)\s+(?:the|my|your|das|die|den|dem|mein|meine)\s+(?:board|panel|hud|screen|tafel)$"));
/// The lists he keeps on the panel; "list" alone only where it can mean nothing else.
const CHECKLIST_NOUNS: &str = r"(checklist|check list|shopping list|to do list|todo list|to-do list|checkliste|einkaufsliste|todo liste|to-do-liste|aufgabenliste)";
const ANY_LIST: &str = r"(?:the\s+|my\s+|die\s+|meine\s+|der\s+|meiner\s+)?(checklist|check list|list|shopping list|to do list|todo list|to-do list|checkliste|liste|einkaufsliste|todo liste|to-do-liste|aufgabenliste)";
/// "Add a checklist with eggs, bread and butter", "start a shopping list", "erstelle eine
/// Einkaufsliste mit Milch und Eiern".
static CHECKLIST_NEW: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(?:add|make|create|start|put up|give me|set up|write|begin|open|erstelle|erstell|mach|mache|leg|lege|schreib|schreibe|starte|öffne)\s+(?:me\s+|mir\s+)?(?:(?:a|an|another|my|the|eine|meine|die)\s+)?(?:new\s+|neue\s+)?{CHECKLIST_NOUNS}(?:\s+(?:with|of|containing|for|saying|mit|für)\s+(.+?))?(?:\s+an)?$"))
});
/// "Add milk to the list", "put eggs and bread on my shopping list", "setz Milch auf die Liste".
static CHECKLIST_ADD: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(?:add|put|write|stick|setz|setze|schreib|schreibe|füg|füge|pack|nimm)\s+(.+?)\s+(?:to|on|onto|in|auf|zur|zu|an)\s+{ANY_LIST}(?:\s+hinzu)?$"))
});
/// "Check off milk", "tick eggs off the list", "mark bread as done", "hak Milch ab".
static CHECKLIST_DONE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(?:check off|tick off|cross off|mark off)\s+(.+?)(?:\s+(?:on|from|in)\s+{ANY_LIST})?$|^(?:check|tick|cross|mark|hak|hake|streich|streiche)\s+(.+?)\s+(?:off|ab|durch)(?:\s+(?:the\s+|my\s+)?(?:check\s?list|list))?$|^mark\s+(.+?)\s+as\s+(?:done|complete|completed|finished)$"))
});
/// "Uncheck milk".
static CHECKLIST_UNDONE: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"^(?:uncheck|untick|unmark)\s+(.+?)(?:\s+(?:on|from|in)\s+{ANY_LIST})?$")));
/// "Remove milk from the list", "nimm Milch von der Liste".
static CHECKLIST_DROP: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(?:remove|delete|take|drop|streich|streiche|entferne|entfern|lösche|lösch|nimm)\s+(.+?)\s+(?:from|off|von|aus)\s+{ANY_LIST}(?:\s+(?:weg|raus))?$"))
});
/// "Show me a CPU chart", "add a RAM graph", "a chart of my GPU temperature", "zeig mir ein CPU
/// Diagramm": a live line on the panel.
static WIDGET_CHART: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:create|make|add|build|give me|show me|show|put up|set up|open|draw|plot|erstelle|erstell|mach|mache|zeig mir|zeig|zeige mir|zeige|öffne)\s+(?:me\s+|mir\s+)?(?:(?:a|an|another|the|my|ein|eine|einen|das)\s+)?(?:new\s+|live\s+|neues\s+)?(?:(.+?)\s+(?:line chart|chart|graph|diagramm|verlauf)|(?:line chart|chart|graph|diagramm)\s+(?:of|for|with|von|für|mit)\s+(?:my\s+|the\s+|meine\s+|die\s+|der\s+|meiner\s+)?(.+?))$")
});
/// "Remove the timer", "close the note", "delete the CPU chart", "cancel the stopwatch", "Notiz
/// löschen": what is on the panel, by its kind (and never a program of that name).
static WIDGET_KIND_REMOVE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:remove|delete|close|dismiss|clear|cancel|hide|get rid of|kill|entferne|entfern|lösche|lösch|schließe|schließ|schliesse)\s+(?:(?:the|my|that|this|your|die|den|das|meine|meinen)\s+)?(?:(.+?)\s+)?(timer|countdown|stopwatch|stoppuhr|note|notiz|checklist|check list|checkliste|shopping list|einkaufsliste|to do list|todo list|to-do list|chart|graph|diagramm|gauge|anzeige)$|^(?:(?:die|den|das|meine|meinen)\s+)?(timer|countdown|stoppuhr|notiz|checkliste|einkaufsliste|diagramm|anzeige)\s+(?:löschen|entfernen|schließen|wegmachen|weg)$|^take\s+(?:the\s+|my\s+|that\s+|this\s+)?(timer|countdown|stopwatch|note|checklist|shopping list|to do list|chart|graph|gauge)\s+away$")
});
/// "What's open?", "what's on the board?", "which widgets are open?", "was ist offen?".
static WHATS_OPEN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:(?:show|tell) me )?(?:whats|what is)\s+open(?:\s+(?:on|in)\s+(?:the|your|my)\s+(?:board|panel|hud|screen))?|(?:(?:show|tell) me )?(?:whats|what is)\s+up\s+(?:on|in)\s+(?:the|your|my)\s+(?:board|panel|hud|screen)|(?:tell me )?(?:whats|what is)\s+on\s+(?:the|your|my)\s+(?:board|panel|hud)|(?:tell me )?what\s+(?:do you have|have you got|have you|you have)\s+open|(?:what|which)\s+(?:widgets|windows|blocks)\s+(?:are|do i have|do you have)(?:\s+(?:open|up))?|list\s+(?:all\s+)?(?:my|the|your|all|open)\s+(?:open\s+)?(?:widgets|windows|blocks)|was\s+(?:ist|hast\s+du)\s+offen|was\s+ist\s+auf\s+(?:dem|deinem|meinem)\s+(?:board|panel|hud)|welche\s+(?:widgets|fenster)\s+(?:sind\s+(?:offen|auf)|habe\s+ich|hast\s+du)|(?:zeig|zeige)(?:\s+mir)?\s+was\s+offen\s+ist)$")
});

/// `body` (words of the normalised sentence) as Sir wrote them after the word `after`: his own
/// capitals and commas, up to its last word.
fn as_written(original: &str, after: &str, body: &str) -> String {
    let lower = original.to_lowercase();
    if lower.len() != original.len() {
        return body.to_string();
    }
    let mut at = lower.find(after).map_or(0, |found| found + after.len());
    let mut start = None;
    for word in body.split_whitespace() {
        let Some(found) = lower[at..].find(word) else {
            return body.to_string();
        };
        start.get_or_insert(at + found);
        at += found + word.len();
    }
    match start {
        Some(start) if original.is_char_boundary(start) && original.is_char_boundary(at) => original[start..at].to_string(),
        _ => body.to_string(),
    }
}

/// "Eggs, bread and butter", as checklist items.
fn checklist_items(list: &str) -> Vec<Value> {
    static SEPARATOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\s*[,;&]\s*(?:and\s+|und\s+)?|\s+(?:and|und)\s+").expect("valid list separator"));
    SEPARATOR.split(list).map(str::trim).filter(|item| !item.is_empty()).map(|item| json!({ "text": item })).collect()
}

/// A list by the name he gave it: its title.
fn checklist_title(noun: &str) -> &'static str {
    match noun {
        "shopping list" | "einkaufsliste" => "Shopping list",
        "to do list" | "todo list" | "to-do list" | "todo liste" | "to-do-liste" | "aufgabenliste" => "To-do list",
        _ => "Checklist",
    }
}

/// What goes onto the list a request names.
fn checklist_add(noun: &str, items: Vec<Value>) -> Value {
    match checklist_title(noun) {
        // "The list": the latest one, or a first one.
        "Checklist" => json!({ "id": "checklist", "default_type": "Checklist", "add_items": items }),
        // A list of that name: found, or started.
        title => json!({ "type": "Checklist", "title": title, "add_items": items }),
    }
}

/// `TIMER_CONTROL` as `create_or_update_widget` arguments.
fn timer_control(s: &str) -> Option<Value> {
    let c = TIMER_CONTROL.captures(s)?;
    let (verb, noun) = match (c.get(1), c.get(2)) {
        (Some(verb), Some(noun)) => (verb.as_str(), noun.as_str()),
        _ => (c.get(4)?.as_str(), c.get(3)?.as_str()),
    };
    let id = if matches!(noun, "stopwatch" | "stoppuhr") { "stopwatch" } else { "timer" };
    Some(match verb {
        "stop" | "pause" | "hold" | "halt" | "freeze" | "stoppe" | "stopp" | "pausiere" | "halte" | "stoppen" | "anhalten" | "pausieren" => json!({ "id": id, "pause": true }),
        "resume" | "continue" | "unpause" | "fortsetzen" => json!({ "id": id, "start": true }),
        "restart" => json!({ "id": id, "start": true, "reset": true }),
        "reset" | "zurücksetzen" => json!({ "id": id, "reset": true }),
        "setze" | "setz" if s.ends_with(" zurück") => json!({ "id": id, "reset": true }),
        "setze" | "setz" if s.ends_with(" fort") => json!({ "id": id, "start": true }),
        _ => return None,
    })
}

/// A sentence about the widgets on his panel: the stopwatch, a note, a checklist, a live chart,
/// or one of them by its kind. `text` is the sentence as he said it (a note keeps his words).
fn widget_sentence(s: &str, text: &str) -> Option<Intent> {
    // (The kit's own things are no widgets on the panel: "delete the last note" is his
    // notebook's, "cancel the sleep timer" the sleep timer's. Seen live 2026-11-13: both were
    // answered with "there's no … on the panel".)
    if crate::kit::command(s, text).is_some() {
        return None;
    }
    let widget = |args: Value| Some(Intent::Tool { name: "create_or_update_widget", args });
    if STOPWATCH.is_match(s) {
        return widget(json!({
            "id": "stopwatch",
            "type": "CountdownOrStopwatch",
            "title": "Stopwatch",
            "mode": "stopwatch",
            "running": true,
            "elapsed_ms": 0,
            "alerted": false,
            "started_at_ms": crate::widgets::now_ms(),
        }));
    }
    if let Some(args) = timer_control(s) {
        return widget(args);
    }
    if let Some(c) = NOTE_ON_PANEL.captures(s) {
        let said = c.iter().skip(1).flatten().next()?;
        // The word before his text: from there on, the text is as he wrote it.
        let cue = s[..said.start()].split_whitespace().last().unwrap_or("note");
        let words = NOTE_PLACE_TAIL.replace(said.as_str(), "");
        // "… in a new window": where the note goes, not words of it.
        let separate = NOTE_WINDOW_TAIL.is_match(&words);
        let words = NOTE_WINDOW_TAIL.replace(&words, "");
        // (Two notes in one sentence are two notes, however fast they are read.)
        static NOTES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let note = as_written(text, cue, &words);
        // Named after its first words ("what's open?" reads the names out).
        let head = note.split([',', ';', '\n']).next().unwrap_or("").split_whitespace().take(5).collect::<Vec<_>>().join(" ");
        let id = format!("note_{}_{}", crate::widgets::now_ms(), NOTES.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let title = format!("Note: {}", head.trim_end_matches(|c: char| !c.is_alphanumeric()));
        if separate {
            return Some(Intent::Tool { name: "spawn_native_window", args: json!({ "id": id, "title": title, "markdown": note, "width": 460, "height": 300 }) });
        }
        return widget(json!({
            "id": id,
            "type": "MarkdownNoteCard",
            "title": title,
            "markdown": note,
        }));
    }
    if let Some(c) = CHECKLIST_NEW.captures(s) {
        let noun = c.get(1)?.as_str();
        let items = c.get(2).map_or_else(Vec::new, |list| checklist_items(&as_written(text, noun, list.as_str())));
        let mut args = json!({ "type": "Checklist", "title": checklist_title(noun) });
        if !items.is_empty() {
            args["add_items"] = json!(items);
        }
        return widget(args);
    }
    if let Some(c) = CHECKLIST_ADD.captures(s) {
        let what = c.get(1)?.as_str();
        if !matches!(what, "it" | "this" | "that" | "them" | "das" | "es" | "sie") {
            let verb = s.split_whitespace().next().unwrap_or("");
            return widget(checklist_add(c.get(2)?.as_str(), checklist_items(&as_written(text, verb, what))));
        }
    }
    // (Of a list that is there: nothing is started for them.)
    let on_list = |c: &regex::Captures, group: usize| c.get(group).map_or("checklist", |noun| noun.as_str()).to_string();
    if let Some(c) = CHECKLIST_DONE.captures(s) {
        return widget(json!({ "id": on_list(&c, 2), "check_item": c.get(1).or(c.get(3)).or(c.get(4))?.as_str() }));
    }
    if let Some(c) = CHECKLIST_UNDONE.captures(s) {
        return widget(json!({ "id": on_list(&c, 2), "uncheck_item": c.get(1)?.as_str() }));
    }
    if let Some(c) = CHECKLIST_DROP.captures(s) {
        return widget(json!({ "id": on_list(&c, 2), "remove_item": c.get(1)?.as_str() }));
    }
    if let Some((source, title, unit)) = WIDGET_CHART.captures(s).and_then(|c| c.get(1).or(c.get(2))).and_then(|m| live_gauge(m.as_str())) {
        // (Its own title: a gauge of the same reading stays.)
        return widget(json!({ "type": "LiveLineChart", "title": format!("{title} chart"), "source": source, "unit": unit }));
    }
    if let Some(c) = WIDGET_KIND_REMOVE.captures(s) {
        let kind = c.get(2).or(c.get(3)).or(c.get(4))?.as_str();
        let subject = c.get(1).map_or("", |m| m.as_str());
        let chart = matches!(kind, "chart" | "graph" | "diagramm");
        // A note or chart on the board is the board's: the one in the spotlight goes back into
        // it, another one leaves it.
        if (chart || matches!(kind, "note" | "notiz")) && subject.is_empty() {
            if crate::hud_board::spotlight_active() {
                return Some(Intent::Silent(SilentAction { tool: "hud_board", args: json!({ "action": "minimize" }), label: "Back to the board".to_string() }));
            }
            if crate::hud_board::is_active() && crate::hud_board::names_block(kind) {
                return Some(Intent::Tool { name: "hud_board", args: json!({ "action": "remove", "target": kind }) });
            }
        }
        let id = match live_gauge(subject) {
            Some((_, title, _)) if chart => format!("{title} chart"),
            Some((_, title, _)) => title.to_string(),
            None if subject.is_empty() => kind.to_string(),
            None => format!("{subject} {kind}"),
        };
        return Some(Intent::Tool { name: "remove_widget", args: json!({ "id": id }) });
    }
    None
}
/// "Remove all widgets", "delete the CPU load widget", "close that widget", "entferne alle
/// Widgets": the widgets themselves (the panel that holds them is the layout's).
static WIDGET_REMOVE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:remove|delete|close|clear|dismiss|get rid of|kill|entferne|entfern|lösche|lösch|schließe|schließ|schliesse)\s+(?:(alle|all|every)\s+)?(?:of\s+)?(?:the\s+|my\s+|your\s+|those\s+|these\s+|meine\s+|die\s+|das\s+)?(?:(.+?)\s+)?(widgets?)$")
});
/// "Create a widget that shows my CPU load", "add a GPU temperature gauge", "make me a RAM
/// widget", "erstelle ein Widget für die CPU-Auslastung": a live gauge on the widgets panel.
static WIDGET_LIVE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:create|make|add|build|give me|show me|show|put up|set up|erstelle|erstell|mach|mache|zeig mir|zeig)\s+(?:me\s+)?(?:a|an|another|ein|eine|einen)?\s*(?:new\s+|live\s+|small\s+|neues\s+)?(?:(?:widget|gauge|meter|anzeige)\s+(?:that shows|which shows|showing|that displays|that monitors|to show|to monitor|for|with|of|das|für|mit|zur)\s+(?:me\s+)?(?:my\s+|the\s+|meine\s+|die\s+|der\s+)?(.+?)|(.+?)\s+(?:widget|gauge|meter|anzeige))$")
});

/// The live source a widget's subject names ("cpu load", "gpu temperature", "memory"): (source,
/// title, unit).
fn live_gauge(subject: &str) -> Option<(&'static str, &'static str, &'static str)> {
    let words: Vec<&str> = subject.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty() && !matches!(*w, "load" | "usage" | "use" | "utilisation" | "utilization" | "auslastung" | "live" | "my" | "the" | "current")).collect();
    Some(match words.as_slice() {
        ["cpu"] | ["processor"] | ["prozessor"] => ("cpu", "CPU load", "%"),
        ["ram"] | ["memory"] | ["arbeitsspeicher"] | ["speicher"] => ("ram", "RAM", "%"),
        ["gpu"] | ["graphics", "card"] | ["grafikkarte"] => ("gpu", "GPU load", "%"),
        ["gpu", "temperature"] | ["gpu", "temp"] | ["gpu", "temperatur"] | ["graphics", "card", "temperature"] => ("gpu_temp", "GPU temperature", "°C"),
        ["vram"] | ["video", "memory"] | ["gpu", "memory"] => ("vram", "VRAM", "%"),
        ["download"] | ["download", "speed"] | ["network"] => ("net_down", "Download", "KB/s"),
        ["upload"] | ["upload", "speed"] => ("net_up", "Upload", "KB/s"),
        _ => return None,
    })
}

fn hud_view_sentence(text: &str) -> Option<Intent> {
    let s = normalize(text);
    let interface = |args: Value| Some(Intent::Tool { name: "configure_interface", args });
    if WINDOWS_BACK.is_match(&s) {
        return Some(Intent::Tool { name: "spawn_native_window", args: json!({ "restore": "recent" }) });
    }
    if let Some(args) = keep_at_start_command(&s) {
        return Some(Intent::Tool { name: "create_or_update_widget", args });
    }
    if FEATURE_LAB.is_match(&s) {
        return interface(json!({ "feature_lab": true }));
    }
    if THEME_STUDIO.is_match(&s) {
        return interface(json!({ "theme_studio": true }));
    }
    if MIXER_VIEW.is_match(&s) {
        return interface(json!({ "mixer_view": true }));
    }
    if DRIVE_SPACE.is_match(&s) {
        return Some(Intent::Tool { name: "spawn_native_window", args: json!({ "live": "drive_space" }) });
    }
    if WHATS_OPEN.is_match(&s) {
        return Some(Intent::Tool { name: "whats_open", args: json!({}) });
    }
    if let Some(intent) = widget_sentence(&s, text) {
        return Some(intent);
    }
    // The emblem is out of the way: "come back" calls it, not the window.
    if crate::hud_dock::orb_aside() && ORB_BACK_SHORT.is_match(&s) {
        return interface(json!({ "orb_board_position": "center" }));
    }
    if let Some(c) = WIDGET_REMOVE.captures(&s) {
        let named = c.get(2).map_or("", |m| m.as_str()).trim();
        let id = match (c.get(1).is_some() || c.get(3).is_some_and(|w| w.as_str() == "widgets"), named) {
            (true, "") => "all".to_string(),
            (_, "" | "that" | "this" | "last" | "latest" | "newest") => "last".to_string(),
            (_, name) => name.to_string(),
        };
        return Some(Intent::Tool { name: "remove_widget", args: json!({ "id": id }) });
    }
    if let Some((source, title, unit)) = WIDGET_LIVE.captures(&s).and_then(|c| c.get(1).or(c.get(2))).and_then(|m| live_gauge(m.as_str())) {
        return Some(Intent::Tool { name: "create_or_update_widget", args: json!({ "type": "MetricGauge", "title": title, "source": source, "unit": unit }) });
    }
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
    if let Some(intent) = docking_retired(&s) {
        return Some(intent);
    }
    if let Some(intent) = shadow_or_server_command(&s) {
        return Some(intent);
    }
    // JAI's own Discord account (once it is set up): calls as himself, pairing, his inbox.
    if let Some(args) = crate::discord_bot::intent(&s) {
        return Some(Intent::Tool { name: "discord_bot", args });
    }
    if let Some(args) = crate::away_mode::intent(&s) {
        return Some(Intent::Tool { name: "away_mode", args });
    }
    if let Some(args) = crate::watch_and_tell::intent(&s) {
        return Some(Intent::Tool { name: "watch_and_tell", args });
    }
    if let Some(action) = discord_call_command(&s) {
        return Some(Intent::Tool { name: "discord_call", args: json!({ "action": action }) });
    }
    if let Some(name) = theme_preset_request(&s) {
        return interface(json!({ "hud_palette": name }));
    }
    if let Some(style) = core_style_request(&s) {
        return interface(json!({ "core_style": style }));
    }
    if let Some(c) = NAV_VIEW.captures(&s) {
        let mode = crate::hud_dock::HudViewMode::parse(capture(&c)?)?;
        return interface(json!({ "hud_view": mode.id() }));
    }
    None
}

/// Going out: "where can I take my fiancée for a Halloween night?" (JAI picks the venue and
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
    // ("I can't hear Discord" and "ich höre Discord nicht" are about the sound, the mixer's.)
    let cannot = ["cant hear", "cannot hear", "can not hear", "dont hear", "do not hear", "couldnt hear", "barely hear", "hardly hear", "scarcely hear", "nicht", "kaum"].iter().any(|w| s.contains(w));
    if call && !cannot && ["listen", "hear", "hör", "hoer", "mithören", "mithoeren", "zuhören", "zuhoeren"].iter().any(|w| s.contains(w)) && !s.contains("music") && !s.contains("musik") {
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
    // By its own name ("the arc reactor theme"), else by what stands before the word: a former
    // name ("arc cyan theme") or what he calls it ("the black and white theme").
    // (A theme that is being made is not one that is being chosen: "design a dark stealth theme".)
    if s.contains("design") || s.contains("create") || s.contains("build") || s.contains("entwirf") || s.contains("erstelle") || s.contains("save") || s.contains("speicher") {
        return None;
    }
    let by_name = |named: &str| {
        let named = named.trim();
        let named = named.rsplit_once(" the ").or(named.rsplit_once(" to ")).or(named.rsplit_once(" das ")).map_or(named, |(_, name)| name);
        let named = ["the ", "to ", "das ", "dem "].iter().find_map(|article| named.strip_prefix(article)).unwrap_or(named);
        crate::holo_theme::preset_name(named).or_else(|| crate::holo_theme::preset_for_word(named))
    };
    crate::holo_theme::PRESETS
        .iter()
        .map(|(n, _, _)| *n)
        .find(|n| s.contains(&n.replace('_', " ")))
        .or_else(|| by_name(THEME_NAMED.captures(s)?.get(1)?.as_str()))
        .or_else(|| by_name(THEME_TO.captures(s)?.get(1)?.as_str()))
}

/// "The arc cyan theme": the name before the word.
static THEME_NAMED: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:.*?\b(?:to|use|the|das|nimm|zum)\s+)?([a-zäöüß ]+?)\s+(?:theme|thema|palette|farbschema)\b"));
/// "Set theme to arc cyan", "stell das Theme auf Arc Cyan": the name after it.
static THEME_TO: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:theme|thema|palette|farbschema)\s+(?:to|auf|zu|in)\s+(?:the\s+)?([a-zäöüß ]+?)$"));

/// "Switch to the complex core", "use the simple core", "make your core complex", "nimm den
/// komplexen Kern": which core stands on the HUD.
fn core_style_request(s: &str) -> Option<&'static str> {
    static CORE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"\b(simple|classic|plain|rings?|complex|detailed|age of ultron|ultron|circuit|einfach\w*|komplex\w*|detailliert\w*)\s+(?:core|kern)\b|\b(?:core|kern)\s+(?:to\s+|look\s+|style\s+|auf\s+|in\s+)?(?:the\s+)?(simple|classic|plain|complex|detailed|ultron|einfach\w*|komplex\w*|detailliert\w*)\b")
    });
    let c = CORE.captures(s)?;
    let word = c.get(1).or_else(|| c.get(2))?.as_str();
    let word = if word.starts_with("einfach") {
        "simple"
    } else if word.starts_with("komplex") || word.starts_with("detailliert") {
        "complex"
    } else {
        word
    };
    crate::holo_core::CoreStyle::of_word(word).map(|style| style.name())
}

/// "Open the integrations hub", "show my productivity settings", "öffne den Integrations-Hub",
/// and as Whisper hears it: "the integration sub".
static INTEGRATIONS_HUB: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:open|show|bring up|pull up|display|launch|öffne|oeffne|zeig|zeige)\s+(?:me\s+|mir\s+)?(?:the\s+|my\s+|your\s+|den\s+|die\s+|das\s+|meine\s+|deine\s+)?(?:integrations?|productivity)(?:\s+(?:and|und)\s+productivity)?(?:\s*(?:hub|sub|hab|tab|settings|window|center|centre|panel|page|einstellungen))?$")
});

/// Which panels from the reference videos are up: a follow-up ("switch back", "fly to Tokyo",
/// "zoom in", "rescan") only means one of them while it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PanelsUp {
    pub radar: bool,
    pub disk: bool,
    pub earth: bool,
}

impl PanelsUp {
    pub fn now() -> Self {
        Self { radar: crate::network_radar::is_open(), disk: crate::disk_map::is_open(), earth: crate::earth::is_open() }
    }
}

/// `panel_command` with the panels as they are now.
pub fn panel_request(text: &str) -> Option<Value> {
    panel_command(text, PanelsUp::now())
}

/// "3D", "three-dimensional", "dreidimensional".
const D3: &str = r"(?:3\s?-?\s?d|three\s?-?\s?d|drei\s?-?\s?d|3\s?-?\s?dimensional|three\s?-?\s?dimensional|dreidimensional)";

/// "Switch to 3D", "make it 3D", "tilt the map", "back to 2D", "top-down view", "mach es 3D"
/// (words as `normalize` leaves them): true for 3D, false for 2D. The Earth's camera and the
/// Simulation Lab's view take it alike.
pub fn view_3d(s: &str) -> Option<bool> {
    static VIEW_3D: LazyLock<Regex> = LazyLock::new(|| {
        let (d3, d2) = (D3, r"(?:2\s?-?\s?d|two\s?-?\s?d|zwei\s?-?\s?d|2\s?-?\s?dimensional|two\s+dimensional|zweidimensional|flat|flach|top\s?-?\s?down|birds\s?-?\s?eye|overhead)");
        let obj = r"(?:it|this|that|the\s+(?:map|view|earth|globe|camera))";
        let (it, mode) = (r"(?:it\s+|this\s+|that\s+|the\s+(?:map|view|earth|globe|camera|city|town|place|area|streets?)\s+)", r"(?:\s+(?:view|mode|map|perspective))?");
        let (verb, de) = (r"(?:switch|change|go|flip|turn|swap|put|set|show|show\s+me|view|see|let\s+me\s+see|give\s+me)", r"(?:\s?-?\s?(?:ansicht|modus|karte))?(?:\s+(?:um|zurück))?");
        re(&format!(
            r"^(?:{verb}\s+{it}?(?:back\s+)?(?:over\s+)?(?:to|into|in)\s+|(?:go\s+)?back\s+to\s+|in\s+)?(?:the\s+)?({d3}|{d2}){mode}(?:\s+again)?$|^(?:go|make\s+{obj})\s+({d3}|{d2})(?:\s+again)?$|^(tilt)(?:\s+{obj})?(?:\s+back)?$|^(lean|tip)\s+{obj}\s+back$|^(untilt|level|straighten)\s+{obj}(?:\s+(?:out|again))?$|^(untilt)$|^look\s+(straight\s+down)$|^(?:schalte?|wechsel|wechsle|geh|gehe|stell|stelle)\s+(?:zurück\s+)?(?:auf|in|zu|zur|in\s+die)\s+(?:die\s+)?({d3}|{d2}|draufsicht){de}$|^(?:zeig|zeige)\s+(?:es|das|mir\s+das|mir\s+die\s+karte|die\s+karte)\s+in\s+({d3}){de}$|^(?:mach|mache)\s+(?:es|das|sie|ihn)\s+(?:wieder\s+)?({d3}|{d2})$|^({d3}|{d2})\s?-?\s?(?:ansicht|modus)$|^(kipp|kippe|neig|neige)\s+(?:die\s+)?(?:karte|ansicht|kamera)$|^(draufsicht)$|^(von\s+oben)$"
        ))
    });
    let c = VIEW_3D.captures(s)?;
    let said = c.iter().skip(1).flatten().next().map_or("", |m| m.as_str());
    Some(said.contains('3') || said.contains("three") || said.contains("drei") || ["tilt", "lean", "tip", "kipp", "kippe", "neig", "neige"].contains(&said))
}

/// "… in 3D", "three-dimensional", "dreidimensional" anywhere in a sentence ("3 days" is none).
pub fn says_3d(text: &str) -> bool {
    static SAYS: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"\b{D3}\b")));
    SAYS.is_match(&text.to_lowercase())
}

/// The network radar, the disk map and the Earth as Sir asks for them (`open_jai_view`
/// arguments): "check the network", "who is on my network?", "is my network secure?", "what is
/// taking up space?", "map my D drive", "show me the earth", "switch to satellite view", "fly to
/// Tokyo", "show the earthquakes in this area", "show me the flights over New York", "where is
/// the ISS?". A question opens its panel too: every opening action is a `show…` or a `check…`,
/// which only looks. Closing one is the board's ("close the globe", `intent_engine::block_close`).
fn panel_command(text: &str, up: PanelsUp) -> Option<Value> {
    // "Hey JAI, check check the network": a word said twice in a row counts once.
    let said = normalize(text);
    let mut words: Vec<&str> = Vec::new();
    for w in said.split_whitespace() {
        if words.last() != Some(&w) {
            words.push(w);
        }
    }
    if words.last() == Some(&"sir") {
        words.pop();
    }
    if words.is_empty() || words.len() > 18 {
        return None;
    }
    let mut s = words.join(" ");
    // "… in a new window", "… on monitor 2": where it goes, not what it is.
    static OWN_WINDOW: LazyLock<Regex> = LazyLock::new(|| {
        re(r"\s+(?:in|into|as|im|in\s+einem|als)\s+(?:a\s+|an\s+|its\s+own\s+|one\s+|the\s+|einem\s+|ein\s+)?(?:new|separate|second|own|extra|another|real|neuen|neues|eigenen|separaten|zweiten)\s+(?:native\s+|os\s+)?(?:window|fenster)$")
    });
    if OWN_WINDOW.is_match(&s) {
        s = OWN_WINDOW.replace(&s, "").trim().to_string();
    }
    let monitor = MONITOR_ANYWHERE.captures(&s).and_then(|m| monitor_value(m.get(1)?.as_str()));
    if monitor.is_some() {
        if let Some(m) = MONITOR_ANYWHERE.find(&s) {
            s = format!("{} {}", s[..m.start()].trim(), s[m.end()..].trim()).trim().to_string();
        }
    }
    let mut args = panel_args(text, &s, up)?;
    if let Some(m) = monitor {
        args["monitor"] = m;
    }
    Some(args)
}

/// A layer of the Earth on or off, said any way, while the Earth is up: "I don't want to see the
/// satellites anymore", "turn the trains back on", "toggle the clouds", "no more flights",
/// "keine Satelliten mehr", "hide everything". (jai.log 2026-11-09: "I don't want to see the
/// satellites anymore" turned the orb off.)
fn earth_layer_switch(s: &str, radar_up: bool) -> Option<Value> {
    const LAYERS: &[(&[&str], &str, &str)] = &[
        (&["satellites", "satellite", "sats", "starlink", "satelliten", "satellit"], "satellites", "show_satellites"),
        (&["trains", "train", "züge", "zuege", "bahnen"], "trains", "show_trains"),
        (&["traffic", "traffic jams", "jams", "verkehr", "staus"], "traffic", "show_traffic"),
        (&["flights", "planes", "aircraft", "air traffic", "airplanes", "flugzeuge", "flüge"], "flights", "show_flights"),
        (&["earthquakes", "quakes", "earthquake", "erdbeben"], "quakes", "show_quakes"),
        (&["storms", "hurricanes", "storm", "stürme"], "storms", "show_storms"),
        (&["fires", "wildfires", "brände", "waldbrände"], "fires", "show_fires"),
        (&["iss", "space station", "raumstation"], "iss", "show_iss"),
        (&["clouds", "cloud cover", "wolken"], "clouds", "show_clouds"),
        (&["rain radar", "weather radar", "radar", "rain", "regenradar", "regen"], "radar", "show_radar"),
        (&["terrain", "mountains", "relief", "peaks", "gelände", "berge"], "terrain", "show_terrain"),
        (&["borders", "labels", "place names", "city names", "names", "grenzen", "beschriftungen", "namen"], "labels", "show_labels"),
        (&["cameras", "webcams", "kameras"], "cameras", "show_cameras"),
    ];
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() > 14 {
        return None;
    }
    let padded = format!(" {s} ");
    let has = |p: &str| padded.contains(&format!(" {p} "));
    let all = ["everything", "all layers", "all the layers", "all markers", "the layers", "alles", "alle ebenen"].iter().any(|p| has(p));
    // A bare "radar" is the network radar's while it is open too.
    let layer = LAYERS.iter().find(|(names, ..)| names.iter().any(|n| has(n) && !(radar_up && *n == "radar")));
    if layer.is_none() && !all {
        return None;
    }
    const OFF: &[&str] = &[
        "hide", "remove", "turn off", "switch off", "disable", "get rid of", "take away", "take off", "stop showing", "don't want", "dont want", "do not want", "don't need", "dont need", "no more", "not anymore",
        "anymore", "any more", "without", "enough", "clear", "nicht mehr", "keine", "weg", "ausblenden", "entferne", "aus", "off",
    ];
    const ON: &[&str] = &["show", "turn on", "switch on", "enable", "add", "display", "put", "bring back", "want to see", "let me see", "zeig", "zeige", "einblenden", "an", "ein", "on"];
    let toggle = has("toggle") || has("umschalten");
    let off = OFF.iter().any(|p| has(p));
    let on = ON.iter().any(|p| has(p));
    let earth = |action: &str| json!({ "view": "earth", "action": action });
    let Some(&(_, layer, show)) = layer else {
        return (off && !on).then(|| earth("hide_layers"));
    };
    // "Turn the trains back on" is on, whatever else it holds; "show me no more satellites" off.
    let hide = if toggle { crate::earth::layer_shown(layer).unwrap_or(false) } else if has("back on") || has("wieder an") { false } else if off { true } else if on { false } else { return None };
    if !hide {
        // Switching a layer on is only that when nothing else was asked: "show the flights over
        // New York", "show me Starlink" and "the traffic on the A8" are their own requests.
        const SAID: &[&str] = &[
            "the", "me", "my", "your", "our", "all", "again", "back", "please", "layer", "layers", "markers", "i", "want", "to", "see", "let", "too", "also", "now", "can", "you", "jai", "and", "it", "them", "toggle", "umschalten",
            "show", "turn", "switch", "on", "enable", "add", "display", "put", "bring", "zeig", "zeige", "mir", "die", "den", "das", "wieder", "bitte", "an", "ein", "einblenden", "schalte",
        ];
        let named: Vec<&str> = LAYERS.iter().find(|(_, l, _)| *l == layer).map_or(&[][..], |(n, ..)| *n).iter().flat_map(|n| n.split(' ')).collect();
        if words.iter().any(|w| !SAID.contains(w) && !named.contains(w)) || has("starlink") {
            return None;
        }
    }
    Some(if hide && layer == "labels" {
        earth("hide_labels")
    } else if hide {
        json!({ "view": "earth", "action": "hide_layer", "layer": layer })
    } else if layer == "satellites" {
        json!({ "view": "earth", "action": show, "group": "active" })
    } else {
        earth(show)
    })
}

/// `panel_command` on the cleaned sentence `s` (`text` is Sir's own, for the case of a place).
fn panel_args(text: &str, s: &str, up: PanelsUp) -> Option<Value> {
    let view = |name: &str, action: &str| json!({ "view": name, "action": action });
    let earth = |action: &str| view("earth", action);
    let at = |mut args: Value, place: &str| {
        // (A description, "the capital of France", keeps its case: it is no name to capitalise.)
        let p = place.trim();
        let p = p.strip_prefix("the ").unwrap_or(p);
        args["place"] = json!(if crate::earth::describes_a_place(p) { original_case(text, p) } else { spoken_place(text, place) });
        args
    };

    // ---- The Earth while it is up: its camera, its layers and its look.
    if up.earth {
        static ZOOM: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:zoom\s+(in|out)(?:\s+(?:a\s+bit|a\s+little|more|further|some|a\s+lot))?|(rein|hinein|raus|heraus)zoomen)$"));
        if let Some(c) = ZOOM.captures(s) {
            let inward = c.get(1).map_or_else(|| matches!(c.get(2).map(|m| m.as_str()), Some("rein" | "hinein")), |m| m.as_str() == "in");
            return Some(earth(if inward { "zoom_in" } else { "zoom_out" }));
        }
        static HOME: LazyLock<Regex> = LazyLock::new(|| {
            re(r"^(?:reset|recenter|re-center|recentre|re-centre)\s+(?:the\s+)?(?:globe|earth|view|map|camera)$|^zoom\s+(?:all\s+the\s+way\s+)?out\s+(?:all\s+the\s+way|completely|fully)$|^show\s+(?:me\s+)?the\s+whole\s+(?:globe|earth|world|planet)$")
        });
        if HOME.is_match(s) {
            return Some(earth("home"));
        }
        // (2026-11-09: "Can you make the globe spin?" went to the model, which asked the orb.)
        static SPIN: LazyLock<Regex> = LazyLock::new(|| {
            re(r"^(?:spin|rotate)\s+(?:the\s+)?(?:globe|earth|world|planet|it)$|^(stop)\s+(?:spinning|rotating)(?:\s+(?:the\s+)?(?:globe|earth|it))?$|^(?:make|let|have|get)\s+(?:the\s+)?(?:globe|earth|world|planet|it)\s+(?:spin|rotate|turn)(?:\s+(?:again|around|round))?$|^(?:start|keep)\s+(?:spinning|rotating)(?:\s+(?:the\s+)?(?:globe|earth|it))?$|^(?:lass|lasse)\s+(?:die\s+erde|den\s+globus|sie|ihn)\s+(?:drehen|rotieren)$")
        });
        // (2026-11-09: "Can you spin it in the other direction?" became a window swap, then an
        // orb status.)
        static SPIN_BACK: LazyLock<Regex> = LazyLock::new(|| {
            let globe = r"(?:the\s+)?(?:globe|earth|world|planet|it)";
            let other = r"(?:(?:in|into)\s+)?(?:the\s+)?(?:other|opposite|reverse|wrong|different)\s+(?:direction|way)(?:\s+(?:round|around))?";
            re(&format!(
                r"^(?:spin|rotate|turn)\s+{globe}\s+(?:{other}|backwards?|back|around|round|counter\s?clockwise|anti\s?clockwise|the\s+other\s+way\s+(?:round|around))$|^(?:make|let|have|get)\s+{globe}\s+(?:spin|rotate|turn)\s+(?:{other}|backwards?)$|^(?:spin|rotate|turn)\s+(?:{other}|backwards?)$|^reverse\s+(?:the\s+|its\s+)?(?:spin|rotation|direction)(?:\s+of\s+{globe})?$|^reverse\s+{globe}$|^(?:change|switch|flip)\s+(?:the\s+|its\s+)?(?:spin\s+|rotation\s+|spinning\s+)?direction(?:\s+of\s+{globe})?$|^(?:and\s+)?(?:now\s+)?(?:the\s+)?other\s+(?:direction|way)(?:\s+(?:round|around))?$|^(?:dreh|drehe)\s+(?:die\s+erde|den\s+globus|sie|ihn|es)\s+(?:andersherum|andersrum|anders\s+herum|rückwärts|in\s+die\s+andere\s+richtung)$|^(?:andersherum|andersrum|in\s+die\s+andere\s+richtung)(?:\s+drehen)?$"
            ))
        });
        if SPIN_BACK.is_match(s) {
            return Some(earth("spin_reverse"));
        }
        if let Some(c) = SPIN.captures(s) {
            return Some(earth(if c.get(1).is_some() { "stop_spin" } else { "spin" }));
        }
        if let Some(args) = earth_layer_switch(s, up.radar) {
            return Some(args);
        }
        // The camera leaned back over the ground (3D) or straight down again (2D), as Google
        // Earth does it: "switch to 3D", "tilt the map", "back to 2D", "top-down view".
        if let Some(leaned) = view_3d(s) {
            return Some(earth(if leaned { "show_3d" } else { "show_2d" }));
        }
        static BACK_TO_HOLO: LazyLock<Regex> = LazyLock::new(|| {
            re(r"^(?:switch|change|flip)\s+(?:it\s+)?back$|^(?:switch|change|flip|turn|go)\s+(?:it\s+)?back\s+to\s+(?:the\s+)?(?:normal|default|blue|original|old|holo|holographic|hologram)\s*(?:view|mode|globe|look)?$|^(?:(?:switch|change|go|flip|turn)\s+(?:it\s+)?(?:to|into)\s+(?:the\s+)?)?(?:holo|holographic|hologram)\s*(?:view|mode|globe|map)?$|^(?:normal|default)\s+(?:view|mode|globe)$|^turn\s+off\s+(?:the\s+)?satellite(?:\s+(?:view|mode|imagery))?$")
        });
        if BACK_TO_HOLO.is_match(s) {
            return Some(earth("show_holo"));
        }
    }

    // ---- The Earth: where Sir is, the terrain, the weather, a past day, the imagery, the tour.
    if let Some(args) = earth_panel_args(text, s, up) {
        return Some(args);
    }

    // ---- The Earth: satellite imagery, a place on it, its layers, the globe itself.
    static SATELLITE: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"^(?:(?:switch|change|go|flip|turn|swap|put|set)\s+(?:it\s+|the\s+(?:globe|earth|map|view)\s+)?(?:over\s+)?(?:to|into|on|in)\s+(?:the\s+)?)?satellite\s+(?:view|mode|imagery|images|map|layer|photos?|pictures?)$",
            r"|^(?:switch|change|go|flip|swap)\s+(?:it\s+)?(?:over\s+)?to\s+satellite$",
            r"|^(?:show|show\s+me|use|give\s+me|turn\s+on)\s+(?:me\s+)?(?:the\s+)?(?:real\s+)?satellite\s+(?:view|imagery|images|map|photos?|pictures?)$",
            r"|^(?:wechsel|wechsle|schalte?|geh|gehe)\s+(?:auf|in|zur|zu)\s+(?:die\s+)?(?:satellitenansicht|satellitenbild|satellitenmodus|satellitenbilder)(?:\s+um)?$|^satellitenansicht$",
        ))
    });
    if SATELLITE.is_match(s) {
        return Some(earth("show_satellite"));
    }
    static ON_GLOBE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:show|show\s+me|find|locate|point\s+(?:me\s+)?to|where\s+is|wheres|zeig|zeig\s+mir|zeige|zeige\s+mir|wo\s+ist|wo\s+liegt|finde)\s+(?:me\s+|mir\s+)?(.+?)\s+(?:on|auf)\s+(?:the|my|der|dem)\s+(?:globe|earth|world\s+map|erde|globus|weltkugel|weltkarte)$")
    });
    if let Some(c) = ON_GLOBE.captures(s) {
        if !not_a_place(&c[1]) {
            return Some(at(earth("show_place"), &c[1]));
        }
    }
    static QUAKES: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:show|show\s+me|display|find|list|mark|plot|pull\s+up|bring\s+up|check|look\s+for|search\s+for|zeig|zeig\s+mir|zeige|zeige\s+mir|suche|such)\s+(?:me\s+|mir\s+)?(?:all\s+|for\s+)?(?:the\s+|die\s+|alle\s+|nach\s+)?|(?:are|were)\s+there\s+(?:any\s+)?|(?:have|has)\s+there\s+been\s+(?:any\s+|an?\s+)?|(?:is|was)\s+there\s+(?:an?\s+|any\s+)?|any\s+|(?:gab|gibt)\s+es\s+(?:hier\s+)?(?:ein\s+|irgendwelche\s+)?)?(?:recent\s+|latest\s+|live\s+|current\s+|new\s+|big\s+|major\s+|strong\s+|aktuelle\s+|neue\s+)?(?:earthquakes?|quakes?|seismic\s+activity|erdbeben)(.*)$")
    });
    if let Some(c) = QUAKES.captures(s) {
        if let Some((place, radius)) = quake_area(&c[1]) {
            let mut args = earth("show_quakes");
            if let Some(place) = place {
                args = at(args, &place);
            }
            if let Some(km) = radius {
                args["radius_km"] = json!(km);
            }
            return Some(args);
        }
    }
    // "Track a flight that goes from Frankfurt to New York", "follow LH 400", "stop tracking".
    static TRACK: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"^(?:track|follow|find|show\s+me|show|verfolge?|finde?|zeig\s+mir|zeige\s+mir)\s+(?:me\s+|mir\s+)?(?:a|an|one|the|any|einen|ein|den)?\s*(?:live\s+|current\s+)?(?:flight|plane|aircraft|airplane|jet|flug|flieger|flugzeug)\s+(?:that\s+(?:goes|is\s+going|flies|is\s+flying|travels)\s+|going\s+|flying\s+|heading\s+|der\s+|das\s+|on\s+the\s+way\s+)?(?:from|von|out\s+of)\s+(.+?)\s+(?:to|nach|into)\s+(.+?)(?:\s+(?:fliegt|geht|right\s+now|now))?$",
            r"|^(?:track|follow|verfolge?)\s+(?:the\s+|den\s+)?(?:flight\s+|flug\s+)?([a-z]{2,3}\s?\d{1,4}[a-z]?)$",
        ))
    });
    if let Some(c) = TRACK.captures(s) {
        return Some(match c.get(3) {
            Some(flight) => json!({ "view": "earth", "action": "track_flight", "flight": flight.as_str() }),
            None => json!({ "view": "earth", "action": "track_flight", "from": spoken_place(text, &c[1]), "to": spoken_place(text, &c[2]) }),
        });
    }
    static UNTRACK: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:stop|end|quit)\s+(?:tracking|following)(?:\s+(?:the|that|this)\s+(?:flight|plane|aircraft))?$|^(?:hör|hoer)\s+auf\s+(?:den\s+flug\s+)?zu\s+verfolgen$"));
    if UNTRACK.is_match(s) && crate::flight_track::following().is_some() {
        return Some(earth("stop_tracking"));
    }
    // "Show me the capital of France": a place described, even before the Earth is up.
    static DESCRIBED: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:show\s+me|show|find|where\s+is|wheres|zeig\s+mir|zeige\s+mir|wo\s+ist|wo\s+liegt)\s+(?:the\s+|die\s+|der\s+|das\s+)?(.+?)(?:\s+(?:on|auf)\s+(?:the|my|der|dem)\s+(?:globe|earth|map|world\s+map|erde|globus|karte))?$"));
    if let Some(c) = DESCRIBED.captures(s).filter(|c| crate::earth::describes_a_place(&c[1]) && c[1].split(' ').count() <= 8) {
        return Some(at(earth("show_place"), &c[1]));
    }
    static FLIGHTS: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(show|show\s+me|display|turn\s+on|enable|track|add|put\s+up|zeig|zeig\s+mir|zeige|zeige\s+mir)\s+(?:me\s+|mir\s+)?(?:all\s+)?(?:the\s+|die\s+|alle\s+)?)?(live\s+|current\s+|real[\s-]?time\s+)?(?:flights|planes|aircraft|airplanes|air\s+traffic|flight\s+traffic|flugzeuge|flüge|fluege|flugverkehr)(?:\s+(?:over|above|near|around|in|über|ueber|bei|um)\s+(.+))?$|^(?:what|which)\s+(?:planes|aircraft|flights)\s+are\s+(?:flying\s+|in\s+the\s+air\s+)?(?:over|above|near|around)\s+(.+)$|^(flight\s+tracker|flight\s+radar|flightradar)$")
    });
    if let Some(c) = FLIGHTS.captures(s) {
        let place = c.get(3).or(c.get(4)).map(|m| m.as_str());
        if c.get(1).is_some() || c.get(2).is_some() || c.get(5).is_some() || place.is_some() || up.earth {
            return Some(match place.filter(|p| !not_a_place(p)) {
                Some(place) => at(earth("show_flights"), place),
                None => earth("show_flights"),
            });
        }
    }
    static NATURE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(show|show\s+me|display|turn\s+on|enable|track|mark|zeig|zeig\s+mir|zeige|zeige\s+mir)\s+(?:me\s+|mir\s+)?(?:all\s+)?(?:the\s+|die\s+|alle\s+)?)?(?:active\s+|current\s+|live\s+|tropical\s+|aktuelle\s+|recent\s+)?(storms?|hurricanes?|typhoons?|cyclones?|tropical\s+storms?|stürme|stuerme|wirbelstürme|hurrikans?|wildfires?|fires|bushfires?|forest\s+fires?|waldbrände|waldbraende|brände|braende)(?:\s+(?:on\s+the\s+(?:globe|earth|map)|right\s+now|today))?$")
    });
    if let Some(c) = NATURE.captures(s) {
        if c.get(1).is_some() || up.earth {
            let fire = ["fire", "brände", "braende", "waldbr"].iter().any(|w| c[2].contains(w));
            return Some(earth(if fire { "show_fires" } else { "show_storms" }));
        }
    }
    static ISS: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:show|show\s+me|where\s+is|wheres|track|find|locate|zeig|zeig\s+mir|zeige\s+mir|wo\s+ist)\s+(?:me\s+|mir\s+)?(?:the\s+|die\s+)?(?:iss|international\s+space\s+station|space\s+station|raumstation|internationale\s+raumstation)(?:\s+gerade)?$")
    });
    if ISS.is_match(s) {
        return Some(earth("show_iss"));
    }
    static EARTH: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:show|show\s+me|open|open\s+up|pull\s+up|bring\s+up|display|give\s+me|launch|put\s+up|zeig|zeig\s+mir|zeige|zeige\s+mir|öffne|oeffne|hol)\s+(?:me\s+|mir\s+)?(?:the\s+|a\s+|my\s+|die\s+|den\s+|das\s+)?(?:whole\s+|entire\s+|ganze\s+|holographic\s+|holo\s+|3d\s+)?)?(?:earth|planet\s+earth|planet|globe|world|erde|weltkugel|globus|earth\s+view|globe\s+view|holo\s*globe)(?:\s+(?:in|im|as|with)\s+(?:the\s+)?(satellite(?:\s+view|\s+mode|\s+imagery)?|satellitenansicht))?$")
    });
    if let Some(c) = EARTH.captures(s) {
        return Some(earth(if c.get(1).is_some() { "show_satellite" } else { "show" }));
    }
    if up.earth {
        static FLY_TO: LazyLock<Regex> = LazyLock::new(|| {
            re(r"^(?:fly|zoom|go|take\s+me|bring\s+me|move|jump|travel|head|pan|navigate)\s+(?:(?:me|us)\s+)?(?:(?:over|in|back|down|right)\s+)?(?:to|into|on|onto|towards?|over)\s+(.+)$|^(?:center|centre|focus)\s+(?:the\s+(?:globe|earth|map|view|camera)\s+)?(?:on|onto)\s+(.+)$|^(?:flieg|fliege|zoom|zoome|geh|gehe|spring|springe)\s+(?:nach|zu|zum|zur|auf|über|ueber)\s+(.+)$")
        });
        if let Some(c) = FLY_TO.captures(s) {
            let place = c.get(1).or(c.get(2)).or(c.get(3))?.as_str();
            if !not_a_place(place) {
                return Some(at(earth("show_place"), place));
            }
        }
        // "Show me where New York is", "where is Tokyo", "show me the USA" while the globe is up
        // (2026-11-09: the model opened the Earth again and flew nowhere).
        static WHERE_IS: LazyLock<Regex> = LazyLock::new(|| {
            re(r"^(?:show\s+me|show|tell\s+me|zeig\s+mir|zeige\s+mir)\s+where\s+(.+?)\s+(?:is|are|lies)$|^where\s+(?:is|are|lies)\s+(.+)$|^(?:zeig|zeige)\s+(?:mir\s+)?wo\s+(.+?)\s+(?:ist|liegt|liegen)$|^wo\s+(?:ist|liegt|liegen)\s+(.+)$|^(?:show\s+me|show|zeig\s+mir|zeige\s+mir|zoom\s+in\s+on|look\s+at)\s+(.+)$")
        });
        static NOT_ON_A_GLOBE: LazyLock<Regex> = LazyLock::new(|| {
            re(r"\b(?:picture|pictures|image|images|photo|photos|bild|bilder|model|3d|camera|cameras|webcam|street|video|videos|window|windows|weather|news|list|calendar|schedule|timetable|mail|emails?|files?|folder|settings|core|orb|board|radar|network|stats|features|history|console|theme|lab|mixer|song|music|playlist|departures?|abfahrten|reminders?|tasks?|todo|disk|drive|storage|telemetry|qr|game|games|flights?|planes?|quakes?|earthquakes?|storms?|fires?|satellites?|trains?|traffic|clouds?|labels|terrain|yesterday|today|tomorrow|tonight|last|ago|desktop|screen|monitor|logs?|options|wireless|wifi|wi-fi|wlan|devices|i|me|my|you|your|it|that|this|them|what|how|why|more|less|all|everything)\b")
        });
        if let Some(c) = WHERE_IS.captures(s) {
            let place = (1..=5).find_map(|i| c.get(i))?.as_str();
            // (And "zeig mir die Freiheitsstatue".)
            let place = ["the ", "die ", "der ", "das "].iter().find_map(|a| place.strip_prefix(a)).unwrap_or(place);
            if !not_a_place(place) && !NOT_ON_A_GLOBE.is_match(place) && place.split(' ').count() <= 4 {
                return Some(at(earth("show_place"), place));
            }
        }
    }

    // ---- The network radar.
    static SECURE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:is|are)\s+(?:my|the|our)\s+(?:home\s+)?(?:network|wifi|wi-fi|wlan|lan)\s+(?:secure|safe)$|^(?:is|are)\s+there\s+(?:any\s+)?(?:unknown|strange|suspicious|unrecogni[sz]ed|foreign)\s+devices\s+(?:on|in)\s+(?:my|the|our)\s+(?:network|wifi|wi-fi|wlan|lan)$|^ist\s+(?:mein|das|unser)\s+(?:netzwerk|wlan|heimnetz)\s+sicher$")
    });
    // ("Is there any security problem?" right after a sweep: the radar on screen is meant.)
    static SECURE_AFTER: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:is|are)\s+(?:there|it)\s+(?:any\s+)?(?:security\s+(?:problems?|issues?|risks?|threats?|holes?)|unknown\s+devices|strangers|intruders)$|^(?:any\s+)?(?:security\s+(?:problems?|issues?|risks?)|unknown\s+devices|intruders)$|^is\s+(?:it|everything|all\s+that)\s+(?:secure|safe)$|^(?:gibt\s+es|gibts)\s+(?:ein\s+|irgendwelche\s+)?(?:sicherheitsprobleme?|sicherheitsrisiken|unbekannte\s+geräte|unbekannte\s+geraete)$")
    });
    if SECURE.is_match(s) || (up.radar && SECURE_AFTER.is_match(s)) {
        return Some(view("network_radar", "check_security"));
    }
    static RADAR: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            // "Check the network", "scan my home network", "sweep the LAN".
            r"^(?:check|scan|sweep|map|radar|analy[sz]e|inspect|survey)\s+(?:my\s+|the\s+|our\s+)?(?:home\s+|local\s+|whole\s+|entire\s+)?(?:network|lan)(?:\s+(?:for\s+devices|devices|again))?$",
            // "Scan my wifi" (checking it is the connection's: `NETWORK`).
            r"|^(?:scan|sweep|map)\s+(?:my\s+|the\s+|our\s+)?(?:wifi|wi-fi|wlan)(?:\s+for\s+devices)?$",
            // "Who is on my network?", "what devices are connected to my wifi?".
            r"|^(?:who|whos|what|whats|which|how\s+many)\s+(?:devices?\s+|else\s+|other\s+devices\s+)?(?:is\s+|are\s+|s\s+)?(?:connected\s+to|on|in)\s+(?:my|the|our)\s+(?:home\s+)?(?:network|wifi|wi-fi|wlan|lan)$",
            // "Show me the devices on my network".
            r"|^(?:show|show\s+me|list|find|give\s+me)\s+(?:me\s+)?(?:all\s+)?(?:the\s+)?(?:devices|gadgets)\s+(?:on|in|connected\s+to)\s+(?:my|the|our)\s+(?:home\s+)?(?:network|wifi|wi-fi|wlan|lan)$",
            // "Open the network radar", "radar".
            r"|^(?:(?:open|open\s+up|show|show\s+me|pull\s+up|bring\s+up|display|launch|start|run|zeig|zeig\s+mir|zeige|zeige\s+mir|öffne|oeffne|starte)\s+(?:me\s+|mir\s+)?(?:the\s+|my\s+|das\s+|den\s+)?)?(?:network\s+radar|radar|netzwerkradar|netzwerk\s+radar|network\s+map|netzwerkkarte|wireless(?:\s+(?:networks?|devices))?|(?:wifi|wi-fi|wlan)\s+(?:networks?|devices|radar))$",
            // German: "scanne mein Netzwerk", "Netzwerk prüfen", "wer ist in meinem WLAN?".
            r"|^(?:scanne|scann|prüfe|prüf|pruefe|überprüfe|ueberpruefe|checke|durchsuche|analysiere)\s+(?:mein\s+|meine\s+|das\s+|unser\s+|unsere\s+)?(?:netzwerk|wlan|lan|heimnetz|heimnetzwerk)$",
            r"|^(?:netzwerk|wlan|heimnetz|heimnetzwerk)\s+(?:scannen|prüfen|pruefen|checken|überprüfen|ueberpruefen|analysieren)$",
            r"|^(?:wer|welche\s+geräte|welche\s+geraete|wie\s+viele\s+geräte|wie\s+viele\s+geraete)\s+(?:ist|sind)\s+(?:alles\s+|gerade\s+)?(?:in|im|mit)\s+(?:meinem\s+|unserem\s+|dem\s+)?(?:netzwerk|wlan|heimnetz|heimnetzwerk)(?:\s+verbunden)?$",
        ))
    });
    if RADAR.is_match(s) {
        return Some(view("network_radar", "show"));
    }

    // ---- A second sweep or scan of whichever is up ("rescan", "scan again").
    static RESCAN: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:rescan|re-scan|re\s+scan|scan\s+(?:it\s+)?again|sweep\s+again|run\s+(?:the\s+)?(?:scan|sweep|radar)\s+again|(?:nochmal|noch\s+mal|neu|erneut)\s+scannen|(?:rescan|refresh|redo)\s+(?:the\s+|my\s+)?(network|radar|lan|wifi|disk|drive|disk\s+map|storage|scan|sweep))(?:\s+(?:the\s+|my\s+)?(network|radar|lan|wifi|disk|drive|disk\s+map|storage))?$")
    });
    if let Some(c) = RESCAN.captures(s) {
        let object = c.get(1).or(c.get(2)).map_or("", |m| m.as_str());
        let disk = matches!(object, "disk" | "drive" | "disk map" | "storage");
        let radar = matches!(object, "network" | "radar" | "lan" | "wifi");
        if disk || (!radar && up.disk && !up.radar) {
            return Some(view("disk_map", "rescan"));
        }
        if radar || up.radar {
            return Some(view("network_radar", "rescan"));
        }
    }

    // ---- The disk map while it is up: one folder back out ("go back" stays the music's).
    if up.disk {
        static FOLDER_UP: LazyLock<Regex> = LazyLock::new(|| {
            re(r"^(?:go\s+)?up\s+(?:a|one)\s+(?:folder|level|directory)$|^go\s+(?:back\s+)?up$|^(?:go\s+)?(?:one\s+|a\s+)?(?:folder|level|directory)\s+up$|^(?:go\s+)?back\s+out(?:\s+of\s+(?:this|the)\s+folder)?$|^(?:go\s+)?(?:back\s+)?to\s+the\s+parent\s+(?:folder|directory)$|^(?:einen\s+|eine\s+)?(?:ordner|ebene)\s+(?:hoch|nach\s+oben|zurück|zurueck)$")
        });
        if FOLDER_UP.is_match(s) {
            return Some(view("disk_map", "back"));
        }
    }

    // ---- The disk map.
    static DISK_MAP: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:open|open\s+up|show|show\s+me|pull\s+up|bring\s+up|display|launch|start|give\s+me|zeig|zeig\s+mir|zeige|zeige\s+mir|öffne|oeffne)\s+(?:me\s+|mir\s+)?(?:the\s+|my\s+|a\s+|die\s+|meine\s+)?)?(?:disk|drive|storage|space|festplatten|laufwerks)[\s-]*(?:map|tree\s*map|karte)(?:\s+(?:of|for|von)\s+(.+))?$|^tree\s*map$")
    });
    if let Some(c) = DISK_MAP.captures(s) {
        let mut args = view("disk_map", "show");
        if let Some(path) = c.get(1).and_then(|m| disk_path(m.as_str())) {
            args["path"] = json!(path);
        }
        return Some(args);
    }
    static SPACE_HOG: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"^(?:what|whats|which\s+(?:files|folders|programs|apps)|what\s+(?:files|folders|programs|apps))\s+(?:is\s+|are\s+|s\s+)?(?:taking|eating|using|filling|hogging|wasting|occupying|clogging|chewing)\s+(?:up\s+)?(.+)$",
            r"|^where\s+(?:has|did|is|does|have)\s+(?:all\s+)?(?:of\s+)?(?:my|the)\s+(.+?)\s+(?:gone|go|going|went)$",
            r"|^why\s+is\s+(?:my|the)\s+(.+?)\s+(?:so\s+|almost\s+|nearly\s+|completely\s+)?full$",
            r"|^(?:my|the)\s+(.+?)\s+is\s+(?:so\s+|almost\s+|nearly\s+|completely\s+|totally\s+)?full$",
            r"|^(?:how\s+(?:can|do|could)\s+i|help\s+me)\s+(?:to\s+)?(?:free|clear|make)\s+(?:up\s+)?(?:some\s+)?(.+)$",
            r"|^(?:map|analy[sz]e|visuali[sz]e)\s+(?:my\s+|the\s+)?(.+)$",
            r"|^(?:analysiere|visualisiere)\s+(?:meine\s+|die\s+|mein\s+|das\s+)?(.+)$",
            r"|^was\s+(?:belegt|frisst|verbraucht|nimmt|füllt|fuellt|blockiert)\s+(.+)$",
            r"|^wo\s+ist\s+(?:mein|der|meine|die)\s+(?:ganzer\s+|ganze\s+)?(.+?)\s+(?:hin|geblieben)$",
            r"|^warum\s+ist\s+(?:meine|die|mein|das)\s+(.+?)\s+(?:so\s+|fast\s+)?voll$",
        ))
    });
    if let Some(c) = SPACE_HOG.captures(s) {
        let tail = c.iter().skip(1).flatten().next()?.as_str();
        // "What is using my disk?" is the disk's activity (Task Manager's 100 %), not its space.
        static SPACE_WORD: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:space|storage|room|platz|speicherplatz)\b"));
        let activity = s.contains(" using ") && !SPACE_WORD.is_match(tail);
        if names_storage(tail) && !activity {
            let mut args = view("disk_map", "show");
            if let Some(path) = disk_path(tail) {
                args["path"] = json!(path);
            }
            return Some(args);
        }
    }
    None
}

/// The Earth beyond its live layers (`panel_args`): "where am I?" / "locate me" (the "YOU"
/// marker), "show me the mountains (of Nepal)", "exaggerate the terrain", "show the rain radar",
/// "animate the radar", "show me the clouds on 1 March 2024", "what did Europe look like
/// yesterday?", "switch to Sentinel imagery", "show the country borders", "give me a world tour",
/// "what's happening in the world?", "stop the tour"; English and German. A bare "where am I?"
/// with the Earth down stays the location engine's (it answers with the street address).
fn earth_panel_args(text: &str, s: &str, up: PanelsUp) -> Option<Value> {
    let earth = |action: &str| json!({ "view": "earth", "action": action });
    let today = chrono::Local::now().date_naive();
    // A day before today in Sir's words ("yesterday", "1 March 2024"; today is the live imagery).
    let past_day = |words: &str| crate::earth_sources::parse_date(words, today).is_some_and(|d| d < today);
    // "the world", "die Erde": the whole globe, no place to fly to.
    static WHOLE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:the\s+|die\s+|den\s+)?(?:whole\s+|entire\s+|ganze\s+)?(?:earth|world|globe|planet|map|erde|welt|globus|weltkugel|karte)$"));
    // A place (Sir's casing), none for the globe itself; None when it is no place at all.
    let place_of = |words: &str| -> Option<Option<String>> {
        let w = words.trim();
        // ("in der Schweiz": the article is no part of the name.)
        let w = ["der ", "die ", "das ", "dem ", "den "].iter().find_map(|a| w.strip_prefix(a)).unwrap_or(w);
        if WHOLE.is_match(w) {
            return Some(None);
        }
        (!not_a_place(w)).then(|| Some(spoken_place(text, w)))
    };

    // Where Sir is.
    static LOCATE: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"^(?:locate|pinpoint)\s+me(?:\s+on\s+(?:the\s+)?(?:map|globe|earth|world\s+map))?$",
            r"|^(?:find|show|mark|put)\s+me\s+on\s+(?:the\s+)?(?:map|globe|earth|world\s+map)$",
            r"|^(?:show|show\s+me|tell\s+me)\s+where\s+i\s+am(?:\s+right)?(?:\s+now)?(?:\s+on\s+(?:the\s+)?(?:map|globe|earth|world\s+map))?$",
            r"|^(?:show|show\s+me|mark|put)\s+(?:me\s+)?my\s+(?:location|position)(?:\s+on\s+(?:the\s+)?(?:map|globe|earth|world\s+map))?$",
            r"|^where\s+am\s+i\s+on\s+(?:the\s+)?(?:map|globe|earth|world\s+map|planet)$",
            // (2026-11-09: "Can you show me where I live?" fetched the weather.)
            r"|^(?:show|show\s+me|tell\s+me)\s+where\s+i\s+(?:live|stay|am\s+living)(?:\s+on\s+(?:the\s+)?(?:map|globe|earth|world\s+map))?$|^(?:zeig|zeige)\s+(?:mir\s+)?wo\s+ich\s+wohne(?:\s+auf\s+der\s+(?:karte|erde))?$",
            r"|^(?:zeig|zeige)\s+(?:mir\s+)?wo\s+ich\s+bin(?:\s+auf\s+der\s+(?:karte|erde|weltkarte))?$",
            r"|^(?:finde|orte|lokalisiere)\s+mich(?:\s+auf\s+der\s+(?:karte|erde))?$",
            r"|^(?:zeig|zeige)\s+(?:mir\s+)?meinen\s+(?:standort|aufenthaltsort|ort)(?:\s+auf\s+der\s+(?:karte|erde))?$",
            r"|^wo\s+bin\s+ich\s+auf\s+(?:der\s+(?:karte|erde|weltkarte)|dem\s+globus)$",
        ))
    });
    static WHERE_AM_I: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:where\s+am\s+i(?:\s+right)?(?:\s+now)?|(?:whats|what\s+is|wheres)\s+my\s+(?:location|position)|wo\s+bin\s+ich(?:\s+jetzt)?)$"));
    if LOCATE.is_match(s) || (up.earth && WHERE_AM_I.is_match(s)) {
        return Some(earth("show_my_location"));
    }

    // The terrain: shaded relief, the peaks named; over a place when one is named.
    static TERRAIN: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:show|show\s+me|display|turn\s+on|put\s+up|bring\s+up|pull\s+up|give\s+me|zeig|zeig\s+mir|zeige|zeige\s+mir)\s+(?:me\s+|mir\s+)?(?:the\s+|die\s+|das\s+|den\s+)?(?:shaded\s+)?(terrain(?:\s+(?:view|map|layer))?|relief|elevation|topography|hillshade|mountains|mountain\s+ranges?|peaks|mountain\s+peaks|gelände|gelaende|topografie|topographie|berge|gebirge|gipfel)(?:\s+(?:of|in|on|around|near|over|von|im|um|bei|der|des)\s+(.+))?$")
    });
    if let Some(c) = TERRAIN.captures(s) {
        let mountains = ["mountain", "peak", "berge", "gebirge", "gipfel"].iter().any(|w| c[1].contains(w));
        let mut args = earth(if mountains { "show_mountains" } else { "show_terrain" });
        match c.get(2).map(|m| place_of(m.as_str())) {
            Some(None) => return None,
            Some(Some(Some(place))) => args["place"] = json!(place),
            _ => {}
        }
        return Some(args);
    }
    // The houses in 3D: block models at street level; over a place when one is named.
    static BUILDINGS: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:show|show\s+me|display|turn\s+on|put\s+up|bring\s+up|pull\s+up|give\s+me|zeig|zeig\s+mir|zeige|zeige\s+mir)\s+(?:me\s+|mir\s+)?(?:the\s+|die\s+)?(?:3d\s+)?(?:buildings|houses|gebäude|gebaeude|häuser|haeuser)(?:\s+(?:of|in|around|near|von|im|um|bei|der|des)\s+(.+?))??(?:\s+(?:in|as|als)\s+3d)?$")
    });
    if let Some(c) = BUILDINGS.captures(s) {
        let mut args = earth("show_buildings");
        match c.get(1).map(|m| place_of(m.as_str())) {
            Some(None) => return None,
            Some(Some(Some(place))) => args["place"] = json!(place),
            _ => {}
        }
        return Some(args);
    }
    static EXAGGERATE: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"^(?:exaggerate|amplify|boost|stretch|enhance)\s+(?:the\s+)?(?:terrain|relief|elevation|mountains|heights?|topography)$",
            r"|^(?:make|let)\s+(?:the\s+)?(?:mountains|terrain|relief|peaks)\s+(?:look\s+)?(?:taller|higher|bigger|steeper|more\s+dramatic|stand\s+out(?:\s+more)?|pop)$",
            r"|^(?:exaggerated|amplified)\s+(?:terrain|relief|elevation)$",
            r"|^(?:überhöhe|ueberhoehe|übertreibe|uebertreibe|verstärke|verstaerke)\s+(?:das\s+|die\s+)?(?:gelände|gelaende|relief|berge|höhen|hoehen)$",
            r"|^(?:mach|mache)\s+(?:die\s+)?berge\s+(?:höher|hoeher|größer|groesser|steiler)$",
        ))
    });
    if EXAGGERATE.is_match(s) {
        return Some(earth("exaggerate_terrain"));
    }

    // The weather: the rain radar (its loop of the past two hours) and the clouds.
    static ANIMATE: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"^(?:animate|play|loop)\s+(?:the\s+)?(?:weather\s+|rain\s+)?radar(?:\s+(?:loop|animation))?$",
            r"|^(?:show|show\s+me|play)\s+(?:me\s+)?(?:the\s+)?(?:rain|radar|weather\s+radar|rain\s+radar|precipitation)\s+(?:(?:of|from|over|for|in)\s+)?the\s+(?:past|last)\s+(?:two|2|few|couple\s+of|couple)\s+hours$",
            r"|^(?:show|show\s+me)\s+(?:me\s+)?how\s+the\s+rain\s+(?:has\s+)?moved$",
            r"|^(?:regenradar|wetterradar|radar)\s+(?:animieren|abspielen)$",
            r"|^(?:spiel|spiele|animiere)\s+(?:das\s+)?(?:regenradar|wetterradar|radar)(?:\s+ab)?$",
        ))
    });
    if ANIMATE.is_match(s) {
        return Some(earth("animate_radar"));
    }
    static RAIN: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"^(?:(?:show|show\s+me|display|turn\s+on|put\s+up|bring\s+up|pull\s+up|give\s+me|zeig|zeig\s+mir|zeige|zeige\s+mir)\s+(?:me\s+|mir\s+)?(?:the\s+|das\s+|den\s+)?)?(?:live\s+|current\s+|latest\s+|aktuelle\s+|aktuelles\s+)?",
            r"(?:weather\s+radar|rain\s+radar|precipitation\s+radar|precipitation\s+map|rain\s+map|regenradar|wetterradar|niederschlagsradar)(\s+(?:loop|animation))?(?:\s+(?:on\s+the\s+(?:globe|earth|map)|right\s+now|now|jetzt))?$",
            r"|^where\s+is\s+it\s+raining(?:\s+right)?(?:\s+now)?$|^wo\s+regnet\s+es(?:\s+jetzt)?$",
        ))
    });
    if let Some(c) = RAIN.captures(s) {
        return Some(earth(if c.get(1).is_some() { "animate_radar" } else { "show_radar" }));
    }
    static CLOUDS: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"^(?:(show|show\s+me|display|turn\s+on|put\s+up|bring\s+up|pull\s+up|give\s+me|zeig|zeig\s+mir|zeige|zeige\s+mir)\s+(?:me\s+|mir\s+)?(?:the\s+|die\s+|den\s+)?)?(?:live\s+|current\s+|latest\s+|todays\s+|aktuelle\s+|aktuellen\s+)?",
            r"(?:clouds|cloud\s+cover|cloud\s+layer|cloud\s+map|infrared(?:\s+(?:clouds|satellite|imagery|images))?|satellite\s+clouds|wolken|wolkendecke|bewölkung|bewoelkung)",
            r"(?:\s+(?:on|from|of|for|at|as\s+of|vom|von|am)\s+(.+?)|\s+((?:the\s+day\s+before\s+)?yesterday|(?:vor)?gestern|last\s+week|letzte\s+woche|\d+\s+days\s+ago))?(?:\s+(?:on\s+the\s+(?:globe|earth|map)|right\s+now|now|today|jetzt|heute))?$",
        ))
    });
    if let Some(c) = CLOUDS.captures(s) {
        if c.get(1).is_some() || up.earth {
            let mut args = earth("show_clouds");
            if let Some(day) = c.get(2).or(c.get(3)).map(|m| m.as_str()).filter(|d| past_day(d)) {
                args["date"] = json!(day);
            }
            return Some(args);
        }
    }

    // A past day: NASA's imagery of it ("what did Europe look like yesterday?").
    static LOOKED: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:what|how)\s+did\s+(.+?)\s+look(?:\s+like)?\s+(.+)$|^wie\s+sah\s+(.+?)\s+((?:vor)?gestern|letzte\s+woche|am\s+.+|vor\s+\d+\s+tagen)\s+aus$")
    });
    static ON_DAY: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"^(?:show|show\s+me|zeig|zeig\s+mir|zeige|zeige\s+mir)\s+(?:me\s+|mir\s+)?(.+?)\s+(?:on|from|as\s+of|as\s+it\s+(?:was|looked)\s+on|am|vom|von)\s+(.+)$",
            r"|^(?:show|show\s+me|zeig\s+mir|zeige\s+mir)\s+(?:me\s+|mir\s+)?(.+?)\s+((?:the\s+day\s+before\s+)?yesterday|last\s+week|\d+\s+days\s+ago|(?:vor)?gestern|letzte\s+woche)$",
        ))
    });
    let looked = LOOKED.captures(s).map(|c| (c.get(1).or(c.get(3)).map_or("", |m| m.as_str()).to_string(), c.get(2).or(c.get(4)).map_or("", |m| m.as_str()).to_string(), true));
    let on_day = || ON_DAY.captures(s).map(|c| (c.get(1).or(c.get(3)).map_or("", |m| m.as_str()).to_string(), c.get(2).or(c.get(4)).map_or("", |m| m.as_str()).to_string(), false));
    if let Some((what, day, asked)) = looked.or_else(on_day) {
        // ("Show me my emails from yesterday" is no place: shown only for the globe itself, a
        // continent, a known city, or any place he names on the Earth that is up.)
        let what = what.trim();
        let known = WHOLE.is_match(what) || crate::earth::continent(what).is_some() || crate::earth::gazetteer(what).is_some();
        static NOT_A_SIGHT: LazyLock<Regex> = LazyLock::new(|| {
            re(concat!(
                r"^(?:my|your|our|his|her|their|all|meine?n?|deine?n?|unsere?n?|alle)\b|^(?:you|i|me|he|she|it|we|they|that|this|du|ich|er|sie|es|wir|ihr)$",
                r"|\b(?:flights?|planes?|aircraft|quakes?|earthquakes?|storms?|fires?|wildfires?|iss|radar|rain|clouds?|weather|wetter|news|nachrichten|photos?|pictures?|images?|bilder|fotos|e-?mails?|mails?|messages?|videos?|calendar|kalender|notes?|tasks?|logs?|files?|dateien|screenshots?|history|verlauf|stats|statistics|charts?|graphs?|results?|scores?|games?|stocks?|prices?|schedule|stundenplan|homework|hausaufgaben)\b",
            ))
        });
        if past_day(&day) && (known || ((asked || up.earth) && !NOT_A_SIGHT.is_match(what))) {
            if let Some(place) = place_of(what) {
                let mut args = earth("show_date");
                args["date"] = json!(day.trim());
                if let Some(place) = place {
                    args["place"] = json!(place);
                }
                return Some(args);
            }
        }
    }

    // The tour of what is happening (the week's strongest quake, the storms, the fires).
    static TOUR: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"^(?:give\s+me|take\s+me\s+on|start|begin|do|run|show\s+me|lets\s+do|go\s+on)\s+(?:a\s+|the\s+|your\s+)?(?:world\s+tour|tour\s+of\s+the\s+world|tour\s+around\s+the\s+world|global\s+tour|world\s+briefing|globe\s+tour)$",
            r"|^(?:world\s+tour|tour\s+the\s+world)$",
            r"|^(?:whats|what\s+is)\s+(?:happening|going\s+on)\s+(?:in|around|across)\s+the\s+world(?:\s+(?:right\s+now|today|now))?$",
            r"|^(?:was\s+passiert|was\s+ist\s+los|was\s+geht\s+vor)\s+(?:jetzt\s+|heute\s+)?(?:in\s+der|auf\s+der)\s+welt(?:\s+(?:jetzt|heute))?$",
            r"|^(?:mach|mache|starte|gib\s+mir|zeig\s+mir)\s+(?:eine\s+|die\s+)?(?:welttour|weltreise|weltrundschau|tour\s+um\s+die\s+welt)$|^welttour$",
        ))
    });
    if TOUR.is_match(s) {
        return Some(earth("show_world_tour"));
    }

    // The rest only means the Earth while it is up.
    if !up.earth {
        // ("Show the country borders" opens it; "show the labels" is anybody's.)
        static BORDERS: LazyLock<Regex> = LazyLock::new(|| {
            re(r"^(?:show|show\s+me|display|zeig|zeig\s+mir|zeige|zeige\s+mir)\s+(?:me\s+|mir\s+)?(?:the\s+|die\s+|alle\s+)?(?:country\s+borders|national\s+borders|borders\s+of\s+the\s+countries|landesgrenzen|ländergrenzen|laendergrenzen)(?:\s+on\s+the\s+(?:globe|earth|map))?$")
        });
        return BORDERS.is_match(s).then(|| earth("show_labels"));
    }
    static STOP_TOUR: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:stop|end|cancel|abort|finish|quit|exit|break\s+off)\s+(?:the\s+)?(?:world\s+)?tour$|^(?:enough|thats\s+enough)\s+(?:of\s+the\s+)?tour$|^(?:welt)?tour\s+(?:beenden|stoppen|abbrechen)$|^(?:beende|stoppe?|brich)\s+die\s+(?:welt)?tour(?:\s+ab)?$")
    });
    if STOP_TOUR.is_match(s) {
        return Some(earth("stop_tour"));
    }
    static FLATTEN: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:flatten|unexaggerate|normali[sz]e)\s+(?:the\s+)?(?:terrain|relief|elevation|mountains)$|^(?:terrain|relief)\s+(?:back\s+)?to\s+(?:normal|scale)$|^(?:true\s+to\s+scale|real\s+scale|normal)\s+(?:terrain|relief)$")
    });
    if FLATTEN.is_match(s) {
        return Some(earth("flatten_terrain"));
    }
    static LABELS: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"^(?:(show|show\s+me|display|turn\s+on|switch\s+on|enable|add|put\s+up|bring\s+back|zeig|zeig\s+mir|zeige|zeige\s+mir|blende)|(hide|remove|turn\s+off|switch\s+off|disable|get\s+rid\s+of|entferne))",
            r"\s+(?:me\s+|mir\s+)?(?:the\s+|die\s+|alle\s+)?(?:country\s+borders|national\s+borders|borders|labels|place\s+names|city\s+names|names|landesgrenzen|ländergrenzen|laendergrenzen|grenzen|beschriftungen|ortsnamen|namen)(?:\s+(?:on\s+the\s+(?:globe|earth|map)|ein|an|aus))?$",
        ))
    });
    if let Some(c) = LABELS.captures(s) {
        let off = c.get(2).is_some() || s.ends_with(" aus");
        return Some(earth(if off { "hide_labels" } else { "show_labels" }));
    }
    static HIDE_OVERLAY: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:hide|remove|turn\s+off|switch\s+off|disable|get\s+rid\s+of|blende|entferne)\s+(?:the\s+|die\s+|das\s+|den\s+)?(weather\s+radar|rain\s+radar|radar|regenradar|wetterradar|clouds|cloud\s+cover|infrared|wolken|terrain|relief|mountains|peaks|hillshade|gelände|gelaende|berge|3d\s+buildings|buildings|houses|gebäude|gebaeude|häuser|haeuser)(?:\s+(?:layer|overlay))?(?:\s+aus)?$")
    });
    if let Some(c) = HIDE_OVERLAY.captures(s) {
        let layer = match &c[1] {
            // (A bare "radar" with the network radar up is that one's.)
            "radar" if up.radar => return None,
            l if l.contains("radar") => "radar",
            l if ["building", "house", "gebäude", "gebaeude", "häuser", "haeuser"].iter().any(|w| l.contains(w)) => "buildings",
            "clouds" | "cloud cover" | "infrared" | "wolken" => "clouds",
            _ => "terrain",
        };
        return Some(json!({ "view": "earth", "action": "hide_layer", "layer": layer }));
    }
    static IMAGERY: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"^(?:(?:switch|change|go|flip|swap|turn)\s+(?:the\s+)?(?:imagery\s+|map\s+|globe\s+|earth\s+)?(?:over\s+|back\s+)?to|use|show|show\s+me|give\s+me)\s+(?:me\s+)?(?:the\s+)?",
            r"(sentinel(?:\s*-?\s*2)?|esri|nasa|blue\s+marble|eox|copernicus)(?:\s+(?:imagery|images|maps?|tiles|view|layer|photos|cloudless))?$",
            r"|^(?:wechsel|wechsle|schalte?|geh|gehe)\s+(?:auf|zu|zur)\s+(?:die\s+|den\s+)?(sentinel(?:\s*-?\s*2)?|esri|nasa|blue\s+marble)(?:\s*-?\s*(?:bilder|ansicht|karte))?(?:\s+um)?$",
        ))
    });
    if let Some(c) = IMAGERY.captures(s) {
        let mut args = earth("set_imagery");
        args["imagery"] = json!(c.get(1).or(c.get(2)).map_or("", |m| m.as_str()));
        return Some(args);
    }
    static TODAY: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:go\s+|switch\s+|come\s+)?back\s+to|return\s+to|show\s+(?:me\s+)?)\s*(?:the\s+)?(?:today|todays\s+imagery|present|now|live\s+imagery|live\s+view|current\s+imagery)$|^(?:zurück|zurueck)\s+(?:zu|nach)\s+(?:heute|jetzt)$")
    });
    TODAY.is_match(s).then(|| earth("show_today"))
}

/// A place on the Earth, or something else that follows "go to" ("page two", "sleep", "Discord").
fn not_a_place(place: &str) -> bool {
    static NOT: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:page|seite|result|tab|slide|step|sleep|bed|standby|desktop|monitor|screen|bildschirm|settings|einstellungen|next|previous|last|first|top|bottom|end|start|beginning|full\s*screen|fullscreen|orb|board|chat|console|it|that|this|there|my\s+(?:desktop|screen|monitor|settings|files?|folder|documents|downloads)|the\s+(?:next|previous|last|first|top|bottom|end|start|beginning|board|chat|orb|console|settings|page|tab|desktop|screen|monitor)\b|\d)")
    });
    let p = place.trim();
    p.is_empty() || p.split_whitespace().count() > 6 || NOT.is_match(p) || known_app(p)
}

/// The place as Sir said it ("tokyo" in his words as "Tokyo"); "this area" and "here" are the
/// point in view, "me" is where he is.
fn spoken_place(text: &str, place: &str) -> String {
    let p = place.trim();
    let p = p.strip_prefix("the ").unwrap_or(p).trim();
    match p {
        "this area" | "area" | "this region" | "region" | "here" | "around here" | "nearby" | "this place" | "there" | "that area" | "that place" | "this spot"
        | "dieser gegend" | "der gegend" | "hier" | "dieser region" => "here".to_string(),
        "me" | "my area" | "my location" | "where i am" | "my city" | "my town" | "my place" | "mir" | "meiner nähe" | "meiner naehe" => "my location".to_string(),
        _ => {
            let cased = original_case(text, p);
            if cased.chars().next().is_some_and(char::is_lowercase) {
                // Whisper writes "new york" in lower case at times.
                cased
                    .split(' ')
                    .map(|w| {
                        let mut letters = w.chars();
                        letters.next().map_or_else(String::new, |first| first.to_uppercase().chain(letters).collect())
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                cased
            }
        }
    }
}

/// What follows "earthquakes": nothing, a time ("recently"), a place ("in this area", "near
/// Tokyo") or a radius ("within 300 km of Berlin"). None: it was about something else
/// ("earthquake safety tips").
fn quake_area(tail: &str) -> Option<(Option<String>, Option<f64>)> {
    static HAPPENED: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(?:that|which)\s+(?:have\s+|had\s+)?(?:happened|occurred|hit|struck|were)\b"));
    static WHEN: LazyLock<Regex> = LazyLock::new(|| {
        re(r"\s*\b(?:recently|lately|today|yesterday|this\s+week|this\s+month|in\s+the\s+(?:last|past)\s+(?:\w+\s+)?(?:days?|weeks?|hours?|months?)|heute|gestern|kürzlich|diese\s+woche)\s*$")
    });
    static RADIUS: LazyLock<Regex> = LazyLock::new(|| {
        re(r"\s*\b(?:within|in\s+a|in\s+einem\s+umkreis\s+von)\s+(\d{1,5})\s*(km|kilometers?|kilometres?|kilometer|miles?|mi)(?:\s+radius)?(?:\s+(?:of|from|around|um))?")
    });
    static WHERE: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(?:in|near|around|at|close\s+to|by|bei|um|nahe|in\s+der\s+nähe\s+von|rund\s+um)\s+(.+)$"));
    let mut t = HAPPENED.replace(tail, "").to_string();
    t = WHEN.replace(&t, "").to_string();
    let mut radius = None;
    if let Some(c) = RADIUS.captures(&t) {
        let n: f64 = c[1].parse().ok()?;
        radius = Some(if c[2].starts_with('m') { (n * 1.609).round() } else { n });
        let whole = c.get(0)?.range();
        let rest = t[whole.end..].trim().to_string();
        t = if rest.is_empty() { t[..whole.start].to_string() } else { format!("{} in {rest}", &t[..whole.start]) };
    }
    let t = t.trim();
    if t.is_empty() {
        return Some((None, radius));
    }
    let place = WHERE.captures(t)?.get(1)?.as_str().trim().to_string();
    let place = WHEN.replace(&place, "").trim().to_string();
    if place.is_empty() || place.split_whitespace().count() > 6 {
        return None;
    }
    Some((Some(place), radius))
}

/// The tail names a disk, a drive or a folder ("my D drive", "all my space", "my downloads
/// folder"), not memory, time or the network ("analyse my desktop" is the screen's).
fn names_storage(tail: &str) -> bool {
    static STORAGE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"\b(?:space|storage|disk|disks|drive|drives|ssd|hdd|hard\s+drive|hard\s+disk|festplatte|speicherplatz|platz|laufwerk|partition|folder|ordner)\b")
    });
    static OTHER: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:ram|memory|arbeitsspeicher|cpu|gpu|vram|bandwidth|internet|network|netzwerk|time|zeit|calendar|schedule|day|tag|screen|bildschirm|monitor)\b"));
    tail.split_whitespace().count() <= 9 && STORAGE.is_match(tail) && !OTHER.is_match(tail)
}

/// The drive or folder a sentence names: "my D drive" → `D:\`, "on E" → `E:\`, "my downloads"
/// → `Downloads` (the disk map finds his known folders by name).
fn disk_path(s: &str) -> Option<String> {
    static LETTER: LazyLock<Regex> = LazyLock::new(|| re(r"\b([b-z])\s+(?:drive|laufwerk|partition)\b|\b(?:drive|laufwerk|partition)\s+([b-z])\b|\b(?:on|in|auf)\s+([b-z])$"));
    if let Some(c) = LETTER.captures(s) {
        let letter = c.iter().skip(1).flatten().next()?.as_str().to_ascii_uppercase();
        return Some(format!("{letter}:\\"));
    }
    static FOLDER: LazyLock<Regex> = LazyLock::new(|| re(r"\b(downloads|documents|desktop|videos|pictures|music|appdata|temp)\b"));
    let c = FOLDER.captures(s)?;
    let mut letters = c[1].chars();
    letters.next().map(|first| first.to_uppercase().chain(letters).collect())
}

/// The JAI view a phrase names (`opening`: Sir wants a window, not a reading in the chat).
/// "A few random windows", "three random views", "some windows of your choice": his own choice
/// of views, and of how many unless a number is said. A named or a plain window is none of his
/// ("two chrome windows", "a new window").
fn random_views(object: &str) -> Option<Value> {
    static RANDOM: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"^(?:me\s+|mir\s+)?(?:(?P<n>\d+|a|an|one|two|three|four|five|six|seven|eine?|eins|zwei|drei|vier|fünf|fuenf|sechs|sieben)\s+|(?P<some>a few|some|several|a couple of|a couple|a bunch of|a handful of|ein paar|einige|mehrere)\s+)?",
            r"(?:(?P<random>random|randomly chosen|arbitrary|zufällige[sn]?|zufaellige[sn]?|beliebige[sn]?|irgendwelche)\s+)?",
            r"(?:windows|window|views|view|fenster|ansichten)",
            r"(?P<choice>\s+(?:of your (?:own )?(?:choice|choosing)|you (?:choose|pick|decide)|your choice|(?:nach )?deiner wahl))?(?:\s+for me|\s+für mich)?$",
        ))
    });
    let c = RANDOM.captures(object.trim())?;
    if c.name("random").is_none() && c.name("choice").is_none() && c.name("some").is_none() {
        return None;
    }
    let mut args = json!({ "view": "random" });
    if let Some(n) = c.name("n").and_then(|m| count_word(m.as_str()).or(Some(1))) {
        args["count"] = json!(n);
    }
    Some(args)
}

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
        "simulation lab" | "sim lab" | "simlab" | "simulationlab" | "simulation labor" | "physics lab" | "stark simulation lab" | "lab" | "holomat" | "holo mat"
        | "holomat lab" | "simulationslabor" | "simulations labor" | "physiklabor" | "labor" | "math lab" | "chem lab" => Some("simulation_lab"),
        "qr code" | "qr" | "qr code for my phone" | "pairing qr" | "pairing code" | "mobile link" | "mobile satellite" | "satellite" | "phone link"
        | "qr code für mein handy" | "qr-code" => Some("mobile_qr"),
        "gym logs" | "gym log" | "gym" | "gym coach" | "workout logs" | "workout log" | "workout coach" | "gym tracker" | "finance tracker" | "finances"
        | "finance" | "finanzen" | "gym and finances" | "workout and finance" | "workouts" => Some("workout_finance"),
        "contacts" | "contact list" | "kontakte" | "contact book" => Some("contacts"),
        "schedule" | "calendar" | "kalender" | "webuntis" | "timetable" | "stundenplan" | "agenda" | "school schedule" | "schedule and calendar"
        | "calendar and schedule" | "termine" | "weekly schedule" | "week" | "webuntis schedule" | "webuntis timetable" | "untis" | "untis schedule" | "school timetable"
        | "class schedule" | "webuntis stundenplan" | "schulplan"
            if opening =>
        {
            Some("schedule")
        }
        _ => None,
    }
}

static STOP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(stop|stopp|halt|enough|cancel|abort|be quiet|quiet|silence|shut up|stop talking|never mind|nevermind|forget it|sei still|sei ruhig|ruhe|still|hör auf|hoer auf|das reicht|abbrechen|schluss|genug|vergiss es|egal|schon gut)$")
});
/// Quit JAI himself. "Close JAI" arrives as "close" (the trailing name is stripped);
/// "shut down the computer" deliberately does not match.
static SHUTDOWN: LazyLock<Regex> = LazyLock::new(|| {
    let forms = [
        // "Turn down" stays a volume request.
        r"(?:(?:shut|power)(?:\s+(?:yourself|jai))?\s+(?:down|off)|(?:turn|switch)(?:\s+(?:yourself|jai))?\s+off)(?:\s+(?:yourself|jai|completely|for (?:today|now|the night)))?",
        r"shutdown(?:\s+(?:yourself|jai))?",
        r"(?:close|quit|exit|terminate|end|kill|stop)\s+(?:yourself|jai|your\s+(?:app|application|program|process|self)|the\s+(?:assistant|jai app))",
        r"close|quit|exit|terminate|log off|sign off|go offline|self[- ]destruct",
        r"(?:beende|beend)(?:\s+(?:dich(?:\s+selbst)?|jai|dein\s+programm|deine\s+app))?",
        // ("Kannst du dich selbst schließen?" arrives as "dich selbst schließen".)
        r"(?:dich|jai)?\s*(?:selbst\s+)?(?:beenden|ausschalten|abschalten|herunterfahren|runterfahren|schließen|schliessen)",
        r"(?:schalt|schalte|mach|mache)\s+(?:dich|jai)\s+(?:aus|ab)",
        r"(?:fahr|fahre)\s+(?:dich|jai)?\s*(?:herunter|runter)",
        r"(?:schließ|schließe|schliess|schliesse)(?:\s+(?:dich|jai))?",
    ];
    re(&format!("^(?:{})$", forms.join("|")))
});
/// "Close" on its own (JAI not named): it means the window, not quitting.
static BARE_QUIT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:close|schließ|schließe|schliess|schliesse|schließen|schliessen)$")
});

/// The raw sentence names JAI himself ("close JAI", "quit yourself", "beende dich").
fn names_jai(text: &str) -> bool {
    raw_words(text).split_whitespace().any(|w| matches!(w, "jai" | "jay" | "yourself" | "urself" | "dich"))
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
    re(r"^(system status|status report|systemstatus|system report|hardware status|run diagnostics|diagnostics|wie geht es dem system|wie geht's dem system|wie gehts dem system|systembericht|cpu usage|gpu temperature|gpu temp|wie warm ist die gpu|auslastung|(?:what s|whats|what is|give me|show me|tell me|check)\s+(?:my\s+|the\s+|a\s+)?(?:system|pc|computer|hardware)\s+(?:status|health|state|report|stats?)|(?:how is|hows|how s)\s+(?:my\s+|the\s+)?(?:system|pc|computer|machine|rig)(?:\s+(?:doing|holding up|running|looking))?|(?:pc|computer|system|hardware)\s+(?:status|health|check|report)|status\s+check|wie ist der systemstatus|wie läuft (?:der pc|das system|mein pc))$")
});
/// "Turn Firefox off", "switch Discord back on": only ever about a real program. The PC, the
/// lights and the sound are no programs; they have rules of their own or none.
static APP_OFF: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:turn|switch|shut)\s+off\s+(.+?)$|^(?:turn|switch|shut)\s+(.+?)\s+off$|^schalte?\s+(.+?)\s+(?:aus|ab)$"));
static APP_ON: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:turn|switch)\s+(?:back\s+)?on\s+(.+?)$|^(?:turn|switch)\s+(.+?)\s+(?:back\s+)?on$|^schalte?\s+(.+?)\s+(?:wieder\s+)?(?:ein|an)$"));

fn app_switch(s: &str) -> Option<Intent> {
    const NO_PROGRAMS: &[&str] = &[
        "pc", "computer", "laptop", "rechner", "monitor", "screen", "bildschirm", "light", "lights", "licht", "wifi", "wlan", "bluetooth", "sound", "audio", "ton", "music", "musik", "game", "spiel", "mic",
        "microphone", "mikrofon", "camera", "kamera",
    ];
    let program = |c: regex::Captures<'_>| c.iter().skip(1).flatten().next().map(|m| strip_article(m.as_str())).filter(|name| !NO_PROGRAMS.contains(&name.as_str()) && known_app(name));
    if let Some(app) = APP_OFF.captures(s).and_then(program) {
        return Some(Intent::Tool { name: "close_application", args: json!({ "app_name": app, "force": true }) });
    }
    let app = APP_ON.captures(s).and_then(program)?;
    Some(Intent::Tool { name: "open_application", args: json!({ "app_name": app }) })
}

/// A courtesy has one answer, and it takes no model: "thank you" once earned the last explanation
/// over again, a bare "JAI" a goodnight. (Raw words: `normalize` strips both as fillers.)
fn courtesy(text: &str) -> Option<&'static str> {
    static THANKS: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:ok|okay|great|perfect|nice|good|cool|awesome|brilliant|lovely|super|prima|klasse|toll|gut)\s+)?(?:thanks|thank\s+you|cheers|many\s+thanks|danke|danke\s+dir|danke\s+schön|danke\s+schoen|vielen\s+dank)(?:\s+(?:very\s+much|so\s+much|a\s+lot|a\s+million))?(?:\s+(?:jai|sir))?(?:\s+that\s+was\s+(?:perfect|great|good|helpful|nice))?$")
    });
    const REPLIES: [&str; 3] = ["You're welcome, Sir.", "My pleasure, Sir.", "Any time, Sir."];
    static SAID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if text.len() > 60 {
        return None;
    }
    let words = raw_words(text);
    if matches!(words.as_str(), "jai" | "hey jai" | "hi jai" | "ok jai" | "okay jai") {
        return Some("Yes, Sir?");
    }
    THANKS.is_match(&words).then(|| REPLIES[SAID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % REPLIES.len()])
}

/// "Is my mic muted?", "am I muted?", "are you listening?": the lock's state, as it stands.
fn microphone_asked(text: &str) -> Option<String> {
    static ASKED: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:jai\s+)?(?:is\s+(?:my|the|your)\s+(?:mic|microphone)\s+(?:muted|on|off|working)|am\s+i\s+muted|are\s+you\s+listening(?:\s+to\s+me)?(?:\s+right\s+now)?|can\s+you\s+hear\s+me|ist\s+(?:mein|das|dein)\s+(?:mikro|mikrofon)\s+(?:stumm|an|aus)|bin\s+ich\s+stumm|(?:hörst|hoerst)\s+du\s+(?:mir\s+)?zu|kannst\s+du\s+mich\s+(?:hören|hoeren))$")
    });
    // "Why did you stop listening?": his state, never his record of actions (2026-11-07 it was
    // "I have not changed anything in this session").
    static WHY_NOT: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:jai\s+)?(?:why\s+(?:did\s+you\s+stop|have\s+you\s+stopped|are\s+you\s+not|aren\s?t\s+you|did\s+you\s+quit)\s+listening(?:\s+to\s+me)?|warum\s+(?:hörst|hoerst)\s+du\s+(?:mir\s+)?(?:nicht\s+mehr|nicht)\s+zu|warum\s+hast\s+du\s+(?:aufgehört|aufgehoert)\s+zuzuhören)$")
    });
    let words = raw_words(text);
    if text.len() <= 60 && WHY_NOT.is_match(&words) {
        return Some(if crate::listening_lock::is_locked() {
            format!("Because you asked me to, Sir. {}", crate::listening_lock::spoken_state())
        } else {
            "I haven't stopped, Sir. I'm listening.".to_string()
        });
    }
    // "How long until you listen again?", "wann hörst du wieder zu?": what is left of a lock
    // with an end (typed while it holds; by voice the lock itself answers it).
    if text.len() <= 70 && crate::listening_lock::asks_how_long(text) {
        return Some(crate::listening_lock::how_long());
    }
    (text.len() <= 50 && ASKED.is_match(&words)).then(crate::listening_lock::spoken_state)
}

/// "Don't close it", "do not mute Discord", "schließ das nicht", "nicht stumm schalten": said of
/// an action, and the action is what must not happen. (A sentence that goes on to what he does
/// want is left to the rest: "don't close the mixer, close the board instead".)
fn held_back(text: &str) -> bool {
    static ENGLISH: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:no|nope|wait|please|jai)\s+)*(?:don\s?t|do\s+not|never)\s+(?:ever\s+|you\s+dare\s+)?(?:close|quit|exit|kill|end|open|start|launch|run|mute|unmute|stop|pause|turn|change|delete|remove|shut|restart|reboot|send|play|skip|switch|move|hide|lower|raise|set|put|make)\b")
    });
    static GERMAN: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:nein|bitte|jai)\s+)*nicht\s+(?:schließen|schliessen|zumachen|aufmachen|öffnen|oeffnen|ausmachen|ausschalten|einschalten|anmachen|stumm\s*schalten|löschen|loeschen|entfernen|stoppen|ändern|aendern|herunterfahren|neu\s+starten|leiser|lauter)(?:\s+machen)?$|^(?:(?:nein|bitte|jai)\s+)*(?:schließ|schließe|schliess|schliesse|mach|mache|öffne|oeffne|stell|stelle|dreh|drehe|lösch|lösche|loesch|loesche|entferne|entfern|schalte|schalt|beende|beend|stopp|stoppe)\s+(?:.{1,30}\s+)?nicht(?:\s+(?:zu|auf|aus|an|ab|weg|stumm|leiser|lauter|runter|hoch|leise|laut))?$")
    });
    let words = raw_words(text);
    let words = words.as_str();
    // ("Don't open it at startup anymore" is an order about the next start; "don't make me
    // repeat myself" is talk.)
    text.len() <= 80
        && (ENGLISH.is_match(words) || GERMAN.is_match(words))
        && !words.split_whitespace().any(|w| matches!(w, "but" | "instead" | "aber" | "sondern" | "stattdessen"))
        && !words.contains("make me")
        && !NEXT_START.is_match(words)
        // "Don't make the windows overlap" sets the packer.
        && !WINDOWS_APART.is_match(words)
        // "Don't start with Windows any more" is his own switch.
        && crate::hud_feel::start_order(&normalize(text)).is_none()
}

/// A plain question: it asks, and asks for nothing to be done. "Can you close Firefox?" is an
/// order in good manners; "should I close Firefox?" is not one, and neither is "why did you mute
/// Discord?". A sentence that goes on to an order ("is it open? close it") does not count.
pub fn only_asks(text: &str) -> bool {
    static ASKING: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:jai|hey|ok|okay|so|well|also|sir|uh|um|und|aber|sag\s+mal)\s+)*(?:is|isn\s?t|are|aren\s?t|am|was|were|did|didn\s?t|does|doesn\s?t|do\s+(?:i|you)|don\s?t\s+(?:i|you)|has|have\s+(?:i|you)|why|what|whats|which|who|whos|whose|when\s+(?:is|are|was|were|did|do|does|will|can|should)|where|wheres|how|hows|should\s+i|shall\s+i|must\s+i|ist|sind|bin|war|waren|hast|hat|hatte|hatten|habe\s+ich|warum|wieso|weshalb|welche|welcher|welches|welchen|wer|wann|wo|wie|läuft|laeuft|soll\s+ich|sollte\s+ich|muss\s+ich|darf\s+ich)\b")
    });
    static SUGGESTS: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:jai|hey|ok|okay|so|well|also|sir|uh|um|und|aber|sag\s+mal)\s+)*(?:how\s+about|what\s+about|why\s+don\s?t\s+you|why\s+not|do\s+you\s+mind|wie\s+wäre\s+es|wie\s+waere\s+es|was\s+ist\s+mit|warum\s+nicht)\b")
    });
    static GOES_ON: LazyLock<Regex> = LazyLock::new(|| {
        re(r"\b(?:then|dann|if\s+so|if\s+it\s+is|if\s+yes|falls\s+ja|wenn\s+ja|and\s+(?:if|close|open|mute|unmute|turn|play|stop|start|pause|put|set|show|make)|und\s+(?:wenn|falls|schließ|schliess|öffne|oeffne|mach|starte|stopp|zeig|stell))\b")
    });
    // "Why is there no sound?" is a complaint: he wants it back, as with "I can't hear anything".
    static COMPLAINS: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:jai|hey|ok|okay|so|well|sir|uh|um)\s+)*why\s+(?:is\s+there\s+no\s+(?:sound|audio)|(?:can\s?t|don\s?t)\s+i\s+hear|is\s+(?:the\s+|my\s+)?(?:sound|audio)\s+(?:gone|broken|not\s+working))\b|^(?:(?:jai|und|aber)\s+)*warum\s+(?:(?:höre|hoere|hör|hoer)\s+ich\s+(?:nichts|keinen|kein)|ist\s+(?:der\s+)?(?:ton|sound)\s+(?:weg|kaputt)|(?:gibt\s+es\s+)?(?:keinen|kein)\s+(?:ton|sound))\b")
    });
    let t = text.trim();
    let words = raw_words(t);
    let one_sentence = ![". ", "! ", "? "].iter().any(|end| t.contains(end));
    one_sentence && ASKING.is_match(&words) && !SUGGESTS.is_match(&words) && !GOES_ON.is_match(&words) && !COMPLAINS.is_match(&words)
}

/// "How do I mute Discord?", "should I close Firefox?", "wie öffne ich den Mixer?": about an
/// order he knows. The answer is the order itself, for when Sir wants it; nothing is done.
fn how_or_whether(text: &str) -> Option<String> {
    static HOW: LazyLock<Regex> = LazyLock::new(|| {
        re(r"(?i)^(?:(?:hey\s+)?jai[,.]?\s+)?(?:so\s+|ok\s+|okay\s+)?how\s+(?:do|can|could|would|should)\s+(?:i|we)\s+(?:get\s+you\s+to\s+|tell\s+you\s+to\s+|ask\s+you\s+to\s+)?(.{3,60}?)[?.!]*$")
    });
    static WHETHER: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(?:(?:hey\s+)?jai[,.]?\s+)?(?:so\s+|ok\s+|okay\s+)?(?:should|shall)\s+i\s+(.{3,60}?)[?.!]*$"));
    static WIE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(?:jai[,.]?\s+)?wie\s+(\w+e)\s+ich\s+(.{2,60}?)[?.!]*$"));
    // "Soll ich Firefox schließen?": the infinitive at the end is how he takes the order too.
    static SOLL: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(?:jai[,.]?\s+)?(?:soll|sollte)\s+ich\s+(.{2,60}?)[?.!]*$"));
    static WHAT_IF: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?i)^(?:(?:hey\s+)?jai[,.]?\s+)?what\s+(?:happens|would\s+happen|will\s+happen)\s+if\s+(?:i|you|we)\s+(.{3,60}?)[?.!]*$"));
    enum Asked {
        How,
        Whether,
        WhatIf,
    }
    let said = text.trim();
    let (order, asked) = if let Some(c) = HOW.captures(said) {
        (c[1].to_string(), Asked::How)
    } else if let Some(c) = WHETHER.captures(said).or_else(|| SOLL.captures(said)) {
        (c[1].to_string(), Asked::Whether)
    } else if let Some(c) = WIE.captures(said) {
        // "Wie schließe ich Firefox?" → "Schließe Firefox" (the first person is the imperative).
        (format!("{} {}", &c[1], &c[2]), Asked::How)
    } else if let Some(c) = WHAT_IF.captures(said) {
        (c[1].to_string(), Asked::WhatIf)
    } else {
        return None;
    };
    // The PC's power is the brain's double-checked dialogue, never a tool call: the same answer,
    // with what follows the order (2026-11-07 "Should I restart my PC?" reached the model, which
    // asked when to schedule it).
    if let Some(action) = crate::system_power::power_request(&order) {
        let mut letters = order.trim().chars();
        let said: String = letters.next().map_or(String::new(), |first| first.to_uppercase().chain(letters).collect());
        let checked = action.needs_confirmation();
        let then = if checked { "I'll ask once more, then it's done" } else { "it's done" };
        return Some(match asked {
            Asked::How => format!("Just say \"{said}\", Sir, and {then}."),
            Asked::Whether => format!("That's yours to decide, Sir. Say \"{said}\" and {then}."),
            Asked::WhatIf => format!("{} Say \"{said}\" if you want that{}.", action.consequence(), if checked { "; I'll ask once more first" } else { "" }),
        });
    }
    // Only what he would carry out; "how do I cook rice?" is the model's.
    let ctx = crate::intent_engine::ReflexContext::live();
    let (tool, args) = crate::compound_planner::resolve_one(&order, &ctx)?;
    if !said_as_an_order(&order, &tool, &args) {
        return None;
    }
    let mut letters = order.trim().chars();
    let order: String = letters.next().map_or(String::new(), |first| first.to_uppercase().chain(letters).collect());
    Some(match asked {
        Asked::How => format!("Just say \"{order}\", Sir, and it's done."),
        Asked::Whether => format!("That's yours to decide, Sir. Say \"{order}\" and it's done."),
        // What closing or muting a program does; anything else is the model's to explain.
        Asked::WhatIf => {
            let app = args["app_name"].as_str().or(args["app"].as_str()).map(crate::apps::display_name)?;
            match (tool.as_str(), args["action"].as_str()) {
                ("close_application", _) => format!("{app} closes, Sir, and anything unsaved in it is lost. Say \"{order}\" if you want that."),
                ("control_audio_mixer", Some("mute_app")) => format!("{app} goes silent until you unmute it, Sir. Nothing else changes. Say \"{order}\" if you want that."),
                _ => return None,
            }
        }
    })
}

/// Whether "say X" honestly answers "how do I X?": the sound, one of his own controls, or a
/// program he knows by name. Never his own sleep ("how do I go to sleep?" is about Sir), and
/// never a window or a program guessed from a free word ("go to bed", "open a PDF").
fn said_as_an_order(order: &str, tool: &str, args: &Value) -> bool {
    if order.to_lowercase().split_whitespace().any(|w| matches!(w, "you" | "dich" | "dir")) {
        return false;
    }
    match tool {
        "control_audio_mixer" | "media_and_volume_control" | "control_music" | "configure_interface" | "control_orb_mode" | "set_listening_mode"
        | "open_jai_view" | "control_popups" | "control_jai_interface" | "hud_board" | "listening_lock" | "undo_last_action"
        | "close_all_secondary_windows" => true,
        "open_application" | "close_application" | "is_app_running" => args["app_name"].as_str().is_some_and(known_app),
        "window_management" => args["action"] != "focus",
        _ => false,
    }
}

/// Whether this call is more than the plain question `text` warrants ("are you in orb mode?"
/// holds the words that switch it on).
pub fn beyond_the_question(text: &str, tool: &str, args: &Value) -> bool {
    only_asks(text) && !crate::tools::only_looks(tool, args)
}

/// What "did you mute Discord?" / "hast du Firefox geschlossen?" asks about, when it is a program
/// or one of his own views; anything vaguer ("did you do that?") gets his last few actions.
fn did_you_about(s: &str) -> Option<String> {
    static EN: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:why\s+)?(?:did|have)\s+you\s+(?:just\s+|really\s+|actually\s+)?[a-z]+\s+(?:the\s+|my\s+)?(.+?)(?:\s+(?:again|already|just\s+now|earlier|for\s+me|down|up|off|on))?$")
    });
    static DE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:warum\s+)?hast\s+du\s+(?:gerade\s+|eben\s+|wirklich\s+)?(?:den\s+|die\s+|das\s+|mein\s+|meine\s+|meinen\s+)?(.+?)\s+(?:stumm\s*)?\S+$")
    });
    const OWN: &[&str] = &["mixer", "board", "theme studio", "feature lab", "console", "text console", "core", "core view", "orb", "theme", "workbench"];
    const VAGUE: &[&str] = &["it", "that", "this", "anything", "something", "everything", "what", "es", "das", "etwas", "was", "alles"];
    let s = s.trim().trim_end_matches(['?', '.', '!']).trim_end();
    let object = EN.captures(s).or_else(|| DE.captures(s))?.get(1)?.as_str().trim().to_string();
    if VAGUE.contains(&object.as_str()) {
        return None;
    }
    (OWN.contains(&object.as_str()) || known_app(&object)).then_some(object)
}

/// "What is orb mode?", "what does the mixer do?", "was ist der Orbmodus?": his own modes and
/// views, told as they are (2026-11-07: the model made up "a translucent overlay").
fn explained(s: &str) -> Option<String> {
    static WHAT_IS: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:what\s+is|whats|what\s+does|what\s+do\s+you\s+mean\s+by|explain|tell\s+me\s+about|was\s+ist|was\s+macht|erklär\s+mir|erkläre\s+mir|erklaer\s+mir|erklaere\s+mir)\s+(?:the\s+|your\s+|a\s+|der\s+|die\s+|das\s+|dein\s+|deine\s+|den\s+)?(.+?)(?:\s+(?:do|for|good\s+for))?$")
    });
    let subject = WHAT_IS.captures(s)?.get(1)?.as_str().trim().to_string();
    let line = match subject.as_str() {
        "orb mode" | "pure orb mode" | "orb only mode" | "orbmodus" | "orb modus" => {
            "In orb mode only my orb floats on your screen, Sir: borderless, transparent and always on top. Drag it to move it, scroll to resize it, and double-click it for the full display."
        }
        "mixer" | "audio mixer" => "My mixer is a fader for every program that makes sound, Sir, and one for the master. Open it, or just tell me what to make louder.",
        "theme studio" => "The Theme Studio is where you pick my colours, name the theme and save it, Sir.",
        "feature lab" => "The Feature Lab lists everything I can do, Sir, each with a button to try it.",
        "core view" => "My core view shows every module I run on, wired as it runs, Sir. Drag to orbit it.",
        "span mode" | "spanmodus" => "Span mode stretches my display across all your monitors, Sir. \"Span mode on\" starts it and \"span mode off\" ends it.",
        _ => return None,
    };
    Some(line.to_string())
}

/// "Is Firefox open?", "läuft Discord?", "is the mixer open?", "is the music playing?", "how long
/// is left on the timer?": what only the machine knows, looked up and never guessed.
fn looked_up(s: &str) -> Option<Intent> {
    if let Some(line) = explained(s) {
        return Some(Intent::Answer(line));
    }
    // "Did you mute Discord?", "why did you close Firefox?": his own record says.
    static DID_YOU: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:why\s+)?(?:did|have)\s+you\s+(?:just\s+|really\s+|actually\s+)?(?:close|closed|mute|muted|unmute|unmuted|open|opened|turn|turned|stop|stopped|change|changed|delete|deleted|remove|removed|send|sent|play|played|skip|skipped|move|moved|lower|lowered|raise|raised|set|put|make|made|do|done)\b|^hast\s+du\s+.{0,40}(?:geschlossen|zugemacht|gemacht|geändert|geaendert|gelöscht|geloescht|geöffnet|geoeffnet|aufgemacht|stumm\s*geschaltet|gestoppt|verschoben|abgespielt)$|^warum\s+hast\s+du\b.{0,50}(?:geschlossen|zugemacht|gemacht|geändert|geaendert|gelöscht|geloescht|geöffnet|geoeffnet|aufgemacht|stumm\s*geschaltet|gestoppt|verschoben|abgespielt)$")
    });
    if DID_YOU.is_match(s) {
        let args = did_you_about(s).map_or_else(|| json!({}), |about| json!({ "about": about }));
        return Some(Intent::Tool { name: "recent_actions", args });
    }
    // "Are you in orb mode?", "is the orb hidden?": how it stands.
    static ORB_ASKED: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^is\s+(?:the\s+|your\s+)?orb(?:\s+mode)?\s+(?:hidden|visible|showing|on|off|active|enabled|up)$|^are\s+you\s+in\s+(?:pure\s+)?orb\s+mode$|^ist\s+(?:der\s+|dein\s+)?(?:orb|orbmodus|orb\s+modus)\s+(?:an|aus|sichtbar|versteckt|aktiv)$|^bist\s+du\s+im\s+(?:orbmodus|orb\s+modus)$")
    });
    if ORB_ASKED.is_match(s) {
        return Some(Intent::Tool { name: "control_orb_mode", args: json!({ "action": "status" }) });
    }
    static TIMER_LEFT: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^how\s+(?:long|much\s+time)\s+(?:is\s+|do\s+i\s+have\s+)?(?:still\s+)?(?:left|remaining)(?:\s+on\s+(?:the|my)\s+(?:timer|countdown))?$|^(?:what|whats|what\s+is)\s+(?:the|my)\s+(?:timer|countdown|stopwatch)\s+at$|^how\s+long\s+until\s+(?:the|my)\s+timer\s+(?:goes\s+off|is\s+done|ends|finishes)$|^is\s+(?:the|my)\s+(?:timer|countdown|stopwatch)\s+(?:done|finished|over|up|running|on)$|^ist\s+(?:der|mein)\s+timer\s+(?:fertig|abgelaufen|an)$|^wie\s+lange\s+(?:noch|läuft\s+(?:der|mein)\s+timer\s+noch)$|^wie\s+viel\s+zeit\s+(?:ist\s+|bleibt\s+|habe\s+ich\s+)?(?:noch\s+)?(?:übrig)?$|^wie\s+viel\s+zeit\s+hat\s+(?:der|mein)\s+timer\s+noch$|^was\s+zeigt\s+(?:der|mein|die)\s+(?:timer|stoppuhr)$")
    });
    if TIMER_LEFT.is_match(s) {
        return Some(Intent::Tool { name: "timer_left", args: json!({}) });
    }
    static IS_UP: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^is\s+(?:the\s+|my\s+)?([a-z][a-z0-9 ]{1,24}?)\s+(?:open|running|on|started|up|playing)$|^(?:läuft|laeuft)\s+(?:der\s+|die\s+|das\s+|mein\s+|meine\s+)?([a-z][a-z0-9äöüß ]{1,24})$|^ist\s+(?:der\s+|die\s+|das\s+|mein\s+|meine\s+)?([a-z][a-z0-9äöüß ]{1,24}?)\s+(?:offen|an|auf|gestartet)$")
    });
    let c = IS_UP.captures(s)?;
    let named = c.iter().skip(1).flatten().next()?.as_str().trim();
    match named {
        "music" | "musik" | "anything" | "something" | "irgendwas" | "etwas" => Some(Intent::Tool { name: "control_music", args: json!({ "action": "what_is_playing" }) }),
        "mixer" | "board" | "theme studio" | "feature lab" | "console" | "text console" | "core view" | "note" | "shopping list" => Some(Intent::Tool { name: "whats_open", args: json!({}) }),
        // (A game is whatever has a channel of its own on the mixer.)
        "game" | "a game" | "spiel" | "ein spiel" => Some(Intent::Tool { name: "control_audio_mixer", args: json!({ "action": "get_app", "app": "game" }) }),
        // (The PC, the lights and whatever else is no program have no answer here.)
        "pc" | "computer" | "laptop" | "rechner" | "monitor" | "screen" | "bildschirm" | "light" | "lights" | "licht" | "wifi" | "wlan" | "bluetooth" | "sound" | "audio" | "ton" | "mic" | "microphone" | "mikro" | "mikrofon" | "camera" | "kamera" | "it" | "that" | "this" | "es" | "das" => None,
        name if known_app(name) || matches!(name, "minecraft" | "sonora") => Some(Intent::Tool { name: "is_app_running", args: json!({ "app_name": name }) }),
        _ => None,
    }
}

/// "More", "weniger" with nothing else said: the word to ask back with.
pub fn bare_comparative(text: &str) -> Option<&'static str> {
    match raw_words(text).trim_start_matches("jai ").trim_end_matches(" please").trim_end_matches(" bitte") {
        "more" | "mehr" | "noch mehr" => Some("More"),
        "less" | "weniger" => Some("Less"),
        _ => None,
    }
}

/// Which reading of his machine a sentence asks for (`get_system_status`).
fn reading_asked(s: &str) -> &'static str {
    let names = |words: &[&str]| s.split(|c: char| !c.is_alphanumeric()).any(|w| words.contains(&w));
    if names(&["vram"]) {
        "vram"
    } else if names(&["ram", "memory", "arbeitsspeicher"]) {
        "memory"
    } else if names(&["gpu", "graphics", "grafikkarte"]) {
        "gpu"
    } else if names(&["cpu", "processor"]) {
        "cpu"
    } else {
        // ("How hot is my PC?": all of it.)
        "all"
    }
}

/// One reading of his machine: "show me my RAM usage", "how much RAM am I using", "what's my CPU
/// at", "how hot is my GPU", "wie heiß ist die Grafikkarte". ("What is the CPU?" is a question.)
static STATUS_READING: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:show|tell|give|check)(?: me)?\s+(?:my\s+|the\s+|our\s+)(?:current\s+)?(?:cpu|gpu|ram|memory|vram|processor)(?:\s+(?:usage|load|temperature|temp|status|utili[sz]ation))?|(?:whats|what is|how is|hows)\s+(?:my\s+(?:cpu|gpu|ram|memory|vram)(?:\s+(?:usage|load|temperature|temp|at|doing))?|(?:the\s+)?(?:cpu|gpu|ram|memory|vram)\s+(?:usage|load|temperature|temp|at|doing))|how (?:much|many)\s+(?:ram|memory|vram)\s+(?:am i using|is (?:in use|used|free|left|being used)|do i have(?:\s+(?:left|free))?|have i got(?:\s+left)?)|how (?:hot|warm)\s+is\s+(?:my\s+|the\s+)?(?:gpu|cpu|graphics card|pc|computer)|(?:zeig|zeige)\s+(?:mir\s+)?(?:meine|die|meinen|den)\s+(?:cpu|gpu|ram)(?:[\s-]*(?:auslastung|last|temperatur|nutzung))?|wie (?:hoch|viel) ist (?:meine |die )?(?:cpu|gpu|ram)(?:[\s-]*(?:auslastung|last|temperatur))?|wie viel (?:ram|arbeitsspeicher) (?:nutze|brauche|verbrauche) ich|wie (?:heiß|heiss|warm) ist (?:meine |die |mein )?(?:gpu|cpu|grafikkarte|pc|rechner)|(?:whats|what s|what are|how are|hows|how s|show me|tell me|check)\s+(?:my\s+|the\s+)?(?:pc\s+|pcs\s+|computer\s+|system\s+)?(?:temperatures|temps|temperature)(?:\s+(?:now|right now|at))?|wie (?:sind|hoch sind) (?:meine |die )?temperaturen)$")
});
static VOLUME_SET: LazyLock<Regex> = LazyLock::new(|| {
    // ("My PC's volume" arrives here as "my pcs volume".)
    re(r"^(?:(?:set|turn|put|stell|stelle|setz|setze|mach|mache|make|change|bring|adjust|crank|pump)\s+)?(?:the\s+|die\s+|my\s+|meine\s+|mein\s+|den\s+)?(?:system\s*|systems\s+|master\s+|pc\s+|pcs\s+|computer\s+|computers\s+|laptops?\s+|overall\s+|gesamt\s*|windows\s+)?(?:volume|lautstärke|lautstaerke|sound|ton|audio|loudness)(?:\s+level)?\s+(?:to\s+|auf\s+|at\s+|on\s+)?(\d{1,3})\s*(?:%|percent|prozent)?$")
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
/// "Make Discord full screen", "put Firefox in fullscreen", "mach Discord auf Vollbild": an app
/// named (the bare "full screen" and "make it full screen" are the conversation focus's).
static NAMED_FULLSCREEN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make|put|set|turn|switch|bring)\s+(.+?)\s+(?:to\s+|in\s+|into\s+|on\s+)?full[ -]?screen(?:\s+mode)?$|^(?:mach|mache|stell|stelle|schalt|schalte)\s+(.+?)\s+(?:auf\s+|in\s+(?:den\s+)?|zum\s+)?vollbild(?:modus)?(?:\s+an)?$")
});
static SNAP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:snap|move|put|schieb|verschieb|verschiebe)\s+(.+?)\s+(?:to the\s+|nach\s+|to\s+|auf die\s+)?(left|right|links|rechts)(?:\s+side|\s+seite|\s+hälfte)?$")
});

// ---- The HUD restyling itself ("make the interface red", "hide the system panel") ----------
// Every rule below only fires when its captured part really names a colour, theme, layout,
// panel or window position; otherwise parsing continues with the other rules.

/// The HUD itself, as the object of a window action.
const HUD_SELF: &str = r"(?:yourself|your (?:window|interface|hud|gui)|the (?:hud|interface|gui)|dich|dein fenster)";

static HUD_SHOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:show yourself|show your (?:window|interface|hud|gui|face)|show (?:me )?(?:the|your) board|open (?:the|your) board|open (?:your (?:window|interface|hud|gui)|the (?:interface|hud|gui))|bring (?:yourself|your window|the hud|the interface) (?:up|back|forward|to the front)|come back|appear|zeig dich|zeige dich|komm zurück|komm her|öffne (?:dein|das) (?:fenster|interface|hud))$")
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
/// "Exit full screen": JAI himself if he is the one in full screen, otherwise the app in focus
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
    re(r"^(?:(?:switch|go|change)\s+(?:back\s+)?(?:to|into)\s+|use\s+)?(?:windowed|window)\s+mode$|^windowed$|^(?:exit|leave)\s+fullscreen\s+(?:yourself|jai|hud|board)$|^(?:wechsel|wechsle|geh|gehe)?\s*(?:in\s+den\s+)?fenstermodus$")
});
/// "Open the interface settings", "show me your settings", "Einstellungen öffnen".
static HUD_SETTINGS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:open|show|bring up|pull up|show me|display)\s+(?:me\s+)?(?:(?:the|your|my)\s+)?(?:interface\s+|hud\s+|gui\s+|design\s+|look\s+|appearance\s+)?(?:settings|options|preferences|settings window|settings menu)$|^(?:öffne|oeffne|zeig|zeige)\s+(?:mir\s+)?(?:die\s+|deine\s+)?(?:interface[- ]?|design[- ]?)?einstellungen$|^einstellungen\s+(?:öffnen|oeffnen|zeigen)$")
});
/// "Make it darker", "a bit lighter", "mach es dunkler": dimmer or brighter light.
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
    re(r"^(?:make\s+)?(?:(?:the|your)\s+)?(?:window|interface|hud|background|wallpaper|yourself|gui|ui|everything)\s+(?:a bit\s+|a little\s+|slightly\s+)?((?:more|less)\s+(?:transparent|see[- ]through|opaque|solid)|transparent|see[- ]through|translucent|opaque|solid|non[- ]transparent)(?:\s+again|\s+so\s+(?:that\s+)?i\s+can\s+see\b.*)?$|^(?:mach|mache)\s+(?:dich|das fenster|das interface|die oberfläche|(?:den|deinen) hintergrund|das hud|alles)\s+(?:wieder\s+)?(durchsichtig|durchsichtiger|transparent|transparenter|undurchsichtig|weniger transparent|weniger durchsichtig)$")
});
static HUD_OPACITY_SET: LazyLock<Regex> = LazyLock::new(|| {
    // ("Make the interfaces opacity 0 %": "interface's" without its apostrophe.)
    re(r"^(?:(?:set|make|change|put|turn|bring|adjust|stell|stelle|setz|setze|mach|mache)\s+)?(?:the\s+|your\s+|my\s+|die\s+|deine\s+)?(?:(?:window|windows|interface|interfaces|hud|huds|background|gui|guis|ui|board|boards)\s+)?(opacity|transparency|deckkraft|transparenz|durchsichtigkeit)\s+(?:to\s+|at\s+|auf\s+)?(\d{1,3})\s*(?:%|percent|prozent)?$")
});
static HUD_ONLY_ORB: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:just|only)\s+(?:show\s+)?(?:the\s+)?(?:orb|reactor)$|^nur\s+(?:noch\s+)?(?:den\s+)?(?:reaktor|orb)$"));
static HUD_BACKGROUND: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make|turn|change|set|paint|switch)\s+(?:(?:the|your)\s+)?(?:background|backdrop)(?:\s+colou?r)?\s+(?:(?:to|into)\s+)?(.+?)$|^(?:(?:use|give me|set|switch to)\s+)?(?:(?:a|an|the)\s+)?(.+?)\s+background(?:\s+colou?r)?$|^(?:mach|mache|färb|färbe|faerbe|stell|stelle|setz|setze|ändere|änder|aendere)\s+(?:den\s+)?hintergrund\s+(?:(?:auf|zu|in)\s+)?(.+?)$|^hintergrund(?:farbe)?\s+(?:(?:auf|zu|in)\s+)?(.+?)$|^(.+?)\s+hintergrund$")
});
/// "Red theme", "switch to the Mark 42 theme", "dark mode", "Kompaktmodus".
static HUD_THEME_MODE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:switch|change|go|turn)\s+(?:to|into)\s+|(?:use|activate|enable|apply|load|start)\s+|(?:wechsel|wechsle|geh|gehe)\s+(?:zu|zum|zur|in den|in die|auf)\s+|(?:aktiviere|aktivier|nimm|nutze)\s+)?(?:(?:the|a|den|das|die|dem)\s+)?(.+?)\s*(theme|mode|modus|design|look|skin|style|stil|ansicht|view|layout|colou?rs|farben|thema)$")
});
static HUD_COLOUR: LazyLock<Regex> = LazyLock::new(|| {
    // ("Make the entire face a rose": Whisper's "the entire interface rose".)
    re(r"^(?:make|turn|change|set|switch|paint|colou?r)\s+(?:(?:the|your|my)\s+)?(?:(?:entire|whole)\s+)?(?:(?:interface|interfaces|gui|ui|hud|display|screen|theme|seam|steam|team|scheme|design|look|accents?|accent colou?rs?|colou?r scheme|colou?rs?|everything|yourself|window|reactor|orb|highlights?|face|thing)\s+)?(?:colou?rs?\s+)?(?:(?:to|into|in)\s+)?(?:the\s+|a\s+)?(.+?)$")
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
    re(r"^(?:(?:overlap|stack|cascade)\s+(?:the\s+|my\s+|all\s+|your\s+)?(?:windows|blocks|panels|them)|let\s+(?:the\s+)?windows\s+overlap|(?:überlappe?|staple|stapel)\s+(?:die\s+|meine\s+|alle\s+)?(?:fenster|blöcke)|(?:lass\s+)?(?:die\s+)?fenster\s+überlappen)$")
});
static WINDOWS_APART: LazyLock<Regex> = LazyLock::new(|| {
    // ("Don't make the windows overlap", "the windows shouldn't overlap": a setting, not a "don't".)
    re(r"^(?:keep\s+(?:the\s+)?windows\s+apart|(?:don'?t|don\s+t|do not|stop)\s+overlap(?:ping)?(?:\s+the\s+windows)?|unstack\s+(?:the\s+)?windows|(?:don'?t|don\s+t|do not|never)\s+(?:make|let)\s+(?:the\s+|my\s+|your\s+)?windows\s+overlap(?:\s+(?:anymore|again|each other))?|(?:the\s+|my\s+|your\s+)?windows\s+(?:shouldn'?t|shouldn\s+t|should not|must not|mustn'?t|mustn\s+t|may not|can'?t|can\s+t|cannot|won'?t|won\s+t|don'?t|don\s+t|do not)\s+overlap(?:\s+(?:anymore|again|each other))?|no\s+(?:more\s+)?overlapping(?:\s+windows)?|(?:stop|prevent)\s+(?:the\s+)?windows\s+(?:from\s+)?overlapping|(?:die\s+)?fenster\s+(?:sollen|dürfen|duerfen)\s+(?:sich\s+)?nicht\s+(?:mehr\s+)?(?:überlappen|ueberlappen))$")
});
/// "Use all monitors", "span over all three monitors", "stretch yourself across both screens",
/// "span mode on", "nutze alle drei Monitore", "erstreck dich über alle Bildschirme".
static SPAN_ALL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:use|span|stretch|spread|extend|expand|go|run)(?:\s+(?:yourself|out|(?:the|your)\s+(?:hud|board|interface)))?(?:\s+(?:over|across|on|onto|to))?\s+(?:all|every|both|each)\s+(?:of\s+)?(?:my\s+|the\s+)?(?:(?:two|three|four|2|3|4)\s+)?(?:monitors?|screens?|displays?)|(?:(?:turn|switch)\s+on|enable|activate|start)\s+(?:the\s+)?span(?:ning)?(?:\s+mode)?|(?:turn|switch)\s+(?:the\s+)?span(?:ning)?(?:\s+mode)?\s+on|span(?:\s+mode)?\s+on|span\s+all|(?:nutze|nutz|benutze|benutz|verwende|nimm)\s+(?:alle|beide)\s+(?:(?:zwei|drei|vier|2|3|4)\s+)?(?:meine\s+)?(?:monitore|bildschirme|displays)|(?:erstreck|erstrecke|verteil|verteile|spann|spanne|streck|strecke|zieh|ziehe)\s+dich\s+(?:über|ueber|auf)\s+alle\s+(?:(?:zwei|drei|vier|2|3|4)\s+)?(?:meine\s+)?(?:monitore|bildschirme))$")
});
/// "Use one monitor", "back to the main screen", "stop spanning", "span mode off", "don't use
/// all monitors anymore", "nutze nur einen Monitor".
static SPAN_ONE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:use|go back to|back to|switch back to|switch to|return to|go to|only use|just use)\s+(?:only\s+|just\s+)?(?:one|a single|1|my main|the main|my primary|the primary|my first|the first)\s+(?:monitor|screen|display)(?:\s+(?:only|again))?|(?:stop|quit|end|leave|exit|disable|deactivate|cancel)\s+(?:the\s+)?(?:spanning|span(?:\s+mode)?)(?:\s+(?:over\s+|across\s+)?(?:all\s+)?(?:(?:the|my)\s+)?(?:(?:two|three|four|2|3|4)\s+)?(?:monitors|screens|displays))?|(?:turn|switch)\s+off\s+(?:the\s+)?span(?:ning)?(?:\s+mode)?|(?:turn|switch)\s+(?:the\s+)?span(?:ning)?(?:\s+mode)?\s+off|span(?:\s+mode)?\s+off|(?:don'?t|do not|stop|no longer)\s+(?:span|spanning|use|using|stretch|stretching)(?:\s+(?:over|across))?\s+(?:all|every|both)\s+(?:of\s+)?(?:(?:my|the)\s+)?(?:(?:two|three|four|2|3|4)\s+)?(?:monitors|screens|displays)(?:\s+anymore)?|(?:nutze|nutz|benutze|benutz|verwende|nimm)\s+nur\s+(?:noch\s+)?(?:einen|den\s+ersten|den\s+haupt)\s*(?:monitor|bildschirm)|(?:zurück|zurueck)\s+auf\s+einen\s+(?:monitor|bildschirm)|(?:nur|wieder)\s+(?:ein|einen)\s+(?:monitor|bildschirm)|(?:hör|höre|hoer|hoere)\s+auf\s+(?:zu\s+|mit\s+dem\s+)?(?:spannen|strecken|überspannen)|nicht\s+mehr\s+(?:spannen|strecken))$")
});
/// "Toggle span mode", "span mode": the other of the two.
static SPAN_TOGGLE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:toggle\s+(?:the\s+)?(?:span(?:ning)?(?:\s+mode)?|span\s+all)|span\s+mode|spanmodus(?:\s+umschalten)?)$"));
/// "Move out of the way", "step aside", "you're in the way", "geh aus dem Weg", "mach Platz":
/// the emblem waits in its corner until Sir calls it back.
static ORB_ASIDE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:move|get|go|step|scoot|shift)\s+(?:yourself\s+|(?:the\s+|your\s+)?(?:orb|emblem|icon|logo|core|sphere)\s+)?(?:out\s+of\s+(?:the|my)\s+way|aside|to\s+the\s+side|away)|you(?:'?re|\s+are|\s+re|r)\s+in\s+(?:the|my)\s+way|make\s+(?:some\s+)?room|(?:geh|gehe|beweg\s+dich|bewege\s+dich)\s+(?:mal\s+)?(?:aus\s+dem\s+weg|zur\s+seite|beiseite)|du\s+bist\s+im\s+weg|mach\s+(?:mal\s+)?platz)$")
});
/// "Come back to the middle", "back to the centre" — and, while the emblem is out of the way,
/// plain "come back" / "you can come back" / "komm zurück".
static ORB_BACK: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:(?:you\s+can\s+|you\s+may\s+)?(?:come|go|move)\s+back\s+(?:to|in|into)\s+the\s+(?:middle|centre|center)(?:\s+now)?|(?:back|return)\s+to\s+the\s+(?:middle|centre|center)|(?:wieder|zurück|zurueck)\s+in\s+die\s+mitte)$"));
static ORB_BACK_SHORT: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:(?:you\s+can\s+|you\s+may\s+)?come\s+back(?:\s+now)?|(?:komm|komme)\s+(?:wieder\s+)?(?:zurück|zurueck))$"));
/// "Turn off the sound cues", "mute the interface sounds", "sound cues on", "Soundeffekte aus".
static SOUND_CUES: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:turn|switch)\s+)?(on|off|mute|silence|disable|stop|enable)?\s*(?:the\s+|your\s+)?(?:sound\s+cues|sound\s+effects|interface\s+sounds|board\s+sounds|hud\s+sounds|ui\s+sounds|soundeffekte)(?:\s+(on|off|an|aus))?$")
});
/// "Move your orb back to the middle", "put the emblem on the right side", "move the orb slightly
/// left", "bewege deinen Orb in die Mitte".
static ORB_TO: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:move|put|place|drag|bring|send|shift)\s+(?:the\s+|your\s+|my\s+)?(?:orb|emblem|icon|logo|core|sphere)\s+(?:back\s+)?(?:(?:over\s+)?(?:to|in|into|on|at)\s+)?(?:the\s+)?(.+)|(?:beweg|bewege|schieb|schiebe|setz|setze|pack|packe)\s+(?:den\s+|deinen\s+|die\s+|deine\s+)?(?:orb|kugel|emblem)\s+(?:wieder\s+)?(?:(?:in die|in den|auf die|zur|zum|nach)\s+)?(.+))$")
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
    if let Some(c) = SOUND_CUES.captures(s) {
        let off = c.iter().flatten().any(|m| matches!(m.as_str(), "off" | "aus" | "mute" | "silence" | "disable" | "stop"));
        return Some(json!({ "sound_cues": !off }));
    }
    if SPAN_TOGGLE.is_match(s) {
        return Some(json!({ "span_all_monitors": "toggle" }));
    }
    if SPAN_ALL.is_match(s) || SPAN_ONE.is_match(s) {
        return Some(json!({ "span_all_monitors": SPAN_ALL.is_match(s) }));
    }
    if ORB_ASIDE.is_match(s) {
        return Some(json!({ "orb_board_position": "aside" }));
    }
    if ORB_BACK.is_match(s) || (ORB_BACK_SHORT.is_match(s) && crate::hud_dock::orb_aside()) {
        return Some(json!({ "orb_board_position": "center" }));
    }
    let c = ORB_TO.captures(s)?;
    let spot = c.get(1).or(c.get(2))?.as_str().trim();
    interface::board_spot(spot).or_else(|| interface::nudge(spot, None)).map(|_| json!({ "orb_board_position": spot }))
}

static HUD_COLOUR_DE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:mach|mache|färbe|färb|faerbe|stell|stelle|setz|setze|ändere|änder|aendere|wechsel|wechsle)\s+(?:(?:das|die|den|dein|deine|deinen|mein|meine)\s+)?(?:(?:interface|gui|ui|hud|oberfläche|oberflaeche|design|farbe|farben|akzentfarbe|anzeige|fenster|theme|thema|bildschirm|dich|alles|aussehen|reaktor)\s+)?(?:(?:auf|zu|in|nach)\s+)?(.+?)$")
});

/// "Close the chat", "show me the conversation again", "blende den Chat aus": the block that
/// holds what was said. (Group 1 or 2: it goes away. The words were a panel's once, and must
/// never reach the chat *application*: "chat" is Discord to the app closer.)
static HUD_CHAT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(hide|close|remove|turn off|switch off|disable|get rid of)|show|open|display|bring back|restore|turn on|switch on|enable)\s+(?:me\s+)?(?:(?:the|your|my)\s+)?(?:chat|conversation|comms)(?:\s+(?:block|panel|window))?(?:\s+again)?$|^(?:blende\s+|blend\s+)?(?:(?:den|das|die|dein|deinen)\s+)?(?:chat|gespräch|unterhaltung)\s+(?:(aus|ausblenden|verstecken|schließen|schliessen)|ein|einblenden|anzeigen|zeigen|öffnen)$")
});

/// First non-empty capture group.
fn capture<'t>(c: &regex::Captures<'t>) -> Option<&'t str> {
    c.iter().skip(1).flatten().map(|m| m.as_str().trim()).find(|s| !s.is_empty())
}

/// A theme, layout or colour named in a request → `configure_interface` arguments.
fn look_args(name: &str, prefer_layout: bool) -> Option<Value> {
    let layout = || HudLayout::parse(name).map(|l| json!({ "layout": l.id() }));
    let colour = || interface::parse_color(name).map(|_| json!({ "accent_color": name }));
    // A theme by a word that is no colour: "rose", "iron man", "dark", "arc reactor".
    let theme = || crate::holo_theme::preset_for_word(name).map(|preset| json!({ "hud_palette": preset }));
    if prefer_layout {
        layout().or_else(colour).or_else(theme)
    } else {
        colour().or_else(theme).or_else(layout)
    }
}

/// Requests to restyle or move the HUD → `configure_interface` arguments.
fn interface_command(s: &str) -> Option<Value> {
    // The look-and-feel features by their names ("night mode", "subtitles on", "show the
    // lyrics", "what did I miss", "learn my voice"), before a "... mode" is taken for a theme.
    if let Some(args) = crate::hud_feel::command(s) {
        return Some(args);
    }
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
    // "Make it darker": the canvas is black as it is, so the light steps back (the dim grey
    // theme); "lighter" is white light. Never a light canvas.
    if let Some(c) = HUD_SHADE.captures(s) {
        let darker = matches!(capture(&c)?, "darker" | "deeper" | "blacker" | "dunkler" | "dark" | "more dark" | "noch dunkler");
        return Some(json!({ "theme": if darker { "dark" } else { "light" } }));
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
    // "Stop making the wallpaper transparent": solid again.
    static NO_MORE_SEE_THROUGH: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:stop|quit)\s+making\s+(?:the|your|my)\s+(?:window|interface|hud|background|wallpaper|desktop|gui|ui|everything)\s+(?:transparent|see[- ]through|translucent)$|^(?:don'?t|do not)\s+make\s+(?:the|your|my)\s+(?:window|interface|hud|background|wallpaper|desktop|gui|ui|everything)\s+(?:transparent|see[- ]through|translucent)(?:\s+anymore|\s+any more)?$")
    });
    if NO_MORE_SEE_THROUGH.is_match(s) {
        return Some(json!({ "opacity": 100 }));
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
    // "A red background": the canvas stays black, the light takes the colour.
    if let Some(name) = HUD_BACKGROUND.captures(s).as_ref().and_then(capture) {
        if interface::parse_color(name).is_some() {
            return Some(json!({ "accent_color": name }));
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
    if let Some(c) = HUD_CHAT.captures(s) {
        return Some(json!({ "comms_block": c.get(1).or(c.get(2)).is_none() }));
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
// tools through `focus`), never on JAI; he is only the target when Sir says so.

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
static FOCUS_REMOVE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:remove|dismiss|hide|get rid of|take away)\s+(?:it|that|this|that one|this one)$|^take\s+(?:it|that|this)\s+away$|^(?:entferne|entfern|nimm)\s+(?:es|das)(?:\s+weg)?$|^(?:mach|mache)\s+(?:es|das)\s+weg$|^weg\s+damit$")
});
static FOCUS_CLOSE_TAB: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:close|shut)\s+(?:the|that|this)\s+tab$|^(?:schließ|schließe|schliess|schliesse)\s+(?:den|diesen)\s+tab$|^tab\s+(?:schließen|schliessen|zu)$"));

/// Sir explicitly means JAI's own window ("maximize JAI", "fullscreen yourself", "make the
/// HUD full screen"). Checked on the raw sentence: normalisation drops a trailing "JAI".
static SELF_WINDOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:go\s+)?(full ?screen|maximi[sz]e|minimi[sz]e)\s+(?:yourself|jai|your\s+(?:own\s+)?(?:window|hud|interface|gui|self)|the\s+(?:hud|interface|gui))\b|\b(?:make|put|set|switch|turn)\s+(?:yourself|jai|your\s+(?:window|hud|interface|gui)|the\s+(?:hud|interface|gui))\s+(?:to\s+|in\s+|into\s+)?(full ?screen|maximi[sz]ed|minimi[sz]ed)\b|\bgo\s+(full ?screen)\s+(?:yourself|jai)\b|\b(?:mach|mache|schalte|schalt)\s+dich\s+(?:auf\s+|in\s+den\s+)?(vollbild)|\b(maximiere|minimiere)\s+dich\b")
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

/// "Maximize JAI", "fullscreen yourself" → `configure_interface` arguments for the HUD.
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
    // "Get rid of it": about something of his or about nothing (a program is closed, not removed).
    if FOCUS_REMOVE.is_match(s) {
        return tool("close_application", json!({ "app_name": "it", "force": true, "own_only": true }));
    }
    None
}

// ---- "Undo that": the last change of his own, taken back (`undo`) -----------------------------

static UNDO_LAST: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:never\s*mind\s+|forget\s+it\s+|scratch\s+that\s+)?(?:undo|revert|reverse)(?:\s+(?:that|this|it|(?:that|this)\s+change|(?:the|your|my)\s+last\s+(?:thing|one|action|change)))?$|^(?:put|set)\s+(?:it|that|this|everything)\s+back(?:\s+(?:as|how|the\s+way)\s+it\s+was)?$|^take\s+(?:that|it|this)\s+back$|^(?:mach|mache)\s+(?:das|es|dies|die\s+letzte\s+aktion)?\s*(?:wieder\s+)?rückgängig$|^rückgängig$|^(?:nimm|nehm)\s+(?:das|es)\s+zurück$|^(?:stell|mach)\s+(?:es|das)\s+(?:wieder\s+)?zurück$|^(?:change|switch)\s+(?:it\s+|that\s+)?back$|^(?:wechsel|wechsle|änder|ändere)\s+(?:es\s+|das\s+)?zurück$")
});

/// "That's not what I said", "I didn't say that", "das habe ich nicht gesagt": what he just did on
/// a misheard request is taken back — only a change of the last two minutes (2026-11-08: "Call me
/// Earth." became Sir's title). Not "that's wrong", which corrects an answer, not an action.
static NOT_WHAT_I_SAID: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:thats|that\s+is|this\s+is|that\s+was|this\s+was|thats\s+just)\s+not\s+what\s+i\s+(?:said|meant|asked\s+for|asked|wanted|told\s+you)$|^i\s+(?:didnt|did\s+not|never)\s+(?:say|ask\s+for|mean)\s+(?:that|this)$|^you\s+misheard(?:\s+me)?$|^you\s+heard\s+(?:me\s+)?wrong$|^(?:das|dies)\s+(?:habe|hab)\s+ich\s+(?:nicht|nie)\s+gesagt$|^(?:das|dies)\s+(?:war|ist)\s+nicht\s+(?:was|das\s+was)\s+ich\s+(?:gesagt|gemeint|wollte)(?:\s+habe)?$|^(?:du\s+hast\s+mich\s+)?falsch\s+verstanden$")
});

/// How long after a change "that's not what I said" still takes it back.
pub const MISHEARD_UNDO_SECS: u64 = 120;

/// "That's not what I said", "you misheard me": Sir says his words were heard wrong.
pub fn says_misheard(text: &str) -> bool {
    NOT_WHAT_I_SAID.is_match(&normalize(text))
}

/// "Undo that", "change it back": the sentence the last change is taken back with.
#[cfg(test)]
pub fn is_undo(text: &str) -> bool {
    let s = normalize(text);
    UNDO_LAST.is_match(&s) || NOT_WHAT_I_SAID.is_match(&s)
}

// "What did you just do?": his own last actions, as they went (never a guess).
static RECENT_ACTIONS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^what\s+(?:did|have)\s+you\s+(?:just\s+|last\s+|recently\s+)?(?:do|done|change|changed)(?:\s+(?:just\s+now|last|lately|recently|there))?$|^what\s+was\s+the\s+last\s+thing\s+you\s+did$|^was\s+hast\s+du\s+(?:zuletzt\s+|soeben\s+)?(?:gemacht|getan|geändert)$")
});

// "What's new?": what this build does that the last one did not.
static WHATS_NEW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:whats|what\s+is)\s+new(?:\s+(?:with\s+you|in\s+this\s+(?:version|build|update)|since\s+the\s+last\s+(?:version|build|update)))?$|^(?:any|got\s+any|do\s+you\s+have\s+any)\s+new\s+(?:features|tricks)$|^what\s+are\s+your\s+new\s+features$|^was\s+ist\s+neu$|^was\s+gibt(?:\s+es|s)\s+neues$|^was\s+kannst\s+du\s+neues$")
});

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

// ---- JAI's extra windows ("make that window transparent", "pin that window") ------------

const JAI_WINDOW: &str =
    r"(?:that|this|the new|the second|the other|the extra|the last|your second|your other|your new|the jai|that jai)\s+window";

static WINDOW_STYLE: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^(?:make|set|turn|switch)\s+{JAI_WINDOW}\s+(?:to\s+|into\s+)?(.+)$")));
static WINDOW_PIN: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(pin|unpin)\s+{JAI_WINDOW}$|^keep\s+{JAI_WINDOW}\s+(on top)$|^(?:dont|do not|stop)\s+keep(?:ing)?\s+{JAI_WINDOW}\s+(on top)$"))
});

/// "make that window transparent / borderless / red / emerald / always on top".
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
            if interface::parse_color(name).is_some() {
                style(json!({ "accent_color": name }))
            } else if crate::holo_theme::preset_for_word(name).is_some() {
                style(json!({ "theme": name }))
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
        for prefix in ["hey jai ", "jai ", "can you ", "could you ", "would you ", "kannst du ", "könntest du ", "ok ", "okay ", "now ", "and ", "und "] {
            if let Some(rest) = s.strip_prefix(prefix) {
                s = rest.to_string();
            }
        }
        for suffix in [" jai", " for me", " für mich", " now", " jetzt", " sir"] {
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
        "meeting", "file", "datei", "folder with", "new", "neue", "skill", "yourself", "your ", "dich", "jai",
        // ("Open three random windows": his views, not a program of that name; jai.log 2026-11-19.)
        "random", "zufällig",
    ];
    words <= 4 && !blacklist.iter().any(|b| name == b.trim() || name.contains(b))
}

// ---- Live windows, JAI's windows, monitors, music and Stark Protocols --------------------
// These are the requests the small local model tends to *answer* ("I'm creating a window with
// your PC stats, Sir") without calling a tool, so they never reach it: they run instantly.

/// A monitor named in a phrase: "monitor 2", "the second screen", "my left monitor".
static MONITOR_PHRASE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:the\s+|my\s+|a\s+|den\s+|meinen\s+|dem\s+|mein\s+|die\s+|meinem\s+|einen\s+)?(?:(first|second|third|fourth|1st|2nd|3rd|4th|other|another|different|next|left|right|primary|main|erste[nm]?|zweite[nm]?|dritte[nm]?|vierte[nm]?|andere[nm]?|linke[nm]?|rechte[nm]?|nächste[nm]?|haupt)\s*(?:monitor|screen|display|bildschirm)|(?:monitor|screen|display|bildschirm)\s*(\d|one|two|three|four|eins|zwei|drei|vier))$")
});
/// Anywhere in a sentence ("… on my second monitor").
static MONITOR_ANYWHERE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:on|to|onto|auf|zum|nach)\s+((?:the\s+|my\s+|a\s+|den\s+|meinen\s+|dem\s+|meinem\s+|einen\s+)?(?:(?:first|second|third|fourth|other|another|different|next|left|right|primary|main|erste[nm]?|zweite[nm]?|dritte[nm]?|andere[nm]?|linke[nm]?|rechte[nm]?|haupt)\s*(?:monitor|screen|display|bildschirm)|(?:monitor|screen|display|bildschirm)\s*(?:\d|one|two|three|four|eins|zwei|drei)))\b")
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
        "andere" | "anderen" | "anderem" | "another" | "different" => json!("other"),
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

/// A JAI window named in a phrase → the id the window tools resolve ("focus" = the one the
/// conversation is about).
fn jai_window_target(phrase: &str) -> Option<String> {
    let p = phrase.trim();
    if crate::focus::is_pronoun(p) || matches!(p, "that window" | "this window" | "the new window" | "the second window" | "the other window" | "the last window" | "your second window" | "your new window") {
        return Some("focus".into());
    }
    let name = p.trim_start_matches("the ").trim_start_matches("my ").trim_start_matches("your ");
    let name = name.strip_suffix(" window").or_else(|| name.strip_suffix(" fenster"))?.trim();
    // "The window" / "your window" mean the HUD.
    (!name.is_empty() && !matches!(name, "hud" | "jai" | "main")).then(|| name.to_string())
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
fn jai_window_edit(s: &str) -> Option<Intent> {
    let tool = |name: &'static str, args: Value| Some(Intent::Tool { name, args });
    if let Some(c) = WINDOW_RESIZE_TO.captures(s) {
        let id = jai_window_target(&c[1])?;
        return tool("update_native_window", json!({ "id": id, "width": c[2].parse::<u32>().ok()?, "height": c[3].parse::<u32>().ok()? }));
    }
    if let Some(c) = WINDOW_RESIZE_REL.captures(s) {
        let id = jai_window_target(&c[1])?;
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
            jai_window_target(&format!("{target} window")).unwrap_or_else(|| "focus".into())
        };
        return tool("update_native_window", json!({ "id": id, "add_content": [what] }));
    }
    None
}

static MOVE_TO: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:move|put|throw|send|shift|drag|place|bring|verschieb|verschiebe|schieb|schiebe|pack|beweg|bewege|tu|tue)\s+(.+?)\s+(?:over\s+)?(?:to|onto|on|auf|nach|zu|zum|in)\s+(.+)$")
});
static SELF_TO_MONITOR: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:move|go|switch|jump|head|geh|gehe|wechsel|wechsle|spring|springe|zieh|ziehe|komm|komme)\s+(?:over\s+|rüber\s+|mal\s+)?(?:to|onto|auf|zu|zum|nach)\s+(.+)$"));
static OPEN_ON: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:open|launch|start|starte|öffne|oeffne)\s+(.+?)\s+(?:on|auf)\s+(.+)$"));

/// "Move Sonora to monitor 2", "put Discord on my second screen", "move the PC stats window to
/// monitor 1", "move yourself to the left monitor", "open Spotify on my second monitor".
fn monitor_command(s: &str) -> Option<Intent> {
    if let Some(monitor) = SELF_TO_MONITOR.captures(s).and_then(|c| monitor_value(&c[1])) {
        return Some(Intent::Tool { name: "move_window_to_monitor", args: json!({ "app_or_window_name": "jai", "target_monitor": monitor }) });
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
    re(r"^(?:pause|stop|halt)\s+(?:the\s+|my\s+)?(?:music|song|track|playback|spotify|sonora|playlist)$|^pause$|^(?:musik|lied|song)\s+(?:pausieren|stoppen|anhalten|aus)$|^(?:pausier|pausiere|stopp|stoppe)\s+(?:die\s+)?musik$|^musik pause$|^pausieren$|^pausiere$|^(?:turn|switch)\s+off\s+(?:the\s+|my\s+)?music$|^(?:turn|switch)\s+(?:the\s+|my\s+)?music\s+off$|^music\s+off$|^(?:mach|mache|schalt|schalte|dreh|drehe)\s+(?:die\s+|meine\s+)?musik\s+aus$")
});
static RESUME_MUSIC: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:resume|continue|unpause)(?:\s+(?:the\s+)?(?:music|song|track|playback))?$|^play(?:\s+(?:some\s+|the\s+|my\s+)?music)?$|^play music$|^musik\s+(?:weiter|fortsetzen|an)$|^spiel weiter$|^fortsetzen$|^weiter$|^abspielen$|^play pause$|^(?:turn|switch)\s+on\s+(?:the\s+|my\s+)?music$|^(?:turn|switch)\s+(?:the\s+|my\s+)?music\s+(?:back\s+)?on$|^music\s+(?:back\s+)?on$|^(?:mach|mache|schalt|schalte)\s+(?:die\s+|meine\s+)?musik\s+(?:wieder\s+)?an$")
});
/// Verbs that may start music when a playlist / song is named explicitly. ("Shuffle X" is "play
/// X" with shuffle on, "queue up X" Sonora's queue: `music_command` reads them first.)
const PLAY_VERB: &str = r"(?:play|spiel|spiele|put on|start|starte|throw on|leg)";
/// Verbs that mean music on their own ("start Discord" opens an app, even if a song is called so).
const PLAY_ONLY: &str = r"(?:play|spiel|spiele|put on|throw on)";

// Sonora's queue and switches (`sonora.rs`, through `control_music`).
static QUEUE_SHOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:what(?:s| is) (?:in|on) (?:the|my) queue|(?:show|read|tell)(?: me)? (?:the |my |whats in the )?queue|what(?:s| is) (?:the )?next (?:song|track)|what (?:song|track) (?:is|comes) next|what(?:s| is) (?:coming )?(?:up )?next in the queue|welches lied kommt (?:als nächstes|danach)|was (?:ist|läuft|kommt)(?: gerade| als nächstes| noch)? in der warteschlange|zeig(?:e)?(?: mir)? (?:die )?warteschlange)$")
});
static QUEUE_ADD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:add|put)\s+(.+?)\s+(?:to|in|on|into)\s+(?:the\s+|my\s+)?queue$|^queue(?:\s+up)?\s+(.+)$|^(?:füg|füge)\s+(.+?)\s+(?:zur|in die)\s+warteschlange(?:\s+hinzu)?$|^(?:pack|packe|setz|setze)\s+(.+?)\s+in die warteschlange$|^(?:play|spiel|spiele)\s+(.+?)\s+(next|als nächstes|danach)$")
});
static CLEAR_QUEUE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:clear|empty|wipe)\s+(?:the|my)\s+queue$|^(?:lösch|lösche|leer|leere)\s+(?:die|meine)\s+warteschlange$"));
static SHUFFLE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:turn|switch|put)\s+(on|off)\s+(?:the\s+)?shuffle$|^(?:turn|switch|put)\s+(?:the\s+)?shuffle\s+(on|off)$|^shuffle\s+(on|off)$|^(enable|start|disable|stop)\s+shuffl(?:e|ing)$|^(?:zufallswiedergabe|shuffle)\s+(an|aus|ein)$|^(?:mach|schalt|schalte)\s+(?:die\s+)?(?:zufallswiedergabe|shuffle)\s+(an|aus|ein)$|^shuffle(?:\s+(?:the|my)\s+(?:songs|queue|music))?$|^(?:misch|mische)\s+(?:die|meine)\s+(?:songs|lieder|musik|warteschlange)$")
});
static REPEAT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:repeat|loop)\s+(?:this|the|that)\s+(song|track)$|^(?:put|set)\s+(?:this|the)\s+(song|track)\s+on\s+(?:repeat|loop)$|^(?:repeat|loop)\s+(?:the|my|this)\s+(?:playlist|queue|album)$|^(?:repeat|loop)\s+(on|off|all|one)$|^(?:turn|switch)\s+(on|off)\s+(?:repeat|looping)$|^(?:turn|switch)\s+(?:repeat|looping)\s+(on|off)$|^(stop) (?:repeating|looping)$|^(?:wiederhol|wiederhole)\s+(?:das|dieses)\s+(lied)$|^(?:wiederholung|dauerschleife)\s+(an|aus)$|^(?:wiederhol|wiederhole)\s+(?:die|meine)\s+(?:playlist|warteschlange)$")
});
static SEEK_TO: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:jump|go|skip|seek|spring|springe|spul|spule)\s+(?:to|zu|auf)\s+(\d{1,2})[ :.](\d{2})$"));
static RESTART_SONG: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:restart|replay)\s+(?:this|the)\s+(?:song|track)$|^(?:start|play)\s+(?:this|the)\s+(?:song|track)\s+(?:over|again|from the (?:start|beginning))$|^(?:spiel|starte)\s+(?:das|dieses)\s+lied\s+(?:nochmal\s+)?von vorne$")
});
static LIKE_SONG: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:like|save|heart|favou?rite)\s+(?:this|the|that)\s+(?:song|track)$|^(?:add|save)\s+(?:this|the|that)\s+(?:song|track)\s+to\s+(?:my\s+)?(?:liked songs|favou?rites|library)$|^(?:like|speicher|speichere)\s+(?:das|dieses)\s+lied$|^(?:dieses|das)\s+lied\s+gefällt mir$")
});
static UNLIKE_SONG: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:unlike|unsave|unheart)\s+(?:this|the|that)\s+(?:song|track)$|^remove\s+(?:this|the|that)\s+(?:song|track)\s+from\s+(?:my\s+)?(?:liked songs|favou?rites|library)$")
});
static EDIT_PLAYLIST: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:add|put|save)\s+(?:this|the|that)\s+(?:song|track)\s+(?:to|in|into)\s+(?:my\s+|the\s+)?(.+)$|^(?:create|make)\s+(?:a\s+)?(?:new\s+)?playlist(?:\s+(?:called|named)\s+.+)?$|^remove\s+(?:this|the|that)\s+(?:song|track)\s+from\s+(?:my\s+|the\s+)?(?:playlist\s+)?.+$|^(?:füg|füge)\s+(?:das|dieses)\s+lied\s+(?:zu|zur|in)\s+.+\s+hinzu$")
});

/// "Skip ahead 30 seconds", "go back 10 seconds", "spul 30 Sekunden vor": a step in the song.
fn seek_step(s: &str) -> Option<String> {
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() > 6 || !matches!(words.first().copied()?, "skip" | "jump" | "go" | "fast" | "forward" | "rewind" | "wind" | "spul" | "spule" | "spring" | "springe") {
        return None;
    }
    let (at, n) = words.iter().enumerate().find_map(|(i, w)| w.parse::<i64>().ok().map(|n| (i, n)))?;
    let unit = words.get(at + 1)?;
    let seconds = if unit.starts_with("sec") || unit.starts_with("sek") { n } else if unit.starts_with("min") { n * 60 } else { return None };
    let back = words.iter().any(|w| matches!(*w, "back" | "backward" | "backwards" | "zurück" | "rewind"));
    let ahead = words.iter().any(|w| matches!(*w, "ahead" | "forward" | "forwards" | "vor" | "vorwärts" | "fast"));
    (back != ahead).then(|| format!("{}{seconds}", if back { "-" } else { "+" }))
}

/// What Sir queues, as `control_music` takes it: "X by Y" a song and its artist, else a name
/// of his library by its kind (a song when it is none of his). None for "it", "something" …
fn queued_name(name: &str) -> Option<Value> {
    let name = name.trim().trim_end_matches(['.', ',', '!']).trim();
    if matches!(name, "" | "up" | "it" | "this" | "that" | "this song" | "that song" | "the song" | "something" | "something else" | "anything" | "music" | "some music" | "das" | "es" | "etwas" | "was") {
        return None;
    }
    if let Some((song, artist)) = name.split_once(" by ").or_else(|| name.split_once(" von ")) {
        return Some(json!({ "song_name": song.trim(), "artist_name": artist.trim() }));
    }
    let field = match crate::music::quick_lookup(name) {
        Some(crate::music::ItemKind::Playlist | crate::music::ItemKind::LocalPlaylist) => "playlist_name",
        Some(crate::music::ItemKind::Album) => "album_name",
        Some(crate::music::ItemKind::Artist) => "artist_name",
        _ => "song_name",
    };
    Some(json!({ field: name }))
}
static PLAY_ON_PLAYER: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^({PLAY_VERB}\s+.+?)\s+(?:on|in|auf|with|mit|über)\s+(spotify|sonora)$")));
static PLAY_PLAYLIST: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^{PLAY_VERB}\s+(?:me\s+|mir\s+)?(?:my\s+|the\s+|meine\s+|die\s+|meinen\s+)?(?:playlist|play list)\s+(?:called\s+|named\s+|namens\s+)?(.+)$|^{PLAY_VERB}\s+(?:me\s+|mir\s+)?(?:my\s+|the\s+|meine\s+|die\s+)?(.+?)\s*(?:playlist|play list)(?:\s+ab)?$"
    ))
});
static PLAY_SOMETHING: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^{PLAY_ONLY}\s+(?:the\s+song\s+|the\s+track\s+|das\s+lied\s+)?(.+)$")));

/// A playlist's name as Sir said it, without what came with it: "called jays rock" → "jays
/// rock", "jays rock playlist my jays rock" (said twice) → "jays rock". None when he named none
/// ("can you play a specific playlist?").
fn playlist_name(raw: &str) -> Option<String> {
    let mut name = raw.trim().trim_end_matches(['.', ',', '!', '?']).trim();
    // ("my playlist called jace rock": every lead-in, in whatever order.)
    while let Some(rest) = ["called ", "named ", "namens ", "titled ", "my ", "the ", "meine ", "die ", "playlist ", "play list "].iter().find_map(|p| name.strip_prefix(p)) {
        name = rest.trim();
    }
    // Said twice ("Jay's Rock playlist, my Jay's Rock playlist"): the first time.
    for word in [" playlist ", " play list "] {
        if let Some((first, _)) = name.split_once(word) {
            name = first.trim();
        }
    }
    let name = name.trim_end_matches(" playlist").trim_end_matches(" play list").trim_end_matches(['.', ',']).trim();
    let none = matches!(
        name,
        "" | "a" | "an" | "a specific" | "specific" | "some" | "any" | "a certain" | "certain" | "one" | "a new" | "another" | "a different" | "different" | "a random" | "random" | "eine" | "irgendeine" | "bestimmte" | "eine bestimmte"
    );
    (!none).then(|| name.to_string())
}

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

/// "Pick some good music", "play something", "surprise me": JAI chooses (executive initiative).
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
    // "List my playlists", "what playlists do I have", "welche Playlists habe ich".
    static LIST_PLAYLISTS: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:list|show|read|tell)(?: me)? (?:all )?(?:my |the )?playlists|(?:what|which) playlists (?:do i have|have i got|are there)|what are my playlists|(?:zeig|zeige)(?: mir)? (?:alle )?(?:meine |die )?playlists|welche playlists (?:habe ich|hab ich|gibt es))$")
    });
    if LIST_PLAYLISTS.is_match(s) {
        return music(json!({ "action": "list_playlists" }));
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
    // Sonora's queue and switches.
    if QUEUE_SHOW.is_match(s) {
        return music(json!({ "action": "queue_show" }));
    }
    if CLEAR_QUEUE.is_match(s) {
        return music(json!({ "action": "clear_queue" }));
    }
    if LIKE_SONG.is_match(s) {
        return music(json!({ "action": "like" }));
    }
    if UNLIKE_SONG.is_match(s) {
        return music(json!({ "action": "unlike" }));
    }
    if let Some(c) = QUEUE_ADD.captures(s) {
        if let Some(mut args) = (1..=5).find_map(|i| c.get(i)).and_then(|m| queued_name(m.as_str())) {
            args["action"] = json!(if c.get(6).is_some() { "play_next" } else { "queue_add" });
            return music(args);
        }
    }
    if let Some(c) = EDIT_PLAYLIST.captures(s) {
        // ("Add this song to the queue" is the queue's; "… to my liked songs" was liked above.)
        if !c.get(1).is_some_and(|m| m.as_str().contains("queue")) {
            return music(json!({ "action": "edit_playlist" }));
        }
    }
    if let Some(c) = SHUFFLE.captures(s) {
        let said = (1..=6).find_map(|i| c.get(i)).map_or("on", |m| m.as_str());
        let state = if matches!(said, "off" | "aus" | "disable" | "stop") { "off" } else { "on" };
        return music(json!({ "action": "set_shuffle", "state": state }));
    }
    if let Some(c) = REPEAT.captures(s) {
        let said = (1..=9).find_map(|i| c.get(i)).map_or("all", |m| m.as_str());
        let state = match said {
            "song" | "track" | "lied" | "one" => "one",
            "off" | "aus" | "stop" => "off",
            _ => "all",
        };
        return music(json!({ "action": "set_repeat", "state": state }));
    }
    if let Some(c) = SEEK_TO.captures(s) {
        return music(json!({ "action": "seek", "position": format!("{}:{}", &c[1], &c[2]) }));
    }
    if RESTART_SONG.is_match(s) {
        return music(json!({ "action": "seek", "position": "start" }));
    }
    if let Some(step) = seek_step(s) {
        return music(json!({ "action": "seek", "position": step }));
    }
    // "Shuffle my JaysRock playlist": played as "play …", with Sonora's shuffle on.
    if let Some(rest) = s.strip_prefix("shuffle ") {
        if let Some(Intent::Tool { name, mut args }) = music_command(&format!("play {rest}")) {
            if args["action"] == "play" {
                args["action"] = json!("shuffle");
            }
            return Some(Intent::Tool { name, args });
        }
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
                let song = song.trim().trim_end_matches(['.', ',']).trim();
                if let Some(list) = playlist_name(list).filter(|_| !song.is_empty()) {
                    return music(json!({ "action": "play", "song_name": song, "playlist_name": list }));
                }
            }
        }
        // ("Can you play a specific playlist?" names none: the model asks which.)
        return music(json!({ "action": "play", "playlist_name": playlist_name(&name)? }));
    }
    if let Some(c) = PLAY_BY.captures(s) {
        let (what, artist) = (c[1].trim().trim_end_matches(['.', ',']).trim(), c[2].trim());
        // "Play Thunderstruck from my playlist JaysRock" / "… from my JaysRock playlist": a song in a
        // playlist even before the library index knows the name, never an artist called "my …".
        let owned = ["my ", "meiner ", "meinem ", "the playlist ", "playlist "].iter().find_map(|p| artist.strip_prefix(p));
        if let Some(rest) = owned.filter(|_| !what.is_empty()) {
            if let Some(list) = playlist_name(rest).filter(|list| list != "playlist") {
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
    re(r"^(?:yes|yeah|yep|yup|yes sir|sure|sure thing|ok|okay|alright|do it|do that|do so|go ahead|go for it|please do|sounds good|make it so|proceed|absolutely|of course|affirmative|why not|lets do it|let s do it|that would be great|that would be nice|ja|jawohl|klar|gerne|gern|mach das|mach es|mach ruhig|los|auf jeden fall|genau|gerne doch|ja gerne|open it|open the window|show me|yes open it|do that for me)(?:\s+(?:sir|jai|then|now|bitte|please|thanks|thank you|danke))?$")
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
    let s = s.trim_start_matches("jai ").trim_end_matches(" jai").trim_end_matches(" sir");
    !s.is_empty() && NEGATE.is_match(s)
}

/// "Terminate it", "kill background tasks": the Guardian's second offer.
pub fn is_termination(text: &str) -> bool {
    TERMINATE.is_match(&normalize(text))
}

/// An action a reply of JAI's states or offers.
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
            if !matches!(name, "shutdown_jai" | "control_jai_lifecycle") {
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
    reply_actions(reply).into_iter().find(|a| a.offer).map(|a| (a.name, a.args))
}

/// Compiles every pattern up front (first use otherwise costs ~0.5 s on the first command).
pub fn warm_up() {
    crate::music::warm_up();
    // The ways of saying an order are several hundred patterns: building them takes half a
    // second, once, and it is meant to. They are built before the first sentence is read, and
    // nothing read while warming up counts as a slow sentence (every start logged the warning
    // "[WAYS] Slow: 'warm up the parser' took 513007 µs").
    crate::brain::warming_up(true);
    crate::ways::warm_up();
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
    crate::brain::warming_up(false);
}

/// "Switch to Ultron mode", "Ultron-Modus aktivieren", "are you in Ultron mode?": the mode is
/// retired, and saying so is all that happens (no model, nothing launched).
fn ultron_retired(s: &str) -> Option<Intent> {
    static ULTRON: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:(?:switch|change|go|turn|flip|wechsel|wechsle|schalte?|geh|gehe)\s+(?:back\s+|zurück\s+)?(?:to|into|zu|zum|in\s+den|auf\s+den|auf)\s+|(?:activate|enable|engage|start|initiate|turn\s+on|aktiviere?|starte?)\s+|(?:what\s+is|whats|was\s+ist|are\s+you\s+in|bist\s+du\s+im|is|ist)\s+)?(?:the\s+|den\s+|der\s+)?ultron(?:[\s-]*(?:mode|modus|protocol|protokoll))?(?:\s+(?:on|off|an|aus|ein|aktivieren|einschalten|starten|engage|activate|active|aktiv|enabled))?$")
    });
    ULTRON.is_match(s).then(|| Intent::Answer("Ultron mode has been retired, Sir.".to_string()))
}

/// "Dock all windows", "dock it back", "detach the radar", "pop out the plotter": every view
/// of his is a window of its own (2026-11-19), so there is nothing to dock or to detach, and
/// saying so is all that happens. (Left to the model, "dock all windows" went for Sir's own
/// programs' windows.)
fn docking_retired(s: &str) -> Option<Intent> {
    static DOCKING: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:re-?dock|dock|undock|detach|pop out)\s+(?:it|them|that|this|all|every|the|my|your)\b"));
    DOCKING.is_match(s).then(|| Intent::Answer("Every view of mine is a window of its own now, Sir. There is nothing to dock.".to_string()))
}

/// An order in Sir's words: by the fast path's own rules, and where those do not know it (or
/// know it only as the name of an app or a window: "start the dictation" was an app called
/// "dictation"), the same order said another way (`ways`).
pub fn parse(text: &str) -> Option<Intent> {
    // "The music app", "my chat app": the app of his that plays that part.
    let named = crate::apps::with_roles(text);
    let text: &str = &named;
    let direct = parse_as_said(text);
    if direct.as_ref().is_some_and(|found| !takes_any_name(found)) {
        return direct;
    }
    match crate::brain::another_way_knowing(text, Some(&direct)) {
        Some((_, crate::brain::Fast::Intent(intent))) => Some(intent),
        _ => direct,
    }
}

/// Whether an order comes from a rule that takes whatever words follow for the name of an app,
/// a window, a widget or a thing to look for: the weakest reading a sentence can have, and the
/// only one another way of saying an order may take the place of.
pub fn takes_any_name(intent: &Intent) -> bool {
    let (tool, args) = match intent {
        Intent::Tool { name, args } => (*name, args),
        Intent::Silent(action) => (action.tool, &action.args),
        _ => return false,
    };
    match tool {
        // ("Activate goon mode" names no app: never read another way, "game mode".)
        "open_application" => args["app_name"] != crate::apps::PRIVATE_WINDOW,
        "close_application" | "find_files" | "remove_widget" => true,
        "window_management" => args.get("target_window").is_some(),
        "create_or_update_widget" => args.get("remove_item").is_some() || args.get("add_item").is_some(),
        "hud_board" => args["action"] == "research",
        // ("Can you turn the music back on?" was taken for a question about what he can do.)
        "list_jai_features" => args["topic"] != "all",
        _ => false,
    }
}

/// The name such a rule took, with its tool: ("open_application", "spotify").
pub fn name_taken(intent: &Intent) -> Option<(&'static str, String)> {
    let Intent::Tool { name, args } = intent else { return None };
    let key = match *name {
        "open_application" | "close_application" => "app_name",
        "window_management" => "target_window",
        _ => return None,
    };
    Some((name, args[key].as_str()?.to_lowercase()))
}

/// An order in Sir's words as they stand, by the fast path's own rules.
pub fn parse_as_said(text: &str) -> Option<Intent> {
    // An order with a delay ("play music in one minute", "close the board at 18:30") is the
    // order and when: read as a whole before any rule below takes the time for part of a
    // name (`later`, which leaves alone whatever is understood as it stands).
    if let Some(args) = crate::later::order(text) {
        return Some(Intent::Tool { name: crate::later::TOOL, args });
    }
    if let Some(line) = courtesy(text) {
        return Some(Intent::Answer(line.to_string()));
    }
    // A picture, a video or a slideshow on the board's black (read as said: a path is no words).
    if let Some(args) = crate::bg_media::command(text) {
        return Some(Intent::Tool { name: "configure_interface", args });
    }
    if let Some(line) = microphone_asked(text) {
        return Some(Intent::Answer(line));
    }
    if held_back(text) {
        return Some(Intent::Answer("Very good, Sir. I'll leave it as it is.".to_string()));
    }
    if let Some(line) = how_or_whether(text) {
        return Some(Intent::Answer(line));
    }
    let s = normalize(text);
    if s.is_empty() {
        return None;
    }
    // "Go on", "continue": what he was cut off saying (talked over, stopped) two minutes ago or
    // less, from the start of that sentence. (With nothing cut off, "continue" and "weiter"
    // resume the music.)
    if GO_ON.is_match(&s) {
        if let Some(rest) = crate::tts::cut_off(RESUME_WITHIN) {
            return Some(Intent::Answer(rest));
        }
    }
    // "Soll ich Firefox schließen?": he asks himself, he tells nobody to.
    static ASKS_HIMSELF: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:soll|sollte|sollen|muss|müsste|darf)\s+ich\b|^(?:should|shall|must)\s+i\b"));
    if ASKS_HIMSELF.is_match(&s) {
        return None;
    }
    if let Some(intent) = ultron_retired(&s) {
        return Some(intent);
    }
    if let Some(intent) = looked_up(&s) {
        return Some(intent);
    }
    let tool = |name: &'static str, args: Value| Some(Intent::Tool { name, args });
    let media = |action: &str| json!({ "action": action });
    // The kit of 2026-11-13 (dictation, the notebook, rules, monitor brightness, the changes
    // list …): each asked for in its own words, before a general rule takes a word of theirs.
    if let Some((name, args)) = crate::kit::command(&s, text) {
        return tool(name, args);
    }

    if STOP.is_match(&s) {
        return Some(Intent::Stop);
    }
    // "Undo that": whatever he changed last, taken back (before any rule that reads "put it
    // back" or "revert" as something of its own).
    if UNDO_LAST.is_match(&s) {
        return tool("undo_last_action", json!({}));
    }
    if NOT_WHAT_I_SAID.is_match(&s) {
        return tool("undo_last_action", json!({ "within_s": MISHEARD_UNDO_SECS }));
    }
    if RECENT_ACTIONS.is_match(&s) {
        return tool("recent_actions", json!({}));
    }
    if WHATS_NEW.is_match(&s) {
        return tool("whats_new", json!({}));
    }
    // The HUD's own views ("close your core", "bring back the screen", "dock all windows").
    if let Some(intent) = hud_view_command(text) {
        return Some(intent);
    }
    if let Some(intent) = outing_command(text) {
        return Some(intent);
    }
    // His own views moved and sized ("move it to the middle", "full screen the Earth", "put the
    // earth on the left half", "move the radar to my second monitor", "put them side by side"),
    // before "show me the earth" opens the panel.
    if let Some(args) = crate::window_commands::parse(text) {
        return tool("arrange_jai_window", args);
    }
    // live layers (C1): satellites, passes, trains, departures, traffic, rain, past weather
    // ("show me Starlink", "when can I see the ISS?", "when's my next train?", "how's the A8?").
    if let Some((name, args)) = crate::live_layers::voice_command(text) {
        return tool(name, args);
    }
    // The network radar, the disk map and the Earth ("check the network", "what is taking up
    // space", "show me the earth", "switch to satellite view"), before the readings and the board.
    if let Some(args) = panel_request(text) {
        return tool("open_jai_view", args);
    }
    // The state of his machine is a reading, not a topic for the board to research ("give me a
    // system report", "show me my RAM usage", "how hot is my GPU").
    if STATUS.is_match(&s) {
        return tool("get_system_status", json!({}));
    }
    if STATUS_READING.is_match(&s) {
        return tool("get_system_status", json!({ "reading": reading_asked(&s) }));
    }
    // The HUD board first: "sign off" puts the screen down (it no longer quits JAI), and with
    // a board up "maximise that" means a block, not a window.
    if let Some(intent) = board_command(text, crate::hud_board::is_active(), crate::hud_board::spotlight_active()) {
        return Some(intent);
    }
    // Before the close/open rules, which would read "close ur self" or "kill process" as apps.
    if SHUTDOWN.is_match(&s) || is_self_shutdown(&s) {
        // A bare "close" / "quit" (JAI not named) only sends the HUD to the tray.
        if BARE_QUIT.is_match(&s) && !names_jai(text) {
            return tool("configure_interface", json!({ "window": "hide" }));
        }
        return tool("shutdown_jai", json!({}));
    }
    // "Clear the VRAM", "get out of my VRAM": the zero-VRAM standby at once.
    if VRAM_FLUSH.is_match(&s) {
        return tool("manage_vram", json!({ "action": "flush" }));
    }
    if GAME_MODE_ASKED.is_match(&s) {
        return tool("manage_vram", json!({ "action": "game_status" }));
    }
    if GOON_MODE.is_match(&s) {
        return tool("open_application", json!({ "app_name": crate::apps::PRIVATE_WINDOW }));
    }
    if GAME_MODE.is_match(&s) {
        let off =["off", "stop", "disable", "deactivate", "end ", "leave", "exit", "quit", " aus", "beende"].iter().any(|w| s.contains(w));
        return tool("manage_vram", json!({ "action": if off { "game_off" } else { "game_on" } }));
    }
    // "Open the integrations hub" (Whisper: "the integration sub").
    if INTEGRATIONS_HUB.is_match(&s) {
        return tool("configure_productivity_service", json!({ "service_name": "all", "action": "open" }));
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
        return tool("media_and_volume_control", media("jai_louder"));
    }
    if VOICE_QUIETER.is_match(&s) {
        return tool("media_and_volume_control", media("jai_quieter"));
    }
    if let Some(intent) = lifecycle_or_window_command(&s) {
        return Some(intent);
    }
    // "Create a new window that shows my PC stats / the space left on my drives."
    if let Some(args) = live_window_request(&s) {
        return tool("spawn_native_window", args);
    }
    // "Make the drive space window smaller", "add CPU temperature to that window".
    if let Some(intent) = jai_window_edit(&s) {
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
    // "Maximize JAI", "fullscreen yourself": JAI only when Sir names him.
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
        // ("Zeig mir das Sonnensystem" shows a thing: only a program of his is a window to bring up.)
        if looks_like_app_name(&target) && (!c[1].starts_with("zeig") || known_app(&target)) {
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
    if let Some(name) = NAMED_FULLSCREEN.captures(&s).as_ref().and_then(capture) {
        let target = strip_article(name);
        // (His own board and windows are the HUD's rules, before this one.)
        let his_own = matches!(target.as_str(), "board" | "hud" | "interface" | "gui" | "ui" | "orb" | "emblem" | "console" | "core" | "screen" | "window" | "windows" | "everything" | "all" | "it all" | "dich" | "alles");
        if looks_like_app_name(&target) && !his_own && !crate::focus::is_pronoun(&target) {
            return tool("window_management", json!({ "action": "fullscreen", "target_window": target }));
        }
    }
    if let Some(intent) = app_switch(&s) {
        return Some(intent);
    }
    // ("Shut down the PC" is the PC, and its double-check is the brain's: never a program.)
    const MACHINE: &[&str] = &["pc", "my pc", "computer", "my computer", "laptop", "rechner", "system", "monitor", "screen", "bildschirm"];
    for re in [&*CLOSE, &*CLOSE_DE, &*CLOSE_DE_SUFFIX] {
        if let Some(c) = re.captures(&s) {
            // "Close the Earth, you", "close Spotify for me now": what follows a name is no part
            // of it (2026-11-08: Windows was asked for programs called "Earth You", "Up", "Off").
            let named = strip_article(&c[1]);
            // ("Close yourself" / "quit you" name JAI himself: that stays as it was said.)
            let app = if crate::tools::names_jai(&named) { named } else { strip_article(&crate::intent_engine::strip_close_fillers(&named)) };
            if app.is_empty() {
                // "Close up", "closed, off": nothing named. Something of his that is up (the view
                // he just opened), never the program Sir is working in.
                return tool("close_application", json!({ "app_name": "it", "force": true, "own_only": true }));
            }
            if crate::focus::is_pronoun(&app) {
                // "Close it up, please": the "it" of "close it".
                return tool("close_application", json!({ "app_name": "it", "force": true }));
            }
            if crate::intent_engine::is_own_view(&app) {
                return Some(Intent::Silent(SilentAction { tool: "hud_board", args: json!({ "action": "close", "target": crate::intent_engine::own_view_name(&app) }), label: "CLOSED".to_string() }));
            }
            // One of his blocks that is up, by its title or by what Sir calls it, whatever the
            // words are ("the changes list" is no program's name, so no rule took it and the
            // language model tidied the interface instead: seen live 2026-11-13).
            if !MACHINE.contains(&app.as_str()) && !crate::hud_dock::blocks_named(&app).is_empty() {
                return Some(Intent::Silent(SilentAction { tool: "hud_board", args: json!({ "action": "close", "target": app }), label: "CLOSED".to_string() }));
            }
            if looks_like_app_name(&app) && !MACHINE.contains(&app.as_str()) {
                return tool("close_application", json!({ "app_name": app, "force": true }));
            }
        }
    }
    // JAI's own views are never programs ("open my system stats", "open message from stats").
    if let Some(args) = internal_view_command(text) {
        return tool("open_jai_view", args);
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
// "Hey JAI, are you there?", "can you do a state of VR report for me?", "yes please, present
// the report", "bring up that VR report you gave me earlier", "maximise the chart", "back to the
// board", "show me Tulleys Farm on the map", "thanks JAI, you can go to sleep again".

static BOARD_WAKE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:are you (?:there|awake|up|still there|with me)|you there|you awake|bist du (?:da|wach|noch da))(?:\s+jai)?$")
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
/// "Show me around (the HUD)", "give me a tour", "zeig mir alles", "führ mich herum". (Not
/// "zeig mir alles über …": that is a question.)
static HUD_TOUR: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:can|could|would|will)\s+you\s+)?(?:show\s+me\s+around|give\s+me\s+a\s+(?:quick\s+)?tour|take\s+me\s+on\s+a\s+tour)(?:\s+(?:of\s+)?(?:the\s+|your\s+)?(?:hud|interface|screen|here))?$|^zeig\s+mir\s+alles$|^(?:führ|fuehr)\s+mich\s+herum$|^(?:mach|gib\s+mir)\s+(?:eine|einen)\s+(?:tour|führung|rundgang)$")
});
static BOARD_RECALL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:bring up|pull up|show me|show|open|put up|get)\s+(?:that|the|my|your)\s+(.+?)\s+report(?:\s+(?:again|from (?:earlier|yesterday|before)|(?:that\s+)?you\s+(?:gave|made|did|showed)\s+(?:me\s+)?(?:earlier|before|yesterday)?))*$")
});
static BOARD_RESEARCH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:do|make|run|put together|prepare|write|build|give)\s+(?:me\s+)?(?:a|an)\s+(.+?)\s+report$|^(?:research|look into)\s+(.+)$|^(?:mach|erstelle)\s+(?:mir\s+)?(?:einen|ein)\s+(.+?)[\s-]*(?:bericht|report)$|^(?:recherchiere|recherchier|erforsche|forsche)\s+(?:mal\s+)?(?:zu\s+|zum\s+|zur\s+|über\s+|nach\s+)?(.+)$")
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

/// "Tidy the windows", "arrange my blocks", "räum die Fenster auf".
static BLOCK_TIDY: LazyLock<Regex> = LazyLock::new(|| {
    // ("My windows" are the desktop's programs: the window tiler's.)
    re(r"^(?:(?:tidy|tidy up|arrange|tile|organi[sz]e|line up|sort out)\s+(?:all\s+)?(?:(?:the\s+|your\s+)?(?:windows|blocks|panels|board|hud)|my\s+(?:blocks|panels|board|hud))(?:\s+up)?|räum(?:e)?\s+(?:die\s+|deine\s+)?(?:fenster|blöcke)\s+auf|ordne\s+(?:die\s+|deine\s+)?(?:fenster|blöcke))$")
});
/// "Find voltage on the board", "search the blocks for Ohm's law", "highlight deadline in the windows".
static BLOCK_FIND: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:find|search for|look for|highlight|filter for)\s+(.+?)\s+(?:on|in|across)\s+(?:all\s+)?(?:the\s+|my\s+|your\s+)?(?:board|blocks|windows|hud|panels|report)|(?:search|filter)\s+(?:all\s+)?(?:the\s+|my\s+|your\s+)?(?:board|blocks|windows|hud|panels|report)\s+for\s+(.+))$")
});
static BLOCK_FIND_CLEAR: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:clear|reset|stop|end|cancel|remove)\s+(?:the\s+|that\s+)?(?:block\s+|board\s+)?(?:search|filter|highlight(?:ing)?)$|^(?:suche|filter|hervorhebung)\s+(?:beenden|löschen|aufheben|zurücksetzen|aus)$"));
/// "Pin the chart", "unpin it", "copy the first note (to the clipboard)", "export the map as JSON".
static BLOCK_MENU: LazyLock<Regex> = LazyLock::new(|| re(r"^(pin|unpin|copy|export)\s+(.+?)(?:\s+to\s+(?:the\s+|my\s+)?clipboard)?(?:\s+(?:as|to|in)\s+(?:a\s+)?(markdown|json|md)(?:\s+file)?)?$"));

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
    // JAI's own blocks, never a program for Windows to look up.
    if let Some(action) = crate::intent_engine::close_command(text) {
        return Some(Intent::Silent(action));
    }
    // "Show me around": the HUD's own parts, one by one, each lit as he names it.
    if HUD_TOUR.is_match(&s) {
        return tool(json!({ "action": "tour" }));
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
        let topic = c.get(1).or(c.get(2)).or(c.get(3)).or(c.get(4))?.as_str().trim();
        let workspace = [" and ", "window", "workspace", "dossier"].iter().any(|w| s.contains(w));
        if !RESEARCH_DURATION.is_match(&s) && !workspace && !topic.is_empty() && topic.split_whitespace().count() <= 8 {
            return tool(json!({ "action": "research", "topic": original_case(text, topic) }));
        }
    }
    // The block menu by voice: "tidy the windows", "find voltage on the board", "clear the
    // search", "pin the chart", "copy the first note", "export the map as JSON".
    if BLOCK_TIDY.is_match(&s) {
        return silent(json!({ "action": "tidy" }), "Tidied");
    }
    if BLOCK_FIND_CLEAR.is_match(&s) {
        return silent(json!({ "action": "find", "target": "" }), "Search cleared");
    }
    if let Some(c) = BLOCK_FIND.captures(&s) {
        let words = c.get(1).or(c.get(2))?.as_str().trim();
        if !words.is_empty() && words.split_whitespace().count() <= 6 {
            return tool(json!({ "action": "find", "target": words }));
        }
    }
    // ("Copy the text of this window" is his window, read by OCR: not a block.)
    if let Some(c) = BLOCK_MENU.captures(&s).filter(|_| !COPY_TEXT.is_match(&s)) {
        let verb = &c[1];
        let target = c[2].trim().trim_start_matches("the ").trim();
        // Only what is a block: a board noun, a block header's words or an open in-app window;
        // "it" only while a board is up (and "copy that" stays the acknowledgement it is).
        let pronoun = matches!(target, "it" | "this" | "that");
        let names_block = (pronoun && active && verb != "copy")
            || target.split_whitespace().any(|w| BOARD_NOUNS.contains(&w))
            || crate::hud_board::names_block(target)
            || !crate::hud_dock::blocks_named(target).is_empty();
        if names_block {
            let format = c.get(3).map(|m| if m.as_str() == "json" { "json" } else { "markdown" });
            return match verb {
                "export" => tool(json!({ "action": "export", "target": target, "format": format.unwrap_or("markdown") })),
                "copy" => silent(json!({ "action": "copy", "target": target }), "Copied"),
                _ => silent(json!({ "action": "pin", "target": target }), if verb == "unpin" { "Unpinned" } else { "Pinned" }),
            };
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
    // "Next page" with nothing on the board is said as it is, at once (the language model took
    // ten seconds over it and then claimed it had closed something). A bare "next" is the
    // music's.
    if !active && BOARD_FLIP.is_match(&s) && ["page", "seite", "note"].iter().any(|w| s.contains(w)) {
        return Some(Intent::Answer("There is nothing to page through, Sir. The board is empty.".into()));
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
// browser" for "open my browser"), so JAI acts instead of only answering with words.

/// JAI himself as the object of a quit verb, including Whisper's spellings. ("Close your
/// window" hides the HUD instead.)
const SELF_REF: &str = r"(?:ur ?self|your ?selfe?|urself|yo ?self|yer ?self|jai|your (?:app|application|program|process|self))";

static SELF_SHUTDOWN_ANY: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        // ("Gliese dich selbst": how the recogniser hears "schließe dich selbst"; "kannst du dich
        // selbst schließen", heard once as "kannte ich selbst schließen".)
        r"\b(?:close|quit|exit|kill|terminate|end)\s+{SELF_REF}\b|\b(?:shut|turn|switch|power)\s+(?:(?:down|off)\s+{SELF_REF}|{SELF_REF}\s+(?:down|off))\b|\b(?:mach|mache)\s+(?:dich|jai)\s+(?:aus|zu|ab)\b|\b(?:schalt|schalte)\s+(?:dich|jai)\s+(?:aus|ab)\b|\b(?:fahr|fahre)\s+(?:dich|jai)\s+(?:selbst\s+)?(?:runter|herunter)\b|\b(?:beende|beend|schließe|schliesse|schließ|schliese|gliese|gließe|gliesse)\s+(?:dich|jai)\b|\b(?:kannst|könntest|koenntest|würdest|wuerdest)\s+du\s+(?:dich|jai)\s+(?:selbst\s+|bitte\s+|mal\s+)*(?:schließen|schliessen|beenden|ausschalten|abschalten|ausmachen|zumachen)\b|\b(?:kannte|kannt)\s+(?:ich|du)\s+(?:dich\s+)?selbst\s+(?:schließen|schliessen|beenden)\b"
    ))
});
/// Whole-sentence forms Sir uses for "quit yourself".
static SELF_SHUTDOWN_EXACT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:go away|kill process|kill the process|turn off|switch off|power off|shut off|get lost|geh weg|mach aus|mach zu|feierabend|schluss für heute|bye bye jai|goodbye jai)$")
});
static NEGATION: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:dont|do not|don t|never|nicht|nie|kein|keine)\b"));
static SLEEP_MODE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:go to sleep|sleep mode|sleep|take a nap|nap time|standby|stand by|standby mode|(?:go|get)\s+(?:on|to|into|in)\s+standby(?:\s+mode)?|enter\s+standby(?:\s+mode)?|schlaf|schlafen|geh schlafen|schlafmodus|ruhemodus|standby modus|bereitschaft|geh\s+(?:in den\s+)?standby)$")
});
/// "Clear the VRAM", "free up my VRAM", "can you move out of my VRAM?", "mach die Grafikkarte
/// frei": every model off the GPU at once (the zero-VRAM standby; Alt+J or a word wakes him).
/// "Game mode on/off" (the VRAM guard's switch; "gaming mode" stays the gaming protocol).
/// "Activate goon mode": a private window of Sir's browser (Sir, 2026-11-21), as Whisper may
/// write "goon" ("gun", "goo", "gooning"). "Game mode" stays the game's.
static GOON_MODE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:activate|enable|start|enter|begin|go\s+in(?:to)?|switch\s+to|turn\s+on|switch\s+on|put\s+on|aktivier(?:e)?|starte?)\s+)?(?:the\s+|den\s+)?(?:goon|goone|gooning|gun|goo)\s*-?\s*(?:mode|modus)(?:\s+(?:on|an|ein|please))?$")
});
static GAME_MODE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:turn|switch|put)\s+(?:on|off)\s+(?:the\s+)?game\s?mode|(?:turn|switch|put)\s+(?:the\s+)?game\s?mode\s+(?:on|off)|game\s?mode\s+(?:on|off)|(?:start|enable|activate|enter|go into|switch to|stop|disable|deactivate|end|leave|exit|quit)\s+(?:the\s+)?game\s?mode|spielmodus\s+(?:an|aus|ein)|(?:starte|beende)\s+(?:den\s+)?spielmodus)(?:\s+please)?$")
});
/// "Is game mode on?", "ist der Spielmodus an?": asked, not ordered.
static GAME_MODE_ASKED: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:is|ist)\s+(?:the\s+|der\s+)?(?:game\s?mode|gaming mode|spielmodus)\s+(?:on|active|running|an|aktiv|ein)$|^(?:am i|are we|bin ich)\s+in\s+(?:the\s+)?(?:game\s?mode|spielmodus)$"));
static VRAM_FLUSH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:clear|free|free up|flush|empty|release|unload|clean|clean up|purge)\s+(?:up\s+)?(?:out\s+)?(?:my\s+|the\s+|your\s+|all\s+(?:of\s+)?(?:my\s+|the\s+)?)?(?:vram|gpu memory|video memory|graphics memory|gpu|graphics card)(?:\s+(?:for me|now|again))?$|^(?:get|move|go|stay|step)\s+out\s+of\s+(?:my\s+|the\s+)?(?:vram|gpu|graphics card|video memory|gpu memory)(?:\s+(?:for me|now|for a while))?$|^(?:leave|give me back)\s+(?:my\s+|the\s+)?(?:vram|gpu|graphics card)(?:\s+(?:alone|free|back))?$|^(?:mach|mache|räum|räume|raeum|leere|leer|gib)\s+(?:meinen\s+|meine\s+|den\s+|die\s+|mir\s+(?:den\s+|die\s+)?)?(?:vram|grafikspeicher|grafikkarte|gpu)\s+(?:frei|leer|zurück|zurueck)$")
});
static VOICE_MUTE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:mute yourself|mute your voice|mute tts|turn off your voice|stummschalten dich|schalte deine stimme aus|deine stimme aus)$")
});
static VOICE_UNMUTE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:unmute yourself|unmute your voice|unmute tts|turn your voice (?:back )?on|you can talk again|you may speak again|deine stimme (?:wieder )?an|du darfst wieder sprechen)$")
});
static RESTART: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^(?:restart|reboot|reload)(?:\s+{SELF_REF})?$|^(?:starte?|start)\s+(?:dich|jai)\s+neu$|^neustart$"))
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
    re(r"\b(?:put|show|display|open|move|place|throw|pop)\s+(?:that|this|it|the)(?:\s+(?:answer|result|results|information|info|text|list|data))?(?:\s+out)?\s+(?:in|into|on|to)\s+(?:a|an|the|its)?\s*(?:second|new|separate|another|extra|own|different)?\s*(?:jai\s+)?window\b|\b(?:zeig|zeige|pack|leg|mach|öffne|oeffne)\s+(?:das|es|dies|diese infos?)\s+(?:in|im)\s+(?:einem|ein|einen|eigenen|neuen|zweiten|separaten|extra)?\s*(?:neuen|zweiten|separaten|eigenen|extra)?\s*fenster\b")
});
static NEW_WINDOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:open|spawn|create|make|give me)\s+(?:a|an|another|one more)\s+(?:new\s+|second\s+|separate\s+|extra\s+)?(?:jai\s+|native\s+)?window$|^(?:öffne|oeffne|mach|erstelle)\s+(?:ein|ein neues|ein zweites|noch ein)\s+(?:neues\s+)?fenster(?:\s+auf)?$|^neues fenster$")
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
static CLOSE_JAI_WINDOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:close|shut)\s+(?:that|this|the|the jai|the new|the second|the separate|the extra|the last|the other)\s+(?:jai\s+)?window$|^(?:schließ|schließe|schliess|schliesse|mach)\s+(?:das|dieses|dein)\s+(?:neue\s+|zweite\s+)?fenster(?:\s+zu)?$")
});
static CLOSE_ALL_JAI_WINDOWS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:close|shut)\s+all\s+(?:of\s+)?(?:your|the|jai|the jai|extra|secondary)?\s*windows$|^(?:schließ|schließe|schliess|schliesse)\s+alle\s+(?:deine\s+|zusätzlichen\s+)?fenster$")
});

/// "Close ur self", "kill process", "mach dich zu" — JAI quitting himself (never negated).
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
        return tool("shutdown_jai", json!({}));
    }
    if let Some(intent) = lifecycle_or_window_command(&s) {
        return Some(intent);
    }
    if TELEMETRY_WINDOW.is_match(&s) {
        return tool("spawn_native_window", json!({ "title": "System status", "telemetry": true, "width": 460, "height": 420 }));
    }
    if WINDOW_FROM_REPLY.is_match(&s) {
        return tool("spawn_native_window", json!({ "title": "J.A.I.", "from_last_reply": true }));
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

/// Whole-sentence lifecycle and JAI-window commands (part of `parse`, ahead of the app rules
/// that would read "close the second window" as an application called "second").
fn lifecycle_or_window_command(s: &str) -> Option<Intent> {
    let tool = |name: &'static str, args: Value| Some(Intent::Tool { name, args });
    let lifecycle = |action: &str| tool("control_jai_lifecycle", json!({ "action": action }));
    if RESTART.is_match(s) {
        return lifecycle("restart_jai");
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
    if CLOSE_ALL_JAI_WINDOWS.is_match(s) {
        return tool("close_native_window", json!({ "id": "all" }));
    }
    if CLOSE_JAI_WINDOW.is_match(s) {
        return tool("close_native_window", json!({ "id": "last", "fallback_to_active_window": true }));
    }
    if NEW_WINDOW.is_match(s) {
        return tool("spawn_native_window", json!({ "title": "J.A.I.", "from_last_reply": true }));
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
        return Some(("shutdown_jai", json!({})));
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
            return Some((a.name, a.args));
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

/// The words no sentence ends on ("… and", "change it to", "öffne den"): unlike "turn it on",
/// "log in" or "mach das", where the last word closes the order. (Not "an": "Mikro an".)
const NEVER_LAST: &[&str] = &[
    "and", "or", "but", "because", "the", "a", "my", "your", "if", "of", "to", "for", "with", "into", "und", "oder", "aber", "weil", "dass",
    "wenn", "der", "den", "dem", "ein", "eine", "einen", "mein", "meine", "meinen", "für", "fuer", "zum", "zur",
];

/// A sentence that surely goes on: it stops on a comma or a dash, on a word no sentence ends on,
/// on a filler or on a correction still to come ("open Chrome, no wait"). JAI gives Sir the
/// time a person would before he acts on it.
pub fn hangs(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.ends_with(['?', '!']) {
        return false;
    }
    let last = t.split_whitespace().last().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).unwrap_or_default();
    t.ends_with([',', '-', '–', '—']) || NEVER_LAST.contains(&last.as_str()) || crate::speech_repair::dangling(t)
}

/// A transcript that sounds unfinished: trailing "…", a dangling connector, or a short fragment
/// that is not a complete command on its own. JAI waits a little longer for the rest.
pub fn looks_incomplete(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if t.ends_with("...") || t.ends_with('…') || hangs(t) {
        return true;
    }
    let last = t
        .split_whitespace()
        .last()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .unwrap_or_default();
    if CONNECTORS.contains(&last.as_str()) {
        // ("Hör nicht zu", "Mikro an": there the "zu" and the "an" close a German verb, and the
        // order is whole, the dispatcher's own. "Don't listen for an …" still waits.)
        let german_lock = matches!(last.as_str(), "zu" | "an")
            && t.to_lowercase().split(|c: char| !c.is_alphanumeric()).any(|w| matches!(w, "hör" | "hoer" | "höre" | "hoere" | "mikro" | "mikrofon"))
            && crate::ear::dispatchers_own(t);
        if !german_lock {
            return true;
        }
    }
    if t.ends_with('?') || t.ends_with('!') {
        return false;
    }
    let words = t.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric)).count();
    // (What the dispatcher carries out itself is whole as it stands: "don't listen" waited
    // here for the rest of a thought, 0.4 s, before the lock came. 2026-11-13.)
    words < 4 && parse(t).is_none() && resolve_fuzzy(t).is_none() && crate::modality::parse_command(t).is_none() && !crate::ear::dispatchers_own(t)
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
        assert_eq!(board("Hey JAI, are you there?", false, false), Some(json!({"action": "wake"})));
        assert_eq!(board("Can you do a state of VR report for me?", false, false), Some(json!({"action": "research", "topic": "state of VR"})));
        assert_eq!(board("Yes please, present the report.", false, false), Some(json!({"action": "present"})));
        assert_eq!(board("Bring up that VR report you gave me earlier", false, false), Some(json!({"action": "recall", "topic": "vr"})));
        assert_eq!(board("Can you maximize the chart please?", true, false), Some(json!({"action": "maximize", "target": "chart"})));
        assert_eq!(board("Maximize that for me", true, false), Some(json!({"action": "maximize", "target": ""})));
        assert_eq!(board("Back to board", true, true), Some(json!({"action": "minimize"})));
        assert_eq!(board("Thanks JAI, you can go to sleep again.", true, false), Some(json!({"action": "sign_off"})));
        assert_eq!(board("Sign off", false, false), Some(json!({"action": "sign_off"})));
        // "Clear the board" takes the blocks away; it is no sign-off (the model stays loaded).
        for text in ["Clear the board", "JAI, clear the HUD", "Clear view", "Board leeren"] {
            assert_eq!(board(text, true, false), Some(json!({"action": "clear"})), "{text}");
        }
        assert_eq!(board("Clear my calendar", true, false), None);
        assert_eq!(board("Show me Tulleys Farm on the map", false, false), Some(json!({"action": "show_map", "place": "Tulleys Farm"})));
        assert_eq!(board("next note", true, false), Some(json!({"action": "flip_note", "direction": "next"})));
        // With nothing on the board a page turn is answered at once, not by the language model
        // (which took ten seconds and claimed to have closed something); a bare "next" stays
        // the music's.
        for text in ["Next page", "Previous page", "Nächste Seite", "Page 3"] {
            assert!(matches!(board_command(text, false, false), Some(Intent::Answer(line)) if line.starts_with("There is nothing to page through")), "{text}");
        }
        assert_eq!(board_command("next", false, false), None);
        assert_eq!(board_command("go back", false, false), None);
        // Voice pagination of a stacked block, and closing the board by name (not as a program).
        for text in ["JAI, next page", "Next slide", "Next result", "Go forward", "Nächste Seite"] {
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
        for text in ["Close the HUD board, please.", "Close the HUD board", "Close web search", "JAI close to web search.", "Schließe das HUD Board", "Mach das Board zu", "HUD Board schließen"] {
            assert_eq!(board(text, true, false), Some(json!({"action": "close"})), "{text}");
            assert!(matches!(parse(text), Some(Intent::Silent(SilentAction { tool: "hud_board", .. }))), "never close_application: {text}");
        }
        // One of JAI's own blocks by name: that block closes, the board stays.
        for (text, target) in [
            ("Close the feature lab", "feature lab"),
            ("Close telemetry", "telemetry"),
            ("Hide the feature lab window", "feature lab"),
            ("Close the theme studio", "theme studio"),
            ("Close the globe", "globe"),
            ("Close the network radar", "network radar"),
            ("Close the disk map", "disk map"),
            ("Schließe die Erde", "erde"),
        ] {
            assert_eq!(board(text, true, false), Some(json!({"action": "close", "target": target})), "{text}");
            assert!(matches!(parse(text), Some(Intent::Silent(SilentAction { tool: "hud_board", .. }))), "never close_application: {text}");
        }
        // Programs and HUD panels are someone else's.
        for text in ["Close Spotify", "Hide the system panel", "Close the settings"] {
            assert_eq!(board(text, true, false), None, "{text}");
        }
    }

    /// What the fast path makes of a spoken command after the wake word (typos fixed first, as
    /// in `brain`).
    fn heard(text: &str) -> Option<Intent> {
        parse(&crate::typos::normalize_user_input_typos(text))
    }

    fn closes_view(text: &str) -> Option<String> {
        match heard(text)? {
            Intent::Silent(SilentAction { tool: "hud_board", args, .. }) | Intent::Tool { name: "hud_board", args } if args["action"] == "close" => args["target"].as_str().map(str::to_string),
            _ => None,
        }
    }

    /// Repro (jai.log, 2026-11-08 23:27-23:42): "JAI, close the Earth, you." asked Windows
    /// for a program called "earth you" ("Earth You isn't running, Sir."); "JAI close up." and
    /// "JAI closed, off." for programs called "up" and "off".
    #[test]
    fn fillers_after_a_close_never_become_a_program() {
        for text in ["close the Earth, you.", "Close the Earth, you", "close the earth please", "close the Earth for me now", "Close the world.", "close the planet, you"] {
            assert_eq!(closes_view(text).as_deref(), Some("earth"), "{text}");
        }
        let own = Some(Intent::Tool { name: "close_application", args: json!({"app_name": "it", "force": true, "own_only": true}) });
        for text in ["close up.", "Close up", "close off", "Close it up, please"] {
            let intent = heard(text);
            assert!(
                intent == own || matches!(&intent, Some(Intent::Tool { name: "close_application", args }) if args["app_name"] == "it"),
                "{text}: {intent:?}"
            );
        }
        // "Closed, off." is no order the parser takes (the model's close_application "off" is
        // caught in `tools::own_close`).
        assert!(!matches!(heard("closed, off."), Some(Intent::Tool { name: "close_application", ref args }) if args["app_name"] != "it"));
        // No filler or pronoun is ever a program's name.
        for text in ["close the Earth, you.", "close up.", "closed, off.", "Close Spotify for me now", "close discord please"] {
            if let Some(Intent::Tool { name: "close_application", args }) = heard(text) {
                let app = args["app_name"].as_str().unwrap_or_default().to_lowercase();
                for filler in ["you", "up", "off", "please", "now", "for me"] {
                    assert!(!app.ends_with(filler), "{text}: '{app}'");
                }
            }
        }
        assert_eq!(heard("Close Spotify for me now"), Some(Intent::Tool { name: "close_application", args: json!({"app_name": "spotify", "force": true}) }));
        // "Close yourself" / "quit you" still name JAI himself.
        for text in ["Close yourself", "Quit you", "Close you"] {
            let intent = heard(text);
            assert!(closes_view(text).is_none() && intent != own, "{text}: {intent:?}");
            if let Some(Intent::Tool { name: "close_application", args }) = &intent {
                assert!(crate::tools::names_jai(args["app_name"].as_str().unwrap_or_default()), "{text}: {intent:?}");
            }
        }
    }

    /// "That's not what I said" takes back the misheard request's change (2026-11-08: "Call me
    /// Earth." became Sir's title); "that's wrong" corrects an answer and stays a sentence.
    #[test]
    fn a_misheard_complaint_takes_the_last_change_back() {
        let undo = Some(Intent::Tool { name: "undo_last_action", args: json!({"within_s": MISHEARD_UNDO_SECS}) });
        for text in ["That's not what I said.", "That is not what I meant", "I didn't say that", "You misheard me", "Das habe ich nicht gesagt", "Du hast mich falsch verstanden"] {
            assert_eq!(heard(text), undo, "{text}");
            assert!(is_undo(text), "{text}");
        }
        assert_ne!(heard("That's wrong"), undo);
        assert!(!is_undo("What did I say?"));
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
        assert!(matches!(parse("Sign off"), Some(Intent::Tool { name: "hud_board", .. })), "sign off no longer quits JAI");
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
    fn the_changes_list_is_closed_by_its_name() {
        let _dock = crate::hud_dock::test_lock();
        crate::hud_dock::begin_frame();
        let at = eframe::egui::Rect::from_min_size(eframe::egui::pos2(40.0, 60.0), eframe::egui::vec2(720.0, 660.0));
        crate::hud_dock::publish_block("changes", "Changes // what is new, better, fixed", at);
        crate::hud_dock::begin_frame();
        // (No program is called "changes list": only the open block makes it an order.)
        let closes = |said: &str| {
            let intent = hud_view_command(said).or_else(|| parse(said));
            matches!(&intent, Some(Intent::Tool { name: "hud_board", args }) | Some(Intent::Silent(SilentAction { tool: "hud_board", args, .. })) if args["action"] == "close")
                || matches!(&intent, Some(Intent::Tool { name: "close_application", .. }))
        };
        assert!(closes("Close the changes list."), "close the changes list");
        assert!(closes("Close the changes."), "close the changes");
        crate::hud_dock::forget("changes");
        crate::hud_dock::begin_frame();
        crate::hud_dock::begin_frame();
        assert!(!matches!(parse("Close the changes list."), Some(Intent::Silent(_))), "with no such block up it is no order for the board");
    }

    #[test]
    fn the_kits_own_things_are_no_widgets_on_the_panel() {
        // Seen live 2026-11-13: both were answered with "there's no … on the panel".
        for (said, tool) in [("Delete the last note.", "notebook"), ("Cancel the sleep timer.", "sleep_timer")] {
            assert!(hud_view_command(said).is_none(), "{said}: {:?}", hud_view_command(said));
            assert!(matches!(parse(said), Some(Intent::Tool { name, .. }) if name == tool), "{said}: {:?}", parse(said));
        }
        // A widget is still taken off the panel by its name.
        assert!(matches!(hud_view_command("Close the timer."), Some(Intent::Tool { name: "remove_widget", .. }) | Some(Intent::Tool { name: "hud_board", .. }) | Some(Intent::Silent(_))), "{:?}", hud_view_command("Close the timer."));
    }

    #[test]
    fn stop_and_time() {
        assert_eq!(parse("Stop!"), Some(Intent::Stop));
        assert_eq!(parse("JAI, sei still."), Some(Intent::Stop));
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
        assert_eq!(tool("Speak louder").unwrap().1["action"], "jai_louder");
    }

    #[test]
    fn apps_and_windows() {
        assert_eq!(tool("Open Spotify"), Some(("open_application", json!({"app_name": "spotify"}))));
        assert_eq!(tool("JAI, öffne bitte Discord."), Some(("open_application", json!({"app_name": "discord"}))));
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
        assert!(is_memory_command("JAI, remember that my sister is called Anna"));
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
    fn shutting_jai_down() {
        let quits = |text: &str| tool(text).map(|(name, _)| name) == Some("shutdown_jai");
        // Phrases from the log that used to fail ("close_application yourself/your").
        for text in [
            "JAI, please close yourself.",
            "bitte fahre JAI herunter",
            "JAI, kill your app.",
            "Shut down.",
            "JAI, shut yourself down",
            "Close JAI",
            "Quit",
            "Turn yourself off",
            "Beende dich",
            "JAI beenden",
            "Schalte dich aus",
            "Fahr herunter",
        ] {
            assert!(quits(text), "should quit: {text}");
        }
        for text in ["Shut down the computer", "Fahr den PC herunter", "Turn down", "Turn off the lights", "Close Spotify"] {
            assert!(!quits(text), "should not quit JAI: {text}");
        }
        assert_eq!(parse("Stop, JAI"), Some(Intent::Stop), "stop only silences");
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
        // "Span over all three monitors" went to the language model, which invented a window.
        for text in [
            "Use all monitors",
            "Span all my screens",
            "Stretch yourself across all monitors",
            "Use both screens",
            "JAI. Span over all three monitors.",
            "Span across all 3 monitors",
            "Spread the HUD over every screen",
            "Extend yourself to all my displays",
            "Span mode on",
            "Turn on span mode",
            "Nutze alle drei Monitore",
            "Erstreck dich über alle Bildschirme",
        ] {
            assert_eq!(hud(text), Some(json!({"span_all_monitors": true})), "{text}");
        }
        for text in [
            "Use only one monitor",
            "Stop spanning",
            "Stop spanning all monitors",
            "Span mode off",
            "Turn off span mode",
            "Go back to one monitor",
            "Back to the main screen",
            "Don't use all monitors anymore",
            "Just use my first monitor",
            "Nutze nur einen Monitor",
        ] {
            assert_eq!(hud(text), Some(json!({"span_all_monitors": false})), "{text}");
        }
        assert_eq!(hud("Toggle span mode"), Some(json!({"span_all_monitors": "toggle"})));
        assert_eq!(hud("Span mode"), Some(json!({"span_all_monitors": "toggle"})));
        // The emblem stays in the middle until Sir tells it to get out of the way.
        for text in ["Move out of the way", "Get out of my way", "Step aside", "You're in the way", "Move the orb out of the way", "Geh aus dem Weg", "Mach Platz"] {
            assert_eq!(hud(text), Some(json!({"orb_board_position": "aside"})), "{text}");
        }
        for text in ["Come back to the middle", "Back to the centre", "You can come back to the center now", "Wieder in die Mitte"] {
            assert_eq!(hud(text), Some(json!({"orb_board_position": "center"})), "{text}");
        }
        assert_eq!(hud("Move the orb to the top left"), Some(json!({"orb_board_position": "top left"})));
        assert_eq!(hud("Put the emblem on the right side"), Some(json!({"orb_board_position": "right side"})));
        assert_eq!(hud("Move the orb to bottom center"), Some(json!({"orb_board_position": "bottom center"})));
        // The screenshot bug: "back" between the orb and the place fell through to the model.
        for (text, spot) in [
            ("JAI, move your orb back to the middle.", "middle"),
            ("Move your orb to the top right", "top right"),
            ("Move your orb left", "left"),
            ("Move the orb slightly left", "slightly left"),
            ("Bewege deinen Orb in die Mitte", "mitte"),
            ("Schieb den Orb nach links", "links"),
        ] {
            assert_eq!(tool(text), Some(("configure_interface", json!({"orb_board_position": spot}))), "{text}");
        }
        // "Move to my 3rd monitor": JAI himself.
        for (text, n) in [("Move to my 3rd monitor", 3), ("JAI, go to monitor 2", 2), ("Move over to the second screen", 2)] {
            assert_eq!(tool(text), Some(("move_window_to_monitor", json!({"app_or_window_name": "jai", "target_monitor": n}))), "{text}");
        }
    }

    /// 2026-11-14, Sir's session: "Can you show me a clock?" had no answer. The clock on the
    /// desktop, the plate beside the corner orb and what comes next are asked for in words; a
    /// question about what comes next is answered by showing it, and changes nothing.
    #[test]
    fn the_desktop_clock_and_what_comes_next_are_asked_for_in_words() {
        let hud = |text: &str| match tool(text) {
            Some(("configure_interface", args)) => Some(args),
            _ => None,
        };
        assert_eq!(hud("Can you show me a clock?"), Some(json!({"desk_clock": true})));
        assert_eq!(hud("JAI, hide the clock"), Some(json!({"desk_clock": false})));
        assert_eq!(hud("Clock with seconds"), Some(json!({"desk_clock_seconds": true})));
        assert_eq!(hud("Hide the words beside the orb"), Some(json!({"orb_words": false})));
        assert_eq!(hud("Make the corner orb smaller"), Some(json!({"corner_orb_size": "smaller"})));
        // What comes next is his to say from a glance at Sir's day that tells it; where none
        // does, the model asks the calendar and the timetable.
        let _turn = crate::idle_screen::GLANCE_TURN.lock().unwrap_or_else(|e| e.into_inner());
        crate::idle_screen::show_glance(crate::idle_screen::Glance::default());
        assert_eq!(tool("What's next?"), None);
        crate::idle_screen::show_glance(crate::idle_screen::Glance { next: Some(("Physics".into(), chrono::Local::now().naive_local() + chrono::Duration::minutes(25), "204".into())), ..Default::default() });
        for asked in ["What's next?", "JAI, what comes next today?", "What is next?"] {
            assert_eq!(hud(asked), Some(json!({"next_now": true})), "{asked}");
            assert!(!beyond_the_question(asked, "configure_interface", &json!({"next_now": true})), "{asked}");
        }
        assert!(!beyond_the_question("What did I miss?", "configure_interface", &json!({"missed": true})));
        // Seen live 2026-11-14: "hide the words beside the orb" hid the orb's panel. The
        // layout's words are asked before the fast path, and took "hide … orb" for theirs.
        for said in ["Hide the words beside the orb", "JAI, show the words next to the orb", "Make the corner orb smaller", "Hide the clock"] {
            assert_eq!(ui_layout_command(said), None, "{said}");
        }
        assert!(ui_layout_command("Make the orb bigger").is_some(), "the orb keeps its own words");
        // Nor does any other router that is asked before the fast path take the new words.
        for said in ["Show me a clock", "Hide the clock", "Clock with seconds", "12 hour clock", "What's next?", "What comes next today?", "Hide the words beside the orb", "Make the corner orb smaller", "Keep the console on top", "Put the console on the left half", "Move the text console to another monitor"] {
            assert!(mixer_command(said).is_none() && internal_view_command(said).is_none() && ui_layout_command(said).is_none(), "{said}");
            assert!(crate::integration_router::capability_topic(said).is_none() && crate::integration_router::route_text(said).is_none() && crate::features::route_text(said).is_none(), "{said}");
        }
        // What time it is stays a reading; the next song stays the music's.
        assert_ne!(hud("What time is it?"), Some(json!({"desk_clock": true})));
        assert_eq!(hud("Next song"), None);
    }

    /// Which core he wears and which theme, by voice: English, German, and as they are said in
    /// the middle of a longer sentence.
    #[test]
    fn the_core_and_the_theme_are_chosen_by_voice() {
        let hud = |text: &str| match tool(text) {
            Some(("configure_interface", args)) => Some(args),
            _ => None,
        };
        for (text, style) in [
            ("Switch to the complex core", "complex"),
            ("JAI, use the simple core", "simple"),
            ("make your core complex", "complex"),
            ("I want the detailed core", "complex"),
            ("the age of ultron core please", "complex"),
            ("set your core to simple", "simple"),
            ("Nimm den komplexen Kern", "complex"),
            ("wechsle zum einfachen Kern", "simple"),
        ] {
            assert_eq!(hud(text), Some(json!({"core_style": style})), "{text}");
        }
        // An order about the core that names neither is not about its style.
        for text in ["show me your core", "make your core bigger", "close the core"] {
            assert!(hud(text).is_none_or(|args| args.get("core_style").is_none()), "{text}: {:?}", hud(text));
        }
        for (text, theme) in [
            ("switch to the black and white theme", "mono"),
            ("Set theme to Arc Reactor", "arc_reactor"),
            ("the hot rod theme", "hot_rod"),
            ("Stell das Theme auf Gamma", "gamma"),
            ("switch to the arc cyan theme", "arc_reactor"),
            ("graphite theme", "graphite"),
        ] {
            assert_eq!(hud(text), Some(json!({"hud_palette": theme})), "{text}");
        }
        // A theme that is being made is not one of the presets, whatever words it shares with one.
        assert_eq!(hud("design a dark graphite theme").and_then(|args| args.get("hud_palette").cloned()), None);
    }

    #[test]
    fn restyling_the_hud() {
        let hud = |text: &str| match tool(text) {
            Some(("configure_interface", args)) => Some(args),
            _ => None,
        };
        assert_eq!(hud("JAI, make the interface red"), Some(json!({"accent_color": "red"})));
        assert_eq!(hud("Mach das Interface rot"), Some(json!({"accent_color": "rot"})));
        assert_eq!(hud("Ändere die Farbe zu Grün"), Some(json!({"accent_color": "grün"})));
        assert_eq!(hud("Switch to the Iron Man theme"), Some(json!({"hud_palette": "hot_rod"})));
        assert_eq!(hud("Dark mode"), Some(json!({"hud_palette": "graphite"})));
        assert_eq!(hud("Compact mode"), Some(json!({"layout": "compact"})));
        assert_eq!(hud("Kompaktmodus"), Some(json!({"layout": "compact"})));
        // "The chat" is his conversation block, never the chat application (Discord).
        for (said, shown) in [("Close the chat", false), ("Hide the conversation", false), ("Show me the chat again", true), ("Open the comms block", true), ("Blende den Chat aus", false), ("Chat einblenden", true)] {
            assert_eq!(hud(said), Some(json!({"comms_block": shown})), "{said}");
        }
        assert!(matches!(tool("Close Discord"), Some(("close_application", _))), "an application by its name is still closed");
        assert_eq!(hud("Make the background dark blue"), Some(json!({"accent_color": "dark blue"})));
        assert_eq!(hud("Roter Hintergrund"), Some(json!({"accent_color": "roter"})));
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
        assert_eq!(hud("Reset the interface"), Some(json!({"reset": true})));
        assert_eq!(hud("Show yourself"), Some(json!({"window": "show"})));
        assert_eq!(hud("Versteck dich"), Some(json!({"window": "hide"})));
        assert_eq!(hud("Minimize yourself"), Some(json!({"window": "minimize"})));
        assert_eq!(hud("Move yourself to the top right corner"), Some(json!({"window": "top_right"})));
        assert_eq!(hud("Schieb dich nach links"), Some(json!({"window": "left"})));
    }

    #[test]
    fn the_orbs_size_is_set_at_once_and_nothing_else_is_the_layouts() {
        let ui = |text: &str| ui_layout_command(text);
        assert_eq!(ui("Make the orb bigger"), Some(json!({"action": "orb_bigger"})));
        assert_eq!(ui("Make the sphere smaller"), Some(json!({"action": "orb_smaller"})));
        assert_eq!(ui("Set the orb size to 150 percent"), Some(json!({"action": "set_orb_scale", "scale_value": 1.5})));
        // (Panels and a zen mode were settings no part of the HUD read any more: gone.)
        for text in ["Open Spotify", "Close Discord", "Turn off the microphone", "Show me the weather", "Clean up my downloads folder", "Remove the reminder", "Zen mode", "Hide the system panel"] {
            assert_eq!(ui(text), None, "{text}");
        }
        assert_eq!(tool("Pick some good music"), Some(("control_music", json!({"action": "pick_music"}))));
        assert_eq!(tool("Play something"), Some(("control_music", json!({"action": "pick_music"}))));
    }

    #[test]
    fn follow_ups_act_on_the_focus_never_on_jai() {
        let window = |text: &str| match tool(text) {
            Some(("window_management", args)) if args.get("target_window").is_none() => args["action"].as_str().map(String::from),
            other => panic!("{text}: {other:?}"),
        };
        for text in ["Full screen", "fullscreen", "JAI, go full screen", "Make it full screen", "Put that in fullscreen", "Vollbild", "Mach es auf Vollbild", "full screen please"] {
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
        // JAI himself only when Sir names him.
        assert_eq!(tool("Maximize JAI"), Some(("configure_interface", json!({"window": "maximize"}))));
        assert_eq!(tool("JAI, minimise yourself"), Some(("configure_interface", json!({"window": "minimize"}))));
        assert_eq!(tool("Make yourself full screen"), Some(("configure_interface", json!({"fullscreen": true}))));
        assert_eq!(tool("Close yourself").map(|t| t.0), Some("shutdown_jai"));
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
        assert_eq!(web("JAI, go to BastiGHG's YouTube channel"), t("youtube", "channel_or_profile", "BastiGHG"));
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
    fn a_volume_is_read_off_the_mixer_when_asked_for() {
        for said in ["What's my volume?", "What is the volume?", "What's the current volume?", "How loud is it?", "Wie laut ist es?", "Wie hoch ist die Lautstärke?"] {
            assert_eq!(mixer_command(said), Some(json!({"action": "get_master"})), "{said}");
        }
        let app = |name: &str| Some(json!({"action": "get_app", "app": name}));
        assert_eq!(mixer_command("How loud is Discord?"), app("discord"));
        assert_eq!(mixer_command("What's Discord's volume?"), app("discord"));
        assert_eq!(mixer_command("Is my browser muted?"), app("browser"));
        assert_eq!(mixer_command("How loud is the game?"), app("game"));
        assert_eq!(mixer_command("Wie laut ist Discord?"), app("discord"));
        assert_eq!(mixer_command("Ist Discord stumm?"), app("discord"));
        assert_eq!(mixer_command("Is anything muted?"), Some(json!({"action": "muted"})));
        assert_eq!(mixer_command("What's muted?"), Some(json!({"action": "muted"})));
        assert_eq!(mixer_command("Ist irgendwas stumm?"), Some(json!({"action": "muted"})));
        // A level of something else is no question for the mixer.
        for said in ["What is the volume of a sphere?", "How loud is a jet engine?", "What is the water level?", "Is the mic muted?"] {
            assert_eq!(mixer_command(said), None, "{said}");
        }
    }

    #[test]
    fn a_named_mix_is_saved_loaded_listed_and_deleted_by_voice() {
        let call = |action: &str, name: &str| json!({ "action": action, "name": name });
        for (said, wanted) in [
            ("Save this mix as gaming", call("save_mix", "gaming")),
            ("Save the current volumes as late night", call("save_mix", "late night")),
            ("Remember this mix as my movie mix", call("save_mix", "movie")),
            ("Speicher diesen Mix als Gaming", call("save_mix", "gaming")),
            ("Load my gaming mix", call("load_mix", "gaming")),
            ("Switch to the movie mix", call("load_mix", "movie")),
            ("Lade den Gaming Mix", call("load_mix", "gaming")),
            ("Delete the gaming mix", call("delete_mix", "gaming")),
            ("Lösch den Gaming Mix", call("delete_mix", "gaming")),
            ("Which mixes do I have?", json!({"action": "list_mixes"})),
            ("List my mixes", json!({"action": "list_mixes"})),
            ("Welche Mixe habe ich?", json!({"action": "list_mixes"})),
        ] {
            assert_eq!(mixer_command(said), Some(wanted), "{said}");
        }
        // A playlist of that sort is the music's; a name nobody saved is nothing.
        for said in ["Play my classic rock mix", "Put on my classic rock mix", "classic rock mix", "the mix of the two", "Mix it up"] {
            assert_eq!(mixer_command(said), None, "{said}");
        }
    }

    #[test]
    fn what_he_just_did_and_what_is_new_are_read_not_guessed() {
        for said in ["What did you just do?", "What have you done?", "What did you change?", "What was the last thing you did?", "Was hast du gerade gemacht?", "Was hast du zuletzt geändert?"] {
            assert_eq!(tool(said), Some(("recent_actions", json!({}))), "{said}");
        }
        for said in ["What's new?", "What's new in this version?", "Any new features?", "Was ist neu?", "Was gibt's Neues?"] {
            assert_eq!(tool(said), Some(("whats_new", json!({}))), "{said}");
        }
        // The news is a question of its own.
        assert_ne!(tool("What's new in the news?").map(|t| t.0), Some("whats_new"));
    }

    #[test]
    fn undo_that_takes_back_the_last_change_of_his_own() {
        for said in ["Undo that", "Undo", "Undo it", "Revert that", "Undo that change", "Undo the last change", "Put it back", "Put that back the way it was", "Take that back", "Never mind, undo that", "Mach das rückgängig", "Nimm das zurück", "Rückgängig"] {
            assert_eq!(tool(said), Some(("undo_last_action", json!({}))), "{said}");
        }
        // Naming what is to be undone keeps it the tool it always was.
        assert_eq!(tool("Undo that edit"), Some(("undo_last_file_edit", json!({}))));
        assert_eq!(tool("Undo that volume change").map(|t| t.0), Some("control_audio_mixer"));
        // … and something else that goes back stays what it was.
        assert_ne!(tool("Put the orb back").map(|t| t.0), Some("undo_last_action"));
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
        assert_eq!(style("Make that window emerald"), json!({"accent_color": "emerald", "target": "focus"}));
        assert_eq!(style("Make that window arc reactor"), json!({"theme": "arc reactor", "target": "focus"}));
        assert_eq!(style("Pin that window"), json!({"always_on_top": true, "target": "focus"}));
        assert_eq!(style("Unpin that window"), json!({"always_on_top": false, "target": "focus"}));
        // "The window" / "your window" is still the HUD.
        assert_eq!(tool("Make the window more transparent"), Some(("configure_interface", json!({"opacity": "-15"}))));
    }

    #[test]
    fn hud_rules_leave_other_requests_alone() {
        // (With none of his blocks on the board: "show the desktop" means Sir's windows then.
        // Another test's blocks, drawn at the same moment, made it mean his own.)
        let _dock = crate::hud_dock::test_lock();
        crate::hud_dock::begin_frame();
        crate::hud_dock::begin_frame();
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
            "JAI, turn yourself off now",
            "Could you power yourself down for today",
        ] {
            assert_eq!(fuzzy(text).map(|(n, _)| n), Some("shutdown_jai"), "{text}");
        }
        for text in ["Don't close yourself", "Close Spotify", "Turn off the lights", "Shut down the computer"] {
            assert_ne!(fuzzy(text).map(|(n, _)| n), Some("shutdown_jai"), "{text}");
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
        let web = fuzzy("Could you maybe open the web browser for me?").unwrap();
        assert_eq!((web.0, crate::apps::find_spec(web.1["app_name"].as_str().unwrap()).map(|s| s.id)), ("open_application", Some("browser")));
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
        assert_eq!(fuzzy("Restart yourself").unwrap().1["action"], "restart_jai");
        assert_eq!(fuzzy("Mute your voice").unwrap().1["action"], "mute_tts");
    }

    #[test]
    fn assistant_abilities_are_instant() {
        let name = |text: &str| tool(text).map(|(n, _)| n);
        assert_eq!(name("Good morning, JAI"), Some("run_executive_briefing"));
        assert_eq!(name("Brief me"), Some("run_executive_briefing"));
        assert_eq!(name("Was steht heute an?"), Some("run_executive_briefing"));
        assert_eq!(name("JAI, give me my morning briefing"), Some("run_executive_briefing"));
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
        // A timer of his own name: its own ring, its own line when it rings.
        for (said, id, title, secs) in [
            ("Set a 10 minute pasta timer", "timer_pasta", "pasta timer", 600.0),
            ("Start a pizza timer for 12 minutes", "timer_pizza", "pizza timer", 720.0),
            ("Stell einen Nudeltimer auf 8 Minuten", "timer_nudel", "nudel timer", 480.0),
            ("Set the timer for 5 minutes", "timer_300", "5 minute timer", 300.0),
            ("Stell den Timer auf 2 Minuten", "timer_120", "2 minute timer", 120.0),
            ("set an egg timer for 4 minutes", "timer_240", "4 minute timer", 240.0),
        ] {
            let t = tool(said).unwrap_or_else(|| panic!("{said}")).1;
            assert_eq!((t["id"].as_str(), t["title"].as_str(), t["duration_secs"].as_f64()), (Some(id), Some(title), Some(secs)), "{said}");
        }
        assert_eq!(tool("Focus mode for 45 minutes").unwrap().1, json!({"action": "start", "minutes": 45.0}));
        assert_eq!(tool("Engage focus mode").unwrap().1["minutes"], 25.0);
        assert_eq!(tool("Stop focus mode").unwrap().1["action"], "stop");
        assert_eq!(name("Copy that"), Some("clipboard"));
        assert_eq!(tool("Copy the text of this window").unwrap(), ("inspect_screen_or_selection", json!({"mode": "copy_text"})));
        assert_eq!(tool("Copy the text on my screen.").unwrap().1, json!({"mode": "copy_text", "target_monitor": "primary"}));
        assert_eq!(tool("Kopier den Text aus dem Fenster").unwrap().1, json!({"mode": "copy_text"}));
        assert_eq!(name("Copy the text of this window into an email"), None);
        assert_eq!(name("Am I online?"), Some("network_status"));
        assert_eq!(name("What's my IP address"), Some("network_status"));
        assert_eq!(name("What's slowing down my PC?"), Some("top_processes"));
        assert_eq!(name("Warum ist mein PC so langsam?"), Some("top_processes"));
        assert_eq!(tool("Speak a bit faster").unwrap().1["action"], "jai_faster");
        assert_eq!(tool("Find my tax return pdf").unwrap().1, json!({"query": "tax return pdf"}));
        assert_eq!(tool("Open budget.xlsx").unwrap().1, json!({"query": "budget.xlsx", "open": true}));
        assert_eq!(name("Find a good restaurant nearby"), None, "not a file");
        assert_eq!(parse("What's 15 percent of 240?"), Some(Intent::Answer("15 percent of 240 is 36, Sir.".into())));
        assert!(matches!(parse("Convert 5 miles to km"), Some(Intent::Answer(_))));
        assert_eq!(upfront_tool("What's on my screen?"), Some(("inspect_screen_or_selection", json!({"mode": "monitor", "target_monitor": "primary"}))));
        assert_eq!(upfront_tool("Can you explain this error?"), Some(("inspect_screen_or_selection", json!({"mode": "active_window"}))));
        assert_eq!(upfront_tool("JAI, what does this error mean?").unwrap().1["mode"], "active_window");
        assert_eq!(upfront_tool("Look at my second monitor"), Some(("inspect_screen_or_selection", json!({"mode": "monitor", "target_monitor": 2}))));
        assert_eq!(upfront_tool("Summarize what's on monitor 1").unwrap().1["target_monitor"], 1);
        // (Sir, 2026-11-14: he was told what his first monitor showed.)
        assert_eq!(upfront_tool("What can you see on my second monitor?"), Some(("inspect_screen_or_selection", json!({"mode": "monitor", "target_monitor": 2}))));
        assert_eq!(upfront_tool("What do you see on the left screen?").unwrap().1["target_monitor"], "left");
        assert_eq!(upfront_tool("Was siehst du auf dem zweiten Bildschirm?").unwrap().1["target_monitor"], 2);
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
        assert_eq!(claimed_action("As you wish, Sir. Powering down.", "JAI you should exit now").map(|(n, _)| n), Some("shutdown_jai"));
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
        assert_eq!(live("JAI, can you create a new window please that shows me my PC stats?"), json!({"live": "pc_stats"}));
        assert_eq!(live("Please create a new window showing my remaining space on my drives."), json!({"live": "drive_space"}));
        assert_eq!(live("Show my CPU and GPU usage in a new window on my second monitor"), json!({"live": "pc_stats", "monitor": 2}));
        assert_eq!(live("Open a window with my drives and processes")["add_content"], json!(["processes"]));
        assert_eq!(live("Zeig mir meinen Speicherplatz in einem neuen Fenster")["live"], "drive_space");
        assert_eq!(live("Create a 800x600 window with my network traffic"), json!({"live": "network", "width": 800, "height": 600}));
        // Not live views: these stay what they were.
        assert_eq!(tool("Close that window"), Some(("close_native_window", json!({"id": "last", "fallback_to_active_window": true}))));
        assert!(tool("Open a new window").is_some_and(|(_, a)| a.get("live").is_none()));
    }

    #[test]
    fn jai_windows_are_resized_restyled_and_extended() {
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

    /// Sonora's queue and switches, by voice in both languages (2026-11-21).
    #[test]
    fn sonoras_queue_and_switches_are_ordered_by_voice() {
        let m = |text: &str| match tool(text) {
            Some(("control_music", a)) => a,
            other => panic!("{text}: {other:?}"),
        };
        for said in ["What's in the queue?", "Show me the queue", "Show me what's in the queue", "What's the next song?", "Zeig mir die Warteschlange", "Welches Lied kommt als Nächstes?", "Was läuft gerade in der Warteschlange?"] {
            assert_eq!(m(said), json!({"action": "queue_show"}), "{said}");
        }
        // Asked as a question, it must pass the guard that lets a question only read.
        assert!(crate::hyper_cortex::is_read_only("control_music", &json!({"action": "queue_show"})));
        assert_eq!(m("Add Thunderstruck by AC/DC to the queue"), json!({"action": "queue_add", "song_name": "thunderstruck", "artist_name": "ac dc"}));
        assert_eq!(m("Queue up Highway to Hell"), json!({"action": "queue_add", "song_name": "highway to hell"}));
        assert_eq!(m("Play Highway to Hell next"), json!({"action": "play_next", "song_name": "highway to hell"}));
        assert_eq!(m("Füge Highway to Hell zur Warteschlange hinzu"), json!({"action": "queue_add", "song_name": "highway to hell"}));
        for (said, state) in [("Turn on shuffle", "on"), ("Shuffle off", "off"), ("Stop shuffling", "off"), ("Zufallswiedergabe an", "on"), ("Shuffle", "on")] {
            assert_eq!(m(said), json!({"action": "set_shuffle", "state": state}), "{said}");
        }
        for (said, state) in [("Repeat this song", "one"), ("Put this song on repeat", "one"), ("Repeat the playlist", "all"), ("Turn off repeat", "off"), ("Wiederhole das Lied", "one"), ("Wiederholung aus", "off")] {
            assert_eq!(m(said), json!({"action": "set_repeat", "state": state}), "{said}");
        }
        for (said, position) in [("Skip ahead 30 seconds", "+30"), ("Go back 10 seconds", "-10"), ("Fast forward 2 minutes", "+120"), ("Spul 30 Sekunden vor", "+30"), ("Jump to 1:30", "1:30"), ("Restart the song", "start")] {
            assert_eq!(m(said), json!({"action": "seek", "position": position}), "{said}");
        }
        for (said, action) in [("Like this song", "like"), ("Save this song to my liked songs", "like"), ("Unlike this song", "unlike"), ("Clear the queue", "clear_queue"), ("Add this song to my workout playlist", "edit_playlist")] {
            assert_eq!(m(said)["action"], action, "{said}");
        }
        assert_eq!(m("Shuffle my workout playlist"), json!({"action": "shuffle", "playlist_name": "workout"}));
        // What they were before: the next song, a bare skip, a step with no direction.
        assert_eq!(m("Next song"), json!({"action": "next_track"}));
        assert_eq!(m("Skip"), json!({"action": "next_track"}));
        for said in ["Queue up", "Play something next", "Wait 10 seconds"] {
            assert!(tool(said).is_none_or(|(_, a)| !matches!(a["action"].as_str(), Some("queue_add" | "play_next" | "seek"))), "{said}");
        }
    }

    #[test]
    fn protocols_capture_and_engage() {
        let _store = crate::protocols::ProtocolStore::in_memory();
        assert_eq!(
            tool("JAI, save my current setup as Gaming Protocol"),
            Some(("manage_stark_protocol", json!({"action": "capture_current_workspace", "protocol_name": "gaming"})))
        );
        assert_eq!(tool("Save this layout as movie night").unwrap().1["protocol_name"], "movie night");
        assert_eq!(tool("Initiate coding protocol"), Some(("manage_stark_protocol", json!({"action": "execute_protocol", "protocol_name": "coding"}))));
        assert_eq!(tool("Run Clean Slate Protocol").unwrap().1["protocol_name"], "clean slate");
        assert_eq!(tool("Engage the Music & Chill Protocol").unwrap().1["protocol_name"], "music and chill");
    }

    #[test]
    fn confirmations_and_reply_claims() {
        for yes in ["Yeah, do that please?", "do it", "Yes please", "Yes", "mach das", "Ja, bitte", "Go ahead", "Sure, JAI"] {
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
        // What surely goes on gets the time a person would give; "…" and a short fragment only
        // the short wait, since Whisper writes "…" after a whole order that trails off.
        for goes_on in ["Open Spotify and", "Change it to", "Set a timer for,", "Open the-", "Öffne den", "Open Chrome, no wait", "Set a timer for five minutes and uh", "Spiel Musik und"] {
            assert!(hangs(goes_on) && looks_incomplete(goes_on), "{goes_on}");
        }
        for whole in ["Turn it on", "Log in", "Mach das", "Where are you from?", "Open Spotify...", "Default.", "Mikro an", "Hör nicht zu", "Open Chrome, no wait, Firefox"] {
            assert!(!hangs(whole), "{whole}");
        }
    }

    /// 2026-11-13: "Jay, don't listen." was whole, and the lock still waited 0.4 s for the rest
    /// of a thought: no parser here knows the orders the dispatcher carries out itself.
    #[test]
    fn the_privacy_lock_is_no_unfinished_thought() {
        for whole in ["don't listen", "stop listening", "Mic off", "mute my mic", "listen again", "Hör nicht zu", "Hör zehn Minuten nicht zu", "Mikro an", "Hör wieder zu"] {
            assert!(!looks_incomplete(whole), "{whole}");
        }
        // What does wait, as before: a fragment, a lock whose end is still to come, and a
        // German sentence that stops at its preposition.
        assert!(looks_incomplete("don't listen for"));
        assert!(looks_incomplete("don't listen for an"));
        assert!(looks_incomplete("Change it to"));
        assert!(looks_incomplete("Ich gehe zu"));
        assert!(looks_incomplete("Hör mal zu"), "no order of the lock's: he waits for the rest as ever");
    }

    /// "How long until you listen again?" is answered from the lock's own clock, typed or
    /// spoken, and by nothing that stands before the parser on the fast path.
    #[test]
    fn how_long_the_lock_still_holds_is_answered_without_the_model() {
        let _lock = crate::listening_lock::test_lock();
        crate::listening_lock::apply(crate::listening_lock::LockCommand::Resume);
        for asked in ["How long until you listen again?", "JAI, when will you listen again?", "Wie lange hörst du noch nicht zu?", "Wann hörst du wieder zu?"] {
            assert_eq!(parse_as_said(asked), Some(Intent::Answer("I'm listening now, Sir.".to_string())), "{asked}");
            assert!(matches!(crate::brain::fast_as_said(asked), Some(crate::brain::Fast::Intent(Intent::Answer(_)))), "{asked}: the parser's, as it was said");
            assert!(only_asks(asked), "{asked}: a question, which changes nothing");
        }
        crate::listening_lock::apply(crate::listening_lock::LockCommand::Lock);
        assert_eq!(parse_as_said("how long until you listen again"), Some(Intent::Answer("Until you tell me I can listen again, Sir.".to_string())));
        crate::listening_lock::apply(crate::listening_lock::LockCommand::Pause(std::time::Duration::from_secs(600)));
        assert_eq!(parse_as_said("How long until you listen again?"), Some(Intent::Answer("Another 10 minutes, Sir.".to_string())));
        crate::listening_lock::apply(crate::listening_lock::LockCommand::Resume);
        // The older questions about the microphone keep their answers.
        assert_eq!(parse_as_said("Are you listening?"), Some(Intent::Answer("I'm listening, Sir.".to_string())));
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
        assert!(!looks_like_app_name("three random windows") && !looks_like_app_name("drei zufällige fenster") && looks_like_app_name("windows terminal"));
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
        // ("Settings" alone is the panel with every switch; the interface's own window by its name.)
        assert_eq!(tool("Einstellungen öffnen"), Some(("configure_interface", json!({"settings_panel": true}))));
        assert_eq!(silent_quick_command("Weitermachen.").map(|a| a.args), Some(json!({"action": "resume"})));
    }

    #[test]
    fn misheard_core_routes_to_the_core_sphere_never_a_vehicle() {
        let routed = |heard: &str| parse(&crate::typos::normalize_user_input_typos(heard));
        let close = Some(Intent::Tool { name: "configure_interface", args: json!({"core_view": false}) });
        let open = Some(Intent::Tool { name: "configure_interface", args: json!({"core_view": true}) });
        // The transcript in Sir's screenshot.
        for heard in ["Close your car.", "No. Close your car.", "Right, hide your call.", "Hide your for C-O-R-E.", "Hide your cord", "JAI, hide your core", "close your c o r e", "Hide core", "Verstecke deinen Kern"] {
            assert_eq!(routed(heard), close, "{heard}");
        }
        for heard in ["Show your car", "Open your call", "JAI, show me your core", "Show your corps", "open your kor", "Zeig mir deinen Kern"] {
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
        // The HUD's own tour, not the board's.
        let tour = Some(Intent::Tool { name: "hud_board", args: json!({"action": "tour"}) });
        for text in ["Show me around", "Can you show me around the HUD?", "Give me a tour of your interface", "Zeig mir alles", "Führ mich herum"] {
            assert_eq!(parse(text), tour, "{text}");
        }
        assert_ne!(parse("Zeig mir alles über Rom"), tour, "a question about Rome is no tour");
    }

    #[test]
    fn going_out_is_routed_to_the_venue_guide() {
        let venue = parse("Hey JAI, where can I take my fiancée for a super serious Halloween night?");
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
        assert_eq!(call("JAI, listen to my Discord call").as_deref(), Some("listen"));
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
        for (text, want) in [("change the theme to Stark", "stark_gold"), ("JAI, switch to the Stark look", "stark_gold"), ("make the interface yellow", "stark_gold"), ("change your color to red", "hot_rod"), ("JAI, make the interface gold", "stark_gold"), ("change your colour to blue", "arc_reactor"), ("Ändere deine Farbe auf Grün", "gamma"), ("Mach dich lila", "vibranium"), ("change your colour to orange", "ember"), ("make the interface black and white", "mono")] {
            assert_eq!(palette(text), want, "{text}");
        }
        // The colour lands on the whole HUD's palette.
        let done = crate::interface::configure(&crate::interface::InterfaceSettings::default(), &json!({"accent_color": "gold"})).unwrap();
        assert_eq!(done.settings.hud_palette, "stark_gold");
        let done = crate::interface::configure(&crate::interface::InterfaceSettings::default(), &json!({"accent_color": "rot"})).unwrap();
        assert_eq!(done.settings.hud_palette, "hot_rod");
        let done = crate::interface::configure(&crate::interface::InterfaceSettings::default(), &json!({"accent_color": "#8B4513"})).unwrap();
        assert_eq!(done.settings.hud_palette, "custom:#8B4513");
        // Presets by name.
        assert_eq!(hud_view_command("switch to the arc cyan theme"), Some(Intent::Tool { name: "configure_interface", args: json!({"hud_palette": "arc_reactor"}) }));
        assert_eq!(hud_view_command("ultraviolet theme"), Some(Intent::Tool { name: "configure_interface", args: json!({"hud_palette": "vibranium"}) }));
    }

    #[test]
    fn a_misheard_wake_word_still_reaches_the_hud_command() {
        // Sir's 23:37 voice turns that ended in the fallback line.
        assert_eq!(parse_misheard("Now this opens your settings."), Some(Intent::Tool { name: "configure_interface", args: json!({"settings_panel": true}) }));
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
    fn screen_restore_and_the_navigation_tabs_by_voice() {
        let board = |args: Value| Some(Intent::Tool { name: "hud_board", args });
        let iface = |args: Value| Some(Intent::Tool { name: "configure_interface", args });
        for text in ["JAI, bring back the screen", "bring the screen back", "restore the screen", "screen back on", "back to the board"] {
            assert_eq!(parse(text), board(json!({"action": "restore"})), "{text}");
        }
        assert_eq!(parse("go to the sim lab"), iface(json!({"hud_view": "sim_lab"})));
        assert_eq!(parse("home view"), iface(json!({"hud_view": "home"})));
        // Nothing docks any more: the old orders are answered in one line and never reach the
        // model (which took "dock all windows" for Sir's own programs' windows).
        for text in ["Dock all windows back inside JAI", "dock it back", "Detach the telemetry window", "pop out the function plotter", "undock the schedule"] {
            assert!(matches!(parse(text), Some(Intent::Answer(ref line)) if line.contains("nothing to dock")), "{text}: {:?}", parse(text));
        }
        assert!(!matches!(parse("docker is not starting"), Some(Intent::Answer(_))), "a word that only begins so");
        // ("In a new window" is where every view opens anyway.)
        assert_eq!(internal_view_command("open my system stats"), Some(json!({"view": "system_stats"})));
        assert_eq!(internal_view_command("open my system stats in a new window"), Some(json!({"view": "system_stats"})));
        assert_eq!(internal_view_command("open the simulation lab in a separate window"), Some(json!({"view": "simulation_lab"})));
    }

    #[test]
    fn core_view_and_llm_reboot_by_voice() {
        let open = Some(("configure_interface", json!({"core_view": true})));
        for text in ["JAI, show me your core", "Open your core", "Core view", "Zeig mir deinen Kern"] {
            assert_eq!(tool(text), open, "{text}");
        }
        assert_eq!(tool("Close your core"), Some(("configure_interface", json!({"core_view": false}))));
        assert_eq!(tool("Make your core bigger"), Some(("configure_interface", json!({"core_scale": "bigger"}))));
        assert_eq!(tool("Set your core to 200 percent"), Some(("configure_interface", json!({"core_scale": 200.0}))));
        assert_eq!(tool("Rotate your core"), Some(("configure_interface", json!({"core_spin": "faster"}))));
        assert_eq!(tool("Spin faster"), Some(("configure_interface", json!({"core_spin": "faster"}))));
        for text in ["JAI, reboot your language model", "Restart LLM", "Reboot brain", "Starte dein Sprachmodell neu"] {
            assert_eq!(tool(text).map(|t| t.0), Some("reboot_language_model"), "{text}");
        }
        // "Restart Spotify" is still an app.
        assert_ne!(tool("Restart Spotify").map(|t| t.0), Some("reboot_language_model"));
    }

    #[test]
    fn text_console_and_windowed_mode_by_voice() {
        let open = Some(("configure_interface", json!({"text_console": true})));
        let close = Some(("configure_interface", json!({"text_console": false})));
        for text in ["Open text interface", "JAI, open the text interface.", "Show the text console", "Pop up the text field", "Textkonsole öffnen"] {
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
        for text in ["Pause", "JAI, pause.", "Stop", "Stopp!", "Halt", "stop music", "Pause the music", "JAI stop", "Musik pausieren", "stop playing"] {
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
        // (What "close" alone does depends on what is up on the board: asked of the rule itself,
        // since other tests put blocks up while this one runs.)
        assert!(quick("Close").is_some());
        assert_eq!(close_alone(false), ("configure_interface", json!({"window": "hide"}), "To the tray"));
        assert_eq!(close_alone(true), ("close_all_secondary_windows", json!({}), "Closed"));
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
        assert_eq!(hud("Make it darker"), Some(json!({"theme": "dark"})));
        assert_eq!(hud("Make the interface a lot darker"), Some(json!({"theme": "dark"})));
        assert_eq!(hud("mach es dunkler"), Some(json!({"theme": "dark"})));
        assert_eq!(hud("A bit lighter"), Some(json!({"theme": "light"})));
        for text in ["Change the interface back to default", "Change it back to those default", "back to the defaults", "Reset the interface", "Stell alles wieder auf Standard"] {
            assert_eq!(hud(text), Some(json!({"reset": true})), "{text}");
        }
        // A bare "close" hides the HUD; quitting needs JAI named.
        assert_eq!(tool("Close"), Some(("configure_interface", json!({"window": "hide"}))));
        assert_eq!(tool("Close JAI").map(|(n, _)| n), Some("shutdown_jai"));
        assert_eq!(tool("Close yourself").map(|(n, _)| n), Some("shutdown_jai"));
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
        assert_eq!(tool("Only listen when I say JAI"), Some(("set_listening_mode", json!({"mode": "wake_word"}))));
        assert_eq!(tool("Only listen when I say Jay"), Some(("set_listening_mode", json!({"mode": "wake_word"}))));
        let mix = tool("Set Sonora to 30% and Discord to 100%").unwrap();
        assert_eq!(mix.0, "control_audio_mixer");
        assert_eq!(mix.1["apps"], json!([{"app": "sonora", "level": 30}, {"app": "discord", "level": 100}]));
        assert_eq!(tool("Mach Spotify auf 20 Prozent").unwrap().1["apps"], json!([{"app": "spotify", "level": 20}]));
        assert_eq!(tool("Mute Chrome"), Some(("control_audio_mixer", json!({"action": "mute_app", "app": "chrome"}))));
        assert_eq!(tool("Lower the volume by 5%"), Some(("control_audio_mixer", json!({"action": "change_master", "delta": -5}))));
        // "It" is what the last volume request was about (the mixer takes the master volume
        // when there was none).
        assert_eq!(tool("Set it back to 100%"), Some(("control_audio_mixer", json!({"action": "set_app", "app": "it", "level": 100}))));
        assert_eq!(tool("Set the volume back to 100%"), Some(("control_audio_mixer", json!({"action": "set_master", "level": 100}))));
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
        for text in ["Which apps are playing audio right now?", "What's playing audio?", "Welche Apps spielen gerade Ton?"] {
            assert_eq!(tool(text), Some(("control_audio_mixer", json!({"action": "list"}))), "{text}");
        }
        // The block with the faders, not a list read out.
        assert_eq!(tool("Show me the volume mixer"), Some(("configure_interface", json!({"mixer_view": true}))));
        // Small talk stays small talk.
        assert_eq!(parse("What's up?"), None);
        assert_eq!(tool("What's up on the board?"), Some(("whats_open", json!({}))));
        assert_eq!(tool("Make the accents gold"), Some(("configure_interface", json!({"accent_color": "gold"}))));
        assert_eq!(tool("Find my jai config file").unwrap().1["query"], "jai config");
        assert_eq!(tool("Find my tax pdf").unwrap().1["query"], "tax pdf");
        // "What's playing" is the song, not the mixer.
        assert_eq!(tool("What's playing?").map(|(n, _)| n), Some("control_music"));
        // A size alone resizes the orb that is shown; a place means the floating orb.
        assert_eq!(tool("Make the orb 300 pixels"), Some(("control_orb_mode", json!({"orb_size_px": 300.0}))));
    }

    #[test]
    fn jai_views_open_as_windows_never_as_programs() {
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
        assert_eq!(tool("Open message from stats please.").map(|(n, _)| n), Some("open_jai_view"));
        assert_eq!(strip_article("discord for me"), "discord");
        assert_eq!(strip_article("chrome on my pc"), "chrome");
    }

    /// "Open three random windows" is his own choice of views, never a program of that name (the
    /// launcher looked for "Three Random Windows", 2026-11-19).
    #[test]
    fn random_windows_are_his_choice() {
        for (text, count) in [
            ("JAI, can you open three random windows?", Some(3)),
            ("open a few random windows", None),
            ("Open some random windows for me, please.", None),
            ("open 2 random views", Some(2)),
            ("show me a couple of windows of your choice", None),
            ("open a few windows", None),
            ("open random windows", None),
            ("öffne ein paar zufällige Fenster", None),
            ("Öffne drei zufällige Fenster", Some(3)),
        ] {
            let (name, args) = crate::phrasings::route(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!((name.as_str(), args["view"].as_str()), ("open_jai_view", Some("random")), "{text}");
            assert_eq!(args["count"].as_u64(), count, "{text}");
        }
        // "… in 3D", the whole road as the brain takes it: a thing is the 3D studio's, a town's
        // houses are the Earth's.
        for text in ["Show me the houses of Musterstadt in 3D", "show me the houses of Berlin in 3D", "Zeig mir die Häuser von Musterstadt in 3D"] {
            let (name, args) = crate::phrasings::route(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!((name.as_str(), args["action"].as_str()), ("open_jai_view", Some("show_buildings")), "{text}");
            assert!(args["place"].as_str().is_some_and(|p| !p.is_empty()), "{text}: {args}");
        }
        assert_eq!(crate::phrasings::route("Render me an Iron Man suit in 3D").map(|r| r.0).as_deref(), Some("show_3d_model"));
        assert_eq!(crate::phrasings::route("Show me a house in 3D").map(|r| r.0).as_deref(), Some("show_3d_model"));
        // A named thing stays what it is.
        for text in ["open two chrome windows", "open a new window", "open the windows settings", "open two windows"] {
            assert_ne!(internal_view_command(text).map(|a| a["view"].clone()), Some(json!("random")), "{text}");
        }
    }

    /// "… in a new window" after a note: a window of its own, and not words of the note.
    #[test]
    fn a_note_in_a_new_window_gets_one() {
        let (name, args) = tool("Add a note saying buy milk in a new window").expect("a note");
        assert_eq!(name, "spawn_native_window");
        assert_eq!((args["markdown"].as_str(), args["title"].as_str()), (Some("buy milk"), Some("Note: buy milk")));
        assert!(args["id"].as_str().is_some_and(|id| id.starts_with("note_")), "{args}");
        let (name, args) = tool("Add a note saying buy milk").expect("a note");
        assert_eq!((name, args["markdown"].as_str()), ("create_or_update_widget", Some("buy milk")));
        assert!(NOTE_WINDOW_TAIL.is_match("milch kaufen in einem neuen fenster"));
        assert!(!NOTE_WINDOW_TAIL.is_match("look out of the window"));
    }

    /// Sir's words with the Earth up: a layer off, back on, all off; never the orb (the brain
    /// once turned the orb off for "I don't want to see the satellite any more").
    /// Sir, 2026-11-22: stopped mid-sentence, "continue" or the like has JAI go on with what he
    /// was saying; with nothing cut off, "continue" and "weiter" are the music's again.
    #[test]
    fn continue_goes_on_with_what_was_cut_off() {
        for text in ["Continue", "continue.", "Go on", "Okay, continue please", "Sorry, go ahead", "Keep going", "You can continue", "Jai, carry on", "Weiter", "Mach weiter", "Ja, sprich weiter", "Fahr fort", "Where were we?", "finish your sentence"] {
            assert!(resumes(text, true), "{text}");
            assert!(!resumes(text, false), "{text}: nothing was cut off");
        }
        for text in ["Continue the music", "Go on to the next slide", "Weiter zum nächsten Lied", "Continue with the timer", "next"] {
            assert!(!resumes(text, true), "{text}");
        }
        assert_eq!(silent_quick_command("continue").map(|a| a.tool), Some("control_music"), "nothing cut off: the music");
    }

    /// Sir, 2026-11-22: "track a flight that goes from Frankfurt to New York" finds one in the air
    /// and follows it; "show me the capital of France" shows Paris (the model names a described
    /// place), with or without the Earth up, and other "show me"s stay theirs.
    #[test]
    fn flights_are_tracked_and_described_places_found_by_voice() {
        let (none, earth_up) = (PanelsUp::default(), PanelsUp { earth: true, ..PanelsUp::default() });
        for text in [
            "Track a flight that goes from Frankfurt to New York",
            "track a flight from frankfurt to new york",
            "Follow a plane flying from Frankfurt to New York right now",
            "Find me a flight from Frankfurt to New York",
            "Verfolge einen Flug von Frankfurt nach New York",
        ] {
            let args = panel_command(text, none).unwrap_or_else(|| panic!("{text}"));
            assert_eq!((args["action"].as_str(), args["from"].as_str(), args["to"].as_str()), (Some("track_flight"), Some("Frankfurt"), Some("New York")), "{text}");
        }
        for (text, flight) in [("Track flight LH 400", "lh 400"), ("follow DLH400", "dlh400")] {
            assert_eq!(panel_command(text, none).unwrap()["flight"], flight, "{text}");
        }
        assert_eq!(panel_command("stop tracking", earth_up), None, "nothing is followed");
        for (text, place) in [
            ("Show me the capital of France", "capital of France"),
            ("Where is the tallest building in the world?", "tallest building in the world"),
            ("Zeig mir die Hauptstadt von Spanien", "Hauptstadt von Spanien"),
            ("Show me the largest city in Germany on the globe", "largest city in Germany"),
        ] {
            for up in [none, earth_up] {
                let args = panel_command(text, up).unwrap_or_else(|| panic!("{text}"));
                assert_eq!((args["action"].as_str(), args["place"].as_str()), (Some("show_place"), Some(place)), "{text}");
            }
        }
        for text in ["Show me the largest file", "Show me my most played songs", "Where is the capital letter key", "Show me the biggest folders on my drive"] {
            assert_ne!(panel_command(text, none).and_then(|a| a["action"].as_str().map(str::to_string)).as_deref(), Some("show_place"), "{text}");
        }
    }

    #[test]
    fn earth_layers_by_voice() {
        let earth_up = PanelsUp { earth: true, ..PanelsUp::default() };
        let args = |text: &str| panel_command(text, earth_up);
        let hide = |layer: &str| Some(json!({ "view": "earth", "action": "hide_layer", "layer": layer }));
        assert_eq!(args("I don't want to see the satellite any more than others."), hide("satellites"));
        assert_eq!(args("I don't want to see the satellites anymore"), hide("satellites"));
        assert_eq!(args("turn off the trains"), hide("trains"));
        assert_eq!(args("hide the traffic"), hide("traffic"));
        assert_eq!(args("keine Satelliten mehr"), hide("satellites"));
        assert_eq!(args("turn the trains back on"), Some(json!({ "view": "earth", "action": "show_trains" })));
        assert_eq!(args("show the satellites again"), Some(json!({ "view": "earth", "action": "show_satellites", "group": "active" })));
        assert_eq!(args("hide everything"), Some(json!({ "view": "earth", "action": "hide_layers" })));
        // Requests of their own stay theirs.
        assert_ne!(args("show the flights over New York"), Some(json!({ "view": "earth", "action": "show_flights" })));
    }

    #[test]
    fn panels_from_the_reference_videos() {
        let none = PanelsUp::default();
        let earth_up = PanelsUp { earth: true, ..PanelsUp::default() };
        let radar_up = PanelsUp { radar: true, ..PanelsUp::default() };
        let disk_up = PanelsUp { disk: true, ..PanelsUp::default() };
        let act = |text: &str, up: PanelsUp| panel_command(text, up).map(|a| (a["view"].as_str().unwrap_or_default().to_string(), a["action"].as_str().unwrap_or_default().to_string()));
        let is = |text: &str, up: PanelsUp, view: &str, action: &str| assert_eq!(act(text, up), Some((view.to_string(), action.to_string())), "{text}");
        // Video 1: the network radar.
        for text in [
            "Hey JAI, check check the network.",
            "Check the network",
            "scan my network",
            "Scan my home network for devices",
            "Who's on my wifi?",
            "What devices are connected to my network?",
            "How many devices are on my network?",
            "Open the network radar",
            "Show me the devices on my network",
            "Scanne mein Netzwerk",
            "Netzwerk prüfen",
            "Wer ist in meinem WLAN?",
        ] {
            is(text, none, "network_radar", "show");
        }
        is("Is my network secure?", none, "network_radar", "check_security");
        is("Is there any security problem?", radar_up, "network_radar", "check_security");
        assert_eq!(act("Is there any security problem?", none), None, "no radar up: about something else");
        is("Rescan", radar_up, "network_radar", "rescan");
        is("scan again", disk_up, "disk_map", "rescan");
        // Video 3: the disk map.
        for text in [
            "What is taking up space?",
            "What's taking up all my disk space on my computer?",
            "Where did all my space go?",
            "Why is my C drive so full?",
            "Show me the disk map",
            "How can I free up some space?",
            "Was frisst meinen Speicherplatz?",
        ] {
            is(text, none, "disk_map", "show");
        }
        assert_eq!(panel_command("Map my D drive", none).unwrap()["path"], "D:\\");
        assert_eq!(panel_command("What's taking up space on my E drive?", none).unwrap()["path"], "E:\\");
        // Video 4: the Earth.
        for text in ["Show me the earth", "Show me the globe.", "Zeig mir die Erde", "Open the globe"] {
            is(text, none, "earth", "show");
        }
        is("Switch to the satellite view.", none, "earth", "show_satellite");
        is("Switch to satellite view", earth_up, "earth", "show_satellite");
        is("Switch back to the holographic view", earth_up, "earth", "show_holo");
        let quakes = panel_command("And also JAI, show the earthquakes that happened in this area", earth_up).unwrap();
        assert_eq!((quakes["action"].as_str(), quakes["place"].as_str()), (Some("show_quakes"), Some("here")));
        let quakes = panel_command("Were there any earthquakes near Tokyo recently?", none).unwrap();
        assert_eq!((quakes["action"].as_str(), quakes["place"].as_str()), (Some("show_quakes"), Some("Tokyo")));
        let quakes = panel_command("show me earthquakes within 300 km of Naples", none).unwrap();
        assert_eq!((quakes["place"].as_str(), quakes["radius_km"].as_f64()), (Some("Naples"), Some(300.0)));
        assert_eq!(panel_command("any earthquakes within 200 km", none).unwrap()["radius_km"], 200.0);
        let fly = panel_command("Fly to New York", earth_up).unwrap();
        assert_eq!((fly["action"].as_str(), fly["place"].as_str()), (Some("show_place"), Some("New York")));
        assert_eq!(panel_command("fly to new york", earth_up).unwrap()["place"], "New York");
        assert_eq!(panel_command("Show me Paris on the globe", none).unwrap()["place"], "Paris");
        let flights = panel_command("Show me the flights over New York", none).unwrap();
        assert_eq!((flights["action"].as_str(), flights["place"].as_str()), (Some("show_flights"), Some("New York")));
        is("Where is the ISS?", none, "earth", "show_iss");
        is("Show me the storms", none, "earth", "show_storms");
        is("zoom in", earth_up, "earth", "zoom_in");
        for text in ["Go up a folder", "up one level", "go back up", "one folder up", "back out", "go to the parent folder", "Einen Ordner hoch"] {
            is(text, disk_up, "disk_map", "back");
        }
        assert_eq!(panel_command("hide the flights", earth_up).unwrap()["layer"], "flights");
        assert_eq!(panel_command("Show me the earth in a new window", none).unwrap()["view"], "earth");
        // Follow-ups only while their panel is up, and nobody else's words.
        for (text, up) in [
            ("zoom in", none),
            ("go to page 2", earth_up),
            ("go to sleep", earth_up),
            ("fly to Tokyo", none),
            ("switch back", none),
            ("open Discord", earth_up),
            ("What is using my disk?", none),
            ("What is using all my memory?", none),
            ("Analyse my desktop", none),
            ("Tell me about the earthquake in Turkey", none),
            ("earthquake safety tips", none),
            ("show me flights to Berlin", none),
            ("Check my internet connection", none),
            ("How much space do I have left?", none),
            ("Diagnose my network", none),
            ("make the globe bigger", none),
            ("Rescan", none),
            ("What is an earthquake?", none),
            ("go up a folder", none),
            ("go back", disk_up),
            ("back up my documents folder", disk_up),
            ("turn it up", disk_up),
        ] {
            assert_eq!(panel_command(text, up), None, "{text}");
        }
        // Through the parser and the view command alike; a question only looks.
        assert_eq!(tool("Check the network").map(|(n, _)| n), Some("open_jai_view"));
        assert_eq!(internal_view_command("What is taking up space?").unwrap()["view"], "disk_map");
        let asked = panel_command("Who is on my network?", none).unwrap();
        assert!(!beyond_the_question("Who is on my network?", "open_jai_view", &asked));
    }

    #[test]
    fn the_earth_locates_shades_rains_remembers_and_tours() {
        let none = PanelsUp::default();
        let earth_up = PanelsUp { earth: true, ..PanelsUp::default() };
        let both = PanelsUp { earth: true, radar: true, ..PanelsUp::default() };
        let act = |text: &str, up: PanelsUp| panel_command(text, up).filter(|a| a["view"] == "earth").map(|a| a["action"].as_str().unwrap_or_default().to_string());
        let is = |text: &str, up: PanelsUp, action: &str| assert_eq!(act(text, up).as_deref(), Some(action), "{text}");
        // Where Sir is: a bare "where am I?" is the Earth's only while it is up.
        for text in ["Locate me", "Show me where I am", "Where am I on the map?", "Show my location on the globe", "Zeig mir, wo ich bin", "Finde mich", "Wo bin ich auf der Karte?"] {
            is(text, none, "show_my_location");
        }
        for text in ["Where am I?", "Where am I right now?", "Wo bin ich?", "What's my location?"] {
            is(text, earth_up, "show_my_location");
            assert_eq!(act(text, none), None, "{text}: the location engine's address");
        }
        // The terrain.
        for (text, action) in [("Show me the mountains", "show_mountains"), ("Show the terrain", "show_terrain"), ("Zeig mir die Berge", "show_mountains"), ("Show me the relief", "show_terrain")] {
            is(text, none, action);
        }
        let nepal = panel_command("Show me the mountains of Nepal", none).unwrap();
        assert_eq!((nepal["action"].as_str(), nepal["place"].as_str()), (Some("show_mountains"), Some("Nepal")));
        assert_eq!(panel_command("Zeig mir die Berge in der Schweiz", none).unwrap()["place"], "Schweiz");
        assert!(panel_command("Show me the mountains on the globe", none).unwrap().get("place").is_none());
        for text in ["Exaggerate the terrain", "Make the mountains taller", "Überhöhe das Gelände"] {
            is(text, none, "exaggerate_terrain");
        }
        // The houses in 3D.
        for text in ["Show me the buildings", "Show me the houses in 3D", "Show the 3D buildings", "Zeig mir die Häuser in 3D", "Zeige mir die Gebäude"] {
            is(text, none, "show_buildings");
        }
        let town = panel_command("Show me the houses of Musterstadt in 3D", none).unwrap();
        assert_eq!((town["action"].as_str(), town["place"].as_str()), (Some("show_buildings"), Some("Musterstadt")));
        assert_eq!(panel_command("Zeig mir die Häuser in Musterstadt", none).unwrap()["place"], "Musterstadt");
        for text in ["Hide the buildings", "Turn off the 3D buildings", "Blende die Häuser aus"] {
            let args = panel_command(text, earth_up).unwrap_or_else(|| panic!("{text}"));
            assert_eq!((args["action"].as_str(), args["layer"].as_str()), (Some("hide_layer"), Some("buildings")), "{text}");
        }
        is("flatten the terrain", earth_up, "flatten_terrain");
        // Its camera in 3D and back to 2D, while it is up; the houses' own orders stay theirs.
        for text in ["Switch to 3D", "3D view", "Show it in 3D", "Go 3D", "Tilt the map", "Switch the map to 3-D", "Show me the city in 3D", "In 3D, please", "Schalte auf 3D", "Wechsle in die 3D-Ansicht", "Kippe die Karte", "Zeig mir das in 3D"] {
            is(text, earth_up, "show_3d");
            assert_eq!(act(text, none), None, "{text}: the Earth's only while it is up");
        }
        for text in ["Switch back to 2D", "Back to 2D", "2D", "Top-down view", "Back to the flat map", "Look straight down", "Make it flat again", "Untilt the camera", "Schalte zurück auf 2D", "2D-Ansicht", "Draufsicht"] {
            is(text, earth_up, "show_2d");
        }
        for text in ["Show me the houses in 3D", "Turn off the 3D buildings", "Make the mountains taller"] {
            assert!(!matches!(act(text, earth_up).as_deref(), Some("show_3d" | "show_2d")), "{text}");
        }
        // The weather.
        for text in ["Show me the weather radar", "Show the rain radar", "Where is it raining?", "Zeig mir das Regenradar", "rain radar"] {
            is(text, none, "show_radar");
        }
        for text in ["Animate the radar", "Show me the rain over the past two hours", "Play the rain radar", "Spiel das Regenradar ab", "show the rain radar loop"] {
            is(text, none, "animate_radar");
        }
        for text in ["Show me the clouds", "Show the live cloud cover", "Zeig mir die Wolken"] {
            is(text, none, "show_clouds");
        }
        is("clouds", earth_up, "show_clouds");
        assert_eq!(act("clouds", none), None);
        let march = panel_command("Show me the clouds on 1 March 2024", none).unwrap();
        assert_eq!((march["action"].as_str(), march["date"].as_str()), (Some("show_clouds"), Some("1 march 2024")));
        assert_eq!(panel_command("Zeig mir die Wolken von gestern", none).unwrap()["date"], "gestern");
        assert!(panel_command("Show me the clouds today", none).unwrap().get("date").is_none(), "today is live");
        // A past day.
        let europe = panel_command("What did Europe look like yesterday?", none).unwrap();
        assert_eq!((europe["action"].as_str(), europe["place"].as_str(), europe["date"].as_str()), (Some("show_date"), Some("Europe"), Some("yesterday")));
        let world = panel_command("Show me the Earth on 2024-03-01", none).unwrap();
        assert_eq!((world["action"].as_str(), world["date"].as_str()), (Some("show_date"), Some("2024-03-01")));
        assert!(world.get("place").is_none());
        assert_eq!(panel_command("Wie sah Europa gestern aus?", none).unwrap()["place"], "Europa");
        assert_eq!(panel_command("Show me Germany from last week", earth_up).unwrap()["place"], "Germany");
        // Imagery, labels, the overlays off, back to today.
        let sentinel = panel_command("Switch to Sentinel imagery", earth_up).unwrap();
        assert_eq!((sentinel["action"].as_str(), sentinel["imagery"].as_str()), (Some("set_imagery"), Some("sentinel")));
        assert_eq!(panel_command("use the esri imagery", earth_up).unwrap()["imagery"], "esri");
        is("Show the country borders", none, "show_labels");
        is("Hide the labels", earth_up, "hide_labels");
        is("Blende die Grenzen aus", earth_up, "hide_labels");
        for (text, layer) in [("Hide the clouds", "clouds"), ("turn off the rain radar", "radar"), ("remove the terrain", "terrain"), ("Blende die Wolken aus", "clouds")] {
            assert_eq!(panel_command(text, earth_up).unwrap()["layer"], layer, "{text}");
        }
        assert_eq!(act("hide the radar", both), None, "the network radar's");
        // Sir's own words of 2026-11-09, which went to the model.
        is("Can you make the globe spin?", earth_up, "spin");
        for text in [
            "Can you spin it in the other direction?",
            "Can you spin the Earth in the other direction?",
            "Spin it the other way round",
            "Reverse the spin",
            "rotate the globe backwards",
            "Turn it the opposite way",
            "Now the other direction",
            "Dreh die Erde andersherum",
        ] {
            is(text, earth_up, "spin_reverse");
        }
        assert_eq!(panel_command("Spin it in the other direction", none), None, "no globe up: not the Earth's");
        is("Can you show me where I live?", none, "show_my_location");
        for (text, place) in [("Can you show me where New York is?", "New York"), ("Can you show me the USA?", "USA"), ("Where is Tokyo?", "Tokyo"), ("Zeig mir, wo Paris liegt", "Paris")] {
            let fly = panel_command(text, earth_up).unwrap();
            assert_eq!((fly["action"].as_str(), fly["place"].as_str()), (Some("show_place"), Some(place)), "{text}");
        }
        for text in ["Show me a picture of the moon", "Show me the weather", "Show me my files", "Show me wireless", "Show me the satellites"] {
            assert_ne!(act(text, earth_up).as_deref(), Some("show_place"), "{text}");
        }
        // Landmarks and buildings by name, in English and in German (Sir's wish of 2026-11-19).
        for (text, place) in [
            ("Show me the Statue of Liberty", "Statue of Liberty"),
            ("Show me the Empire State Building", "Empire State Building"),
            ("Zoom to the Eiffel Tower", "Eiffel Tower"),
            ("Fly me to the Golden Gate Bridge", "Golden Gate Bridge"),
            ("Take me to Times Square", "Times Square"),
            ("Zeig mir die Freiheitsstatue", "Freiheitsstatue"),
        ] {
            let fly = panel_command(text, earth_up).unwrap_or_else(|| panic!("{text}"));
            assert_eq!((fly["action"].as_str(), fly["place"].as_str()), (Some("show_place"), Some(place)), "{text}");
        }
        assert_eq!(panel_command("Can you show me wireless?", earth_up).unwrap()["view"], "network_radar");
        is("back to today", earth_up, "show_today");
        // The tour.
        for text in ["What's happening in the world?", "Give me a world tour", "Start the world tour", "Was passiert in der Welt?", "Mach eine Welttour"] {
            is(text, none, "show_world_tour");
        }
        for text in ["Stop the tour", "End the world tour", "Tour beenden"] {
            is(text, earth_up, "stop_tour");
            assert_eq!(act(text, none), None, "{text}: no Earth, no tour");
        }
        // Other people's words stay theirs.
        for (text, up) in [
            ("Show me my emails from yesterday", earth_up),
            ("Show me the flights from yesterday", earth_up),
            ("Show me the photos from last week", none),
            ("What did you look like yesterday?", none),
            ("show me", none),
            ("find me a restaurant", none),
            ("show me the labels", none),
            ("Tell me about the mountains", none),
            ("switch to nasa", none),
            ("hide the clouds", none),
        ] {
            assert_eq!(act(text, up), None, "{text}");
        }
        // A question only looks: these pass the question guard.
        for text in ["Where am I on the map?", "What's happening in the world?", "What did Europe look like yesterday?", "Where is it raining?"] {
            let args = panel_command(text, none).unwrap();
            assert!(!beyond_the_question(text, "open_jai_view", &args), "{text}");
        }
        // The whole fast path reaches them (the verifier's probes).
        for text in ["Where am I on the map?", "Show me the mountains.", "Show me the rain radar.", "Show me the clouds on 1 March 2024.", "What's happening in the world?", "Zeig mir, wo ich bin.", "locate me", "give me a world tour"] {
            assert_eq!(tool(text).map(|(n, _)| n), Some("open_jai_view"), "{text}");
        }
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
        volume_scenarios();
        for (text, action) in [("paus", "stop_all"), ("weiter", "resume"), ("skip", "next_track"), ("zurück", "previous_track")] {
            assert_eq!(silent_quick_command(text).map(|a| a.args["action"].clone()), Some(json!(action)), "{text}");
        }
        for (text, action) in [("silence", "mute_master"), ("stumm", "mute_master"), ("ton an", "unmute_master"), ("lauter", "change_master"), ("leiser", "change_master")] {
            assert_eq!(silent_quick_command(text).map(|a| a.args["action"].clone()), Some(json!(action)), "{text}");
        }
    }

    /// He starts with an empty HUD; what was open comes back when asked, and a widget stays for
    /// the next start only when Sir says so.
    #[test]
    fn recent_windows_come_back_and_widgets_are_kept_by_voice() {
        let back = Some(Intent::Tool { name: "spawn_native_window", args: json!({"restore": "recent"}) });
        for text in [
            "Bring back my windows",
            "Bring my windows back",
            "JAI, bring back up all recently opened windows.",
            "Reopen my windows",
            "Restore my last session",
            "Reopen the windows from last time",
            "Open everything I had open again",
            "Bring back the widgets",
            "Hol meine Fenster zurück",
            "Öffne meine letzten Fenster wieder",
        ] {
            assert_eq!(parse(text), back, "{text}");
        }
        let keep = |id: &str, on: bool| Some(Intent::Tool { name: "create_or_update_widget", args: json!({"id": id, "keep_at_start": on}) });
        for (text, id, on) in [
            ("Open that widget again the next time I start you", "last", true),
            ("Keep this widget for the next start", "last", true),
            ("Show the CPU gauge again when you start", "CPU load", true),
            ("Keep that window open at startup", "window", true),
            ("Don't open it at startup anymore", "last", false),
            ("Öffne das Widget beim nächsten Start wieder", "last", true),
        ] {
            assert_eq!(parse(text), keep(id, on), "{text}");
        }
        // What the silent session of 2026-11-06 found: the Feature Lab was taken for a program,
        // "remove all widgets" hid the panel that holds them, a status question and a plain
        // gauge went to the language model.
        let iface = |args: Value| Some(Intent::Tool { name: "configure_interface", args });
        for text in ["Open the feature lab", "Show me the feature lab", "Feature lab", "JAI, open your feature lab."] {
            assert_eq!(parse(text), iface(json!({"feature_lab": true})), "{text}");
        }
        let remove = |id: &str| Some(Intent::Tool { name: "remove_widget", args: json!({"id": id}) });
        for (text, id) in [("Remove all widgets", "all"), ("Delete the widgets", "all"), ("Clear my widgets", "all"), ("Remove the CPU load widget", "cpu load"), ("Close that widget", "last"), ("Entferne alle Widgets", "all")] {
            assert_eq!(parse(text), remove(id), "{text}");
        }
        assert_ne!(parse("Hide the widgets panel"), remove("all"), "the panel is the layout's");
        let gauge = |title: &str, source: &str, unit: &str| Some(Intent::Tool { name: "create_or_update_widget", args: json!({"type": "MetricGauge", "title": title, "source": source, "unit": unit}) });
        assert_eq!(parse("Create a widget that shows my CPU load"), gauge("CPU load", "cpu", "%"));
        assert_eq!(parse("Add a GPU temperature gauge"), gauge("GPU temperature", "gpu_temp", "°C"));
        assert_eq!(parse("Make me a RAM widget"), gauge("RAM", "ram", "%"));
        assert_eq!(parse("Erstelle ein Widget für die CPU-Auslastung"), gauge("CPU load", "cpu", "%"));
        for text in ["What's my system status?", "How is my PC doing?", "System status", "PC status", "Give me a system report"] {
            assert_eq!(parse(text), Some(Intent::Tool { name: "get_system_status", args: json!({}) }), "{text}");
        }
        // One reading asked for is the one read out.
        for (text, reading) in [("Show me my RAM usage", "memory"), ("How much memory am I using?", "memory"), ("What's my CPU at?", "cpu"), ("How hot is my GPU?", "gpu"), ("How much VRAM is free?", "vram"), ("Wie heiß ist die Grafikkarte?", "gpu"), ("Wie viel Arbeitsspeicher nutze ich?", "memory"), ("Wie hoch ist die CPU-Auslastung?", "cpu"), ("Wie hoch ist die RAM-Auslastung?", "memory"), ("How hot is my PC?", "all")] {
            assert_eq!(parse(text), Some(Intent::Tool { name: "get_system_status", args: json!({"reading": reading}) }), "{text}");
        }
    }

    #[test]
    fn quiet_is_a_level_to_go_down_to() {
        for (text, level) in [("Make it quiet", 20), ("Make it very quiet", 10), ("Very quiet", 10), ("Make everything quiet", 20), ("Mach es leise", 20), ("Mach es ganz leise", 10), ("Ganz leise", 10)] {
            assert_eq!(mixer_command(text), Some(json!({"action": "quiet", "level": level})), "{text}");
        }
        // How it sounds is one step the other way, about what was just changed.
        for (text, delta) in [("It's very quiet", 10), ("It's quiet", 10), ("It's so loud", -10), ("That is really loud", -10), ("Das ist laut", -10), ("Es ist sehr leise", 10)] {
            assert_eq!(mixer_command(text), Some(json!({"action": "change_app", "app": "it", "delta": delta})), "{text}");
        }
        for (text, level) in [("Make it loud", 80), ("Make it very loud", 100), ("Mach es laut", 80), ("Mach es richtig laut", 100)] {
            assert_eq!(mixer_command(text), Some(json!({"action": "loud", "level": level})), "{text}");
        }
        assert_eq!(silent_quick_command("Keep it down").unwrap().args, json!({"action": "change_app", "app": "it", "delta": -10}));
        assert_eq!(bare_comparative("More"), Some("More"));
        assert_eq!(bare_comparative("Weniger bitte"), Some("Less"));
        assert_eq!(bare_comparative("More volume"), None);
    }

    #[test]
    fn courtesies_are_answered_without_the_model() {
        for text in ["Thank you", "Thanks", "Thanks, JAI.", "Thank you very much", "Thank you, JAI, that was perfect.", "Perfect, thanks", "Danke", "Vielen Dank", "Danke schön"] {
            assert!(matches!(parse(text), Some(Intent::Answer(line)) if line.ends_with("Sir.")), "{text}");
        }
        for text in ["JAI", "Hey JAI", "JAI!"] {
            assert_eq!(parse(text), Some(Intent::Answer("Yes, Sir?".into())), "{text}");
        }
        // Thanks with a request, or a declined offer, are no courtesy.
        for text in ["No thanks", "Thanks for the update, now open Spotify", "Thank you for watching my stream today"] {
            assert!(!matches!(parse(text), Some(Intent::Answer(_))), "{text}");
        }
    }

    /// Repro (route reader, 2026-11-07): "is the sound on?" unmuted, "I can barely hear Discord"
    /// joined the Discord call, "switch to my speakers" looked for a window.
    #[test]
    fn a_question_about_the_sound_moves_nothing_and_every_sentence_lands_on_the_mixer() {
        let mixer = |args: Value| Some(Intent::Tool { name: "control_audio_mixer", args });
        for text in ["Is the sound on?", "Is the sound off?", "Is my sound muted?", "Ist der Ton an?", "Ist mein Ton stumm?"] {
            assert_eq!(parse(text), mixer(json!({"action": "muted"})), "{text}");
        }
        for text in ["Is it too loud?", "Why is it so loud?", "Was ist lauter?"] {
            assert!(volume_phrase(&text.to_lowercase().replace('?', "")).is_none(), "{text} is no request");
        }
        for (text, app, delta) in [
            ("Not loud enough", "it", 10),
            ("It's not loud enough", "it", 10),
            ("Das ist nicht laut genug", "it", 10),
            ("Not quiet enough", "it", -10),
            ("Discord isn't loud enough", "discord", 10),
            ("I can barely hear it", "it", 10),
            ("I can hardly hear Discord", "discord", 10),
            ("Ich höre Discord kaum", "discord", 10),
            ("Ich kann es kaum hören", "it", 10),
        ] {
            assert_eq!(parse(text), mixer(json!({"action": "change_app", "app": app, "delta": delta})), "{text}");
        }
        for (text, app, seconds) in [("Mute Discord for 10 minutes", "discord", 600), ("Mute Firefox for an hour", "firefox", 3600), ("Schalte Discord für 5 Minuten stumm", "discord", 300), ("Discord für eine halbe Stunde stumm", "discord", 1800)] {
            assert_eq!(parse(text), mixer(json!({"action": "mute_app", "app": app, "seconds": seconds})), "{text}");
        }
        for text in ["Mute for 10 minutes", "Mute everything for 10 minutes", "Mute the sound for 10 minutes"] {
            assert_eq!(parse(text), mixer(json!({"action": "mute_master", "seconds": 600})), "{text}");
        }
        assert_eq!(parse("Mute Discord until I say so"), mixer(json!({"action": "mute_app", "app": "discord"})));
        assert_eq!(parse("Firefox stumm schalten"), mixer(json!({"action": "mute_app", "app": "firefox"})));
        for text in ["Mach alles stumm außer Discord", "Alles stumm außer Discord", "Schalte alles aus außer Discord"] {
            assert_eq!(parse(text), mixer(json!({"action": "mute_others", "app": "discord"})), "{text}");
        }
        for (text, args) in [
            ("Fade out the music", json!({"action": "fade", "way": "out", "app": "music"})),
            ("Fade the music out", json!({"action": "fade", "way": "out", "app": "music"})),
            ("Fade in the music", json!({"action": "fade", "way": "in", "app": "music"})),
            ("Fade Discord back in", json!({"action": "fade", "way": "in", "app": "discord"})),
            ("Blende die Musik aus", json!({"action": "fade", "way": "out", "app": "musik"})),
            ("Fade out", json!({"action": "fade", "way": "out", "app": "it"})),
            ("Fade everything out", json!({"action": "fade", "way": "out"})),
            ("Turn it down slowly", json!({"action": "fade", "delta": -20, "app": "it"})),
            ("Slowly turn the music up", json!({"action": "fade", "delta": 20, "app": "music"})),
            ("Dreh die Musik langsam runter", json!({"action": "fade", "delta": -20, "app": "musik"})),
        ] {
            assert_eq!(parse(text), mixer(args), "{text}");
        }
        for (text, app, to, offset) in [
            ("Set Firefox to the same level as Discord", "firefox", "discord", 0),
            ("Make Discord as loud as Firefox", "discord", "firefox", 0),
            ("Mach Firefox so laut wie Discord", "firefox", "discord", 0),
            ("Make Firefox quieter than Discord", "firefox", "discord", -10),
            ("Make Discord louder than the music", "discord", "music", 10),
            ("Mach Firefox leiser als Discord", "firefox", "discord", -10),
        ] {
            assert_eq!(parse(text), mixer(json!({"action": "match_app", "app": app, "to": to, "offset": offset})), "{text}");
        }
        assert_eq!(parse("Set all apps to 100 percent"), mixer(json!({"action": "set_all_apps", "level": 100})));
        assert_eq!(parse("Set everything to 100"), mixer(json!({"action": "set_master", "level": 100})));
        for text in ["What's my output device?", "Which audio device am I using?", "Where is the sound going?", "Welches Audiogerät nutze ich?"] {
            assert_eq!(parse(text), mixer(json!({"action": "device"})), "{text}");
        }
        assert_eq!(parse("List my audio devices"), mixer(json!({"action": "devices"})));
        for text in ["Switch to my speakers", "Switch the sound to my headset", "Use my headset", "Change the output device", "Schalte auf die Kopfhörer um"] {
            assert_eq!(parse(text), mixer(json!({"action": "switch_device"})), "{text}");
        }
        assert_eq!(parse("What's Discord at?"), mixer(json!({"action": "get_app", "app": "discord"})));
        assert_eq!(parse("What is Firefox set to?"), mixer(json!({"action": "get_app", "app": "firefox"})));
        for text in ["Why is it so quiet?", "Why is it so loud?", "Warum ist es so leise?", "Why is everything so quiet?"] {
            assert_eq!(parse(text), mixer(json!({"action": "list"})), "{text}");
        }
        for text in ["Why is Discord muted?", "Why is Discord so quiet?", "Warum ist Discord stumm?"] {
            assert_eq!(parse(text), mixer(json!({"action": "get_app", "app": "discord"})), "{text}");
        }
        assert_eq!(parse("Is it muted?"), mixer(json!({"action": "muted"})));
        for text in ["Kannst du es leiser machen?", "Make it not so loud", "Mach den Ton nicht so laut"] {
            assert_eq!(parse(text), mixer(json!({"action": "change_app", "app": "it", "delta": -10})), "{text}");
        }
    }

    #[test]
    fn whether_he_is_listening_is_answered_from_the_lock() {
        for text in ["Is my mic muted?", "Is the microphone on?", "Am I muted?", "Are you listening?", "JAI, are you listening to me?", "Can you hear me?", "Ist mein Mikro stumm?", "Hörst du mir zu?"] {
            assert!(matches!(parse(text), Some(Intent::Answer(ref line)) if line.starts_with("I'")), "{text}");
        }
    }

    /// Repro (silent session, 2026-11-07): "Should I close Firefox?" had the model call
    /// close_application, and "Is the orb hidden?" control_orb_mode.
    #[test]
    fn a_plain_question_asks_and_an_order_in_good_manners_does_not() {
        for text in [
            "Should I close Firefox?",
            "Soll ich Firefox schließen?",
            "Is the orb hidden?",
            "JAI, is the orb hidden?",
            "Did you close Firefox?",
            "Why did you mute Discord?",
            "What happens if I close it?",
            "Do you know how to open Firefox?",
            "How do I close Firefox?",
            "Is it muted?",
            "Why is Firefox closed?",
            "Warum ist Firefox zu?",
            "Hast du Firefox geschlossen?",
            "When is my next class?",
            "What's the weather like?",
            "Is it possible to close Firefox?",
        ] {
            assert!(only_asks(text), "{text}");
        }
        for text in [
            "Can you close Firefox?",
            "Could you mute Discord?",
            "Would you open the mixer?",
            "Will you turn it down?",
            "Kannst du Firefox schließen?",
            "Close Firefox",
            "How about some music?",
            "What about Discord?",
            "Why don't you close it?",
            "Why not close it?",
            "Do you mind closing Firefox?",
            "Wie wäre es mit Musik?",
            "Is Firefox open? Close it.",
            "Is Firefox open and if so close it",
            "Is Firefox open, then close it",
            "When the timer ends, play music",
            "Do a backup",
            "Have a look at it",
            "Open the mixer",
            // Complaints: he wants the sound back.
            "Why is there no sound?",
            "Why can't I hear anything?",
            "Warum höre ich nichts?",
        ] {
            assert!(!only_asks(text), "{text}");
        }
        // "Are you there?" wakes the board, and a question may do that.
        assert!(!beyond_the_question("Hey JAI, are you there?", "hud_board", &json!({"action": "wake"})));
        assert!(beyond_the_question("Is the board open?", "hud_board", &json!({"action": "close"})));
        assert_eq!(tool("Why is there no sound?"), Some(("control_audio_mixer", json!({"action": "no_sound"}))));
    }

    #[test]
    fn his_own_record_and_his_modes_answer_for_themselves() {
        for (text, about) in [
            ("Did you mute Discord?", "discord"),
            ("Did you close Firefox?", "firefox"),
            ("Why did you close Firefox?", "firefox"),
            ("Did you close the mixer?", "mixer"),
            ("Hast du Firefox geschlossen?", "firefox"),
            ("Warum hast du Discord stumm geschaltet?", "discord"),
        ] {
            assert_eq!(tool(text), Some(("recent_actions", json!({ "about": about }))), "{text}");
        }
        for text in ["Have you changed the volume?", "Did you do that?", "Did you just close it?"] {
            assert_eq!(tool(text), Some(("recent_actions", json!({}))), "{text}");
        }
        for text in ["Is the orb hidden?", "Are you in orb mode?", "Is orb mode on?", "Ist der Orbmodus an?", "Bist du im Orbmodus?"] {
            assert_eq!(tool(text), Some(("control_orb_mode", json!({"action": "status"}))), "{text}");
        }
        // Small talk is nobody's record.
        assert_eq!(tool("Did you sleep well?"), None);
        assert_eq!(tool("Did you hear me?"), None);
    }

    /// Sir retired Ultron Mode (2026-11-09): asking for it gets one line, and no parser that runs
    /// before this one takes the sentence for a theme, a mode, an app or the model.
    #[test]
    fn ultron_mode_is_retired_in_one_line() {
        let ctx = crate::intent_engine::ReflexContext::default();
        for text in [
            "Switch to Ultron mode.",
            "JAI, switch to Ultron.",
            "Activate Ultron protocol",
            "Turn on Ultron mode, please",
            "Ultron mode on",
            "Are you in Ultron mode?",
            "What is Ultron mode?",
            "Ultron-Modus aktivieren",
            "Wechsel zu Ultron.",
            "Schalte in den Ultron-Modus",
            "Bist du im Ultron Modus?",
        ] {
            let said = crate::typos::normalize_user_input_typos(text);
            assert!(matches!(parse(&said), Some(Intent::Answer(ref line)) if line == "Ultron mode has been retired, Sir."), "{text}: {:?}", parse(&said));
            assert!(crate::intent_engine::route(&said, &ctx).is_none(), "{text}: the reflex router");
            assert!(crate::intent_engine::deterministic_action(&said).is_none(), "{text}: a window action");
            assert!(silent_quick_command(&said).is_none(), "{text}: a quick command");
            assert!(hud_view_command(&said).is_none() && outing_command(&said).is_none(), "{text}: a HUD view");
            assert!(crate::dynamic_popups::voice_command(&said).is_none(), "{text}: the look");
            assert!(orb_mode_command(&said).is_none() && listening_mode_command(&said).is_none() && mixer_command(&said).is_none(), "{text}: a mode");
            assert!(internal_view_command(&said).is_none() && ui_layout_command(&said).is_none(), "{text}: a view");
            assert!(crate::modality::parse_command(&said).is_none(), "{text}: the reply mode");
            assert!(crate::features::route_text(&said).is_none() && crate::integration_router::route_text(&said).is_none(), "{text}: a route");
            assert!(crate::compound_planner::plan(&said, &ctx).is_none(), "{text}: a compound");
        }
        // Ultron the character is still the model's to talk about.
        for text in ["Who is Ultron?", "Play the Age of Ultron soundtrack", "Is Ultron a villain?"] {
            assert!(!matches!(parse(text), Some(Intent::Answer(ref line)) if line.contains("retired")), "{text}");
        }
    }

    /// Repro (route reader, 2026-11-07): "nicht schließen" went to close_application.
    #[test]
    fn a_thing_he_is_told_not_to_do_stays_undone() {
        for text in [
            "Don't close it",
            "Do not close Firefox",
            "No, don't close it",
            "Don't open the mixer",
            "Don't mute Discord",
            "Don't turn it down",
            "Don't shut down the PC",
            "Please don't delete that",
            "Nicht schließen",
            "Schließ das nicht",
            "Mach das nicht zu",
            "Mach das nicht",
            "Nicht stumm schalten",
            "Stell das nicht leiser",
            "Schließ Firefox nicht",
            "Nein, nicht ausschalten",
            "Don't ever close Firefox",
            "Don't you dare close that",
        ] {
            assert!(matches!(parse(text), Some(Intent::Answer(ref line)) if line.contains("leave it")), "{text}: {:?}", parse(text));
        }
        // What he does want still counts.
        for text in ["Don't close the mixer, close the board instead", "Mach den Ton nicht so laut", "Don't listen", "Don't make me repeat myself", "Don't open it at startup anymore"] {
            assert!(!matches!(parse(text), Some(Intent::Answer(ref line)) if line.contains("leave it")), "{text}");
        }
        assert!(matches!(parse("Soll ich Firefox schließen?"), Some(Intent::Answer(ref line)) if line.contains("yours to decide")));
    }

    /// Repro (silent session, 2026-11-07): "How do I mute Discord?" got "I haven't done anything
    /// about that yet", and "Should I close Firefox?" "I'm not certain what you meant".
    #[test]
    fn how_to_and_whether_to_are_answered_with_the_order() {
        let answer = |text: &str| match parse(text) {
            Some(Intent::Answer(line)) => line,
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(answer("How do I mute Discord?"), "Just say \"Mute Discord\", Sir, and it's done.");
        assert_eq!(answer("How can I open the mixer?"), "Just say \"Open the mixer\", Sir, and it's done.");
        assert_eq!(answer("Should I close Firefox?"), "That's yours to decide, Sir. Say \"Close Firefox\" and it's done.");
        assert_eq!(answer("Should I turn the volume down?"), "That's yours to decide, Sir. Say \"Turn the volume down\" and it's done.");
        assert_eq!(answer("Wie öffne ich den Mixer?"), "Just say \"Öffne den Mixer\", Sir, and it's done.");
        assert_eq!(answer("Soll ich Firefox schließen?"), "That's yours to decide, Sir. Say \"Firefox schließen\" and it's done.");
        // Repro (silent session r1, 2026-11-07): the model answered "I'm not certain what
        // closing Firefox has to do with your query".
        assert_eq!(answer("What happens if I close Firefox?"), "Firefox closes, Sir, and anything unsaved in it is lost. Say \"Close Firefox\" if you want that.");
        assert!(answer("What would happen if I mute Discord?").starts_with("Discord goes silent until you unmute it"));
        // His own modes are told as they are, never switched on by the question.
        for text in ["What is orb mode?", "What's pure orb mode?", "What does the mixer do?", "Was ist der Orbmodus?", "What is span mode?"] {
            assert!(!answer(text).is_empty(), "{text}");
        }
        assert!(answer("What is orb mode?").starts_with("In orb mode only my orb floats"));
        // Why he stopped listening: his state, never his record of actions. (Other tests move
        // the lock meanwhile, so only the subject is pinned.)
        for text in ["Why did you stop listening?", "Why aren't you listening?", "Warum hörst du mir nicht mehr zu?"] {
            let line = answer(text);
            assert!(line.contains("listening") && !line.contains("changed anything"), "{text}: {line}");
        }
        // Nothing of his, or nothing he would do from those words: the model answers. ("Go to
        // sleep" is his own sleep, "go to bed" a window called "bed", "open a PDF" a guess.)
        // ("Should I shut down the PC?" is answered with its order since 2026-11-07: the power
        // dialogue's, see `his_logged_words_reach_the_right_tool`.)
        for text in ["How do I cook rice?", "Should I go to bed?", "Wie koche ich Reis?", "How do I go to sleep?", "How do I open a PDF?", "How do I close it?"] {
            assert!(!matches!(parse(text), Some(Intent::Answer(ref line)) if line.contains("Say") || line.contains("say")), "{text}");
        }
        // Repro (route reader, 2026-11-07): "Shut down the PC" was close_application "pc".
        for text in ["Shut down the PC", "Shut down my computer", "Close the PC", "Beende den Rechner"] {
            assert!(!matches!(parse(text), Some(Intent::Tool { name: "close_application", .. })), "{text}");
        }
    }

    #[test]
    fn what_only_the_machine_knows_is_looked_up() {
        let running = |app: &str| Some(("is_app_running", json!({ "app_name": app })));
        for (text, app) in [("Is Firefox open?", "firefox"), ("Is Discord running?", "discord"), ("Läuft Firefox?", "firefox"), ("Ist Firefox offen?", "firefox"), ("Ist Discord an?", "discord")] {
            assert_eq!(tool(text), running(app), "{text}");
        }
        for (text, app) in [("Is Minecraft running?", "minecraft"), ("Is Sonora open?", "sonora")] {
            assert_eq!(tool(text), running(app), "{text}");
        }
        for text in ["Is the music playing?", "Is music playing?", "Ist die Musik an?", "Is anything playing?"] {
            assert_eq!(tool(text), Some(("control_music", json!({"action": "what_is_playing"}))), "{text}");
        }
        for text in ["Is the mixer open?", "Is the board open?"] {
            assert_eq!(tool(text), Some(("whats_open", json!({}))), "{text}");
        }
        for text in ["Is a game running?", "Is the game running?", "Läuft ein Spiel?"] {
            assert_eq!(tool(text), Some(("control_audio_mixer", json!({"action": "get_app", "app": "game"}))), "{text}");
        }
        for text in ["How long is left on the timer?", "How much time is left?", "What's the timer at?", "What is the stopwatch at?", "How long until the timer goes off?", "Is my timer done?", "Is the timer running?", "Ist der Timer an?", "Wie lange noch?", "Wie viel Zeit ist noch übrig?"] {
            assert_eq!(tool(text), Some(("timer_left", json!({}))), "{text}");
        }
        for text in ["Is my PC on?", "Is it on?", "Are the lights on?", "Is that on?"] {
            assert_eq!(tool(text), None, "{text}");
        }
    }

    #[test]
    fn removing_it_never_reaches_a_program() {
        let own = Some(Intent::Tool { name: "close_application", args: json!({"app_name": "it", "force": true, "own_only": true}) });
        for text in ["Remove it", "Get rid of that", "Take it away", "Dismiss that", "Hide it", "Weg damit", "Entferne das", "Mach das weg", "Nimm das weg"] {
            assert_eq!(parse(text), own, "{text}");
        }
        assert_eq!(parse("Close it"), Some(Intent::Tool { name: "close_application", args: json!({"app_name": "it", "force": true}) }));
    }

    #[test]
    fn a_program_is_turned_off_and_on_and_nothing_else_is() {
        let close = |app: &str| Some(Intent::Tool { name: "close_application", args: json!({"app_name": app, "force": true}) });
        let open = |app: &str| Some(Intent::Tool { name: "open_application", args: json!({"app_name": app}) });
        assert_eq!(parse("Turn Firefox off"), close("firefox"));
        assert_eq!(parse("Turn off Discord"), close("discord"));
        assert_eq!(parse("Switch Spotify off"), close("spotify"));
        assert_eq!(parse("Schalte Firefox aus"), close("firefox"));
        assert_eq!(parse("Turn Firefox back on"), open("firefox"));
        assert_eq!(parse("Turn on Discord"), open("discord"));
        assert_eq!(parse("Schalte Discord wieder ein"), open("discord"));
        // What is no program stays where it was.
        for text in ["Turn off my PC", "Turn the lights off", "Turn off the screen", "Turn it off", "Turn that back on"] {
            assert!(!matches!(parse(text), Some(Intent::Tool { name: "close_application" | "open_application", .. })), "{text}");
        }
    }

    /// The volume requests of jai.log 2026-11-06 15:08 (each went to the language model,
    /// which set the whole PC to 0 with the music tool, then ignored "system volume one hundred
    /// percent") and the ways Sir might say the same things.
    fn volume_scenarios() {
        let app = |name: &str, level: i64| json!({"action": "set_apps", "apps": [{"app": name, "level": level}]});
        let step = |name: &str, delta: i64| json!({"action": "change_app", "app": name, "delta": delta});
        let scenarios: Vec<(&str, Value)> = vec![
            // ---- As he said them ----
            ("JAI, lower my browser's volume by 50%.", step("browser", -50)),
            ("No, don't, don't mute it, make it 50% louder.", step("it", 50)),
            ("And you make discords, volume 50%.", app("discord", 50)),
            ("Make everything loud again.", json!({"action": "restore"})),
            ("Makes system volume one hundred percent.", json!({"action": "set_master", "level": 100})),
            // ---- One app, a level ----
            ("Make Discord's volume 50%", app("discord", 50)),
            ("Discord volume 50 percent", app("discord", 50)),
            ("Set the volume of Discord to 30", app("discord", 30)),
            ("browser volume thirty percent", app("browser", 30)),
            ("Put my browser at 40 percent", app("browser", 40)),
            ("Set Spotify volume to 25%", app("spotify", 25)),
            ("make the music 20 percent", app("music", 20)),
            ("Mach Discord auf 50 Prozent", app("discord", 50)),
            ("Stell die Lautstärke von Spotify auf 30 Prozent", app("spotify", 30)),
            ("Set Discord and Spotify to 40 percent", json!({"action": "set_apps", "apps": [{"app": "discord", "level": 40}, {"app": "spotify", "level": 40}]})),
            // ---- One app, a step ----
            ("Raise Chrome by 20", step("chrome", 20)),
            ("turn Discord down", step("discord", -10)),
            ("Turn down Discord a bit", step("discord", -5)),
            ("turn up the music", step("music", 10)),
            ("Make Spotify louder", step("spotify", 10)),
            ("make spotify a lot louder", step("spotify", 20)),
            ("Discord quieter", step("discord", -10)),
            ("Spotify volume up", step("spotify", 10)),
            ("increase the browser's volume by 15 percent", step("browser", 15)),
            ("Can you lower Discord by 30 percent", step("discord", -30)),
            ("Mach Discord leiser", step("discord", -10)),
            ("Mach den Browser 20 Prozent leiser", step("browser", -20)),
            ("Discord etwas lauter", step("discord", 5)),
            ("lower discords volume by 10", step("discord", -10)),
            // ---- "It": what the last request was about ----
            ("make it 50% louder", step("it", 50)),
            ("Make it louder", step("it", 10)),
            ("turn it down by 20 percent", step("it", -20)),
            ("set it to 30 percent", json!({"action": "set_app", "app": "it", "level": 30})),
            ("mute it", json!({"action": "mute_app", "app": "it"})),
            ("unmute it", json!({"action": "unmute_app", "app": "it"})),
            // ---- Mutes ----
            ("Mute my browser", json!({"action": "mute_app", "app": "browser"})),
            ("silence Spotify", json!({"action": "mute_app", "app": "spotify"})),
            ("Unmute Discord", json!({"action": "unmute_app", "app": "discord"})),
            ("mute Discord and Spotify", json!({"action": "mute_app", "app": "discord and spotify"})),
            ("Mute everything", json!({"action": "mute_master"})),
            ("unmute everything", json!({"action": "unmute_master"})),
            ("Mute everything except Discord", json!({"action": "mute_others", "app": "discord"})),
            ("silence all but the browser", json!({"action": "mute_others", "app": "browser"})),
            ("Alles außer Discord stumm", json!({"action": "mute_others", "app": "discord"})),
            // ---- The master volume, said in words or with a degree ----
            ("System volume one hundred percent", json!({"action": "set_master", "level": 100})),
            ("make the system volume fifty percent", json!({"action": "set_master", "level": 50})),
            ("volume to max", json!({"action": "set_master", "level": 100})),
            ("full volume", json!({"action": "set_master", "level": 100})),
            ("half volume", json!({"action": "set_master", "level": 50})),
            ("Master volume to twenty five percent", json!({"action": "set_master", "level": 25})),
            ("much louder", json!({"action": "change_master", "delta": 20})),
            ("a little quieter", json!({"action": "change_master", "delta": -5})),
            ("make everything quieter", json!({"action": "change_master", "delta": -10})),
            ("turn the volume up by 15", json!({"action": "change_master", "delta": 15})),
            ("Mach alles etwas leiser", json!({"action": "change_master", "delta": -5})),
            // ---- Undo ----
            ("Put the volume back to normal", json!({"action": "restore"})),
            ("restore the volume", json!({"action": "restore"})),
            ("Undo that volume change", json!({"action": "restore"})),
            ("everything back to normal", json!({"action": "restore"})),
            ("Mach alles wieder laut", json!({"action": "restore"})),
        ];
        let wrong: Vec<String> = scenarios.iter().filter(|(text, wanted)| mixer_command(text).as_ref() != Some(wanted)).map(|(text, wanted)| format!("\"{text}\": wanted {wanted}, got {:?}", mixer_command(text))).collect();
        assert!(wrong.is_empty(), "{} of {} volume requests go wrong:\n  {}", wrong.len(), scenarios.len(), wrong.join("\n  "));
        // Every one of them is a command JAI knows: none is dropped as a song's words while
        // music plays, and none reaches the language model.
        for (text, _) in &scenarios {
            assert!(matches!(parse(text), Some(Intent::Tool { name: "control_audio_mixer" | "media_and_volume_control", .. })), "{text}: {:?}", parse(text));
        }
        // What is no volume request stays none: other things with a level, an up or a down.
        for text in [
            "Set the zoom to 125 percent",
            "turn up the brightness",
            "Turn down the lights",
            "lower the blinds by 50 percent",
            "bring up the browser",
            "open Discord",
            "play some loud music",
            "set a timer for 50 minutes",
            "make the orb 50 percent bigger",
            "raise the opacity by 20",
            "Discord",
            "what is the volume of a sphere",
        ] {
            assert_eq!(mixer_command(text), None, "{text}");
        }
    }

    #[test]
    fn short_answers_right_after_a_volume_change() {
        use crate::audio_mixer::Step;
        let down = Step { app: true, delta: Some(-10), ..Step::default() };
        let master = Step { delta: Some(10), ..Step::default() };
        let level = Step { app: true, level: Some(50), ..Step::default() };
        let muted = Step { app: true, mute: Some(true), ..Step::default() };
        let say = |last: Step, text: &str| volume_follow_up(last, &quick_words(text));
        let it = |delta: i64| Some(json!({"action": "change_app", "app": "it", "delta": delta}));
        for (text, delta) in [("More", -10), ("Again", -10), ("Nochmal", -10), ("A bit more", -5), ("Noch ein bisschen", -5), ("A lot more", -20), ("Less", 5), ("Too much", 5), ("Zu viel", 5), ("The other way", 20)] {
            assert_eq!(say(down, text), it(delta), "{text}");
        }
        assert_eq!(say(master, "Noch mehr"), Some(json!({"action": "change_master", "delta": 10})));
        assert_eq!(say(level, "More"), it(10), "more of a level is louder");
        assert_eq!(say(level, "The other way"), None, "a level has no way");
        for text in ["Same for Firefox", "Do the same for Firefox", "And Firefox too", "Also Firefox", "Firefox too", "Firefox as well", "Auch Firefox", "Firefox auch", "Dasselbe für Firefox", "Das gleiche für Firefox"] {
            assert_eq!(say(down, text), Some(json!({"action": "change_app", "app": "firefox", "delta": -10})), "{text}");
        }
        assert_eq!(say(level, "Same for Firefox"), Some(json!({"action": "set_apps", "apps": [{"app": "firefox", "level": 50}]})));
        assert_eq!(say(muted, "And Firefox too"), Some(json!({"action": "mute_app", "app": "firefox"})));
        assert_eq!(say(muted, "More"), None, "there is no more of a mute");
        for text in ["Tell me more", "Me too", "What else", "Also", "Hello too", "Thanks", "Yes"] {
            assert_eq!(say(down, text), None, "{text}");
        }
    }

    #[test]
    fn a_name_with_a_way_or_a_level_and_the_other_short_forms() {
        let list = Some(json!({"action": "list"}));
        for (text, wanted) in [
            ("Spotify 30", Some(json!({"action": "set_apps", "apps": [{"app": "spotify", "level": 30}]}))),
            ("Discord 2", None),
            ("Pump up the volume", Some(json!({"action": "change_master", "delta": 10}))),
            ("Crank the music", Some(json!({"action": "change_app", "app": "music", "delta": 10}))),
            ("Crank it down", Some(json!({"action": "change_app", "app": "it", "delta": -10}))),
            ("Lower the music a bit more", Some(json!({"action": "change_app", "app": "music", "delta": -5}))),
            ("Discord etwas runter", Some(json!({"action": "change_app", "app": "discord", "delta": -5}))),
            ("Mute all apps", Some(json!({"action": "mute_master"}))),
            ("Alle Apps stumm", Some(json!({"action": "mute_master"}))),
            ("Solo Discord", Some(json!({"action": "mute_others", "app": "discord"}))),
            ("Make Discord loud again", Some(json!({"action": "restore", "app": "discord"}))),
            ("Discord back to normal", Some(json!({"action": "restore", "app": "discord"}))),
            ("Discord wieder laut", Some(json!({"action": "restore", "app": "discord"}))),
            ("Mach Discord wieder normal", Some(json!({"action": "restore", "app": "discord"}))),
            ("Who is making that noise?", list.clone()),
            ("What's making that sound?", list.clone()),
            ("What's playing sound?", list.clone()),
            ("Was spielt gerade Ton ab?", list.clone()),
            ("Wer macht da so einen Lärm?", list.clone()),
            // (Not a name: nothing to turn.)
            ("Pump up the jam", None),
            ("Blast off", None),
            ("Make my life loud again", None),
        ] {
            assert_eq!(mixer_command(text), wanted, "{text}");
        }
        // A bit is a bit, however it is said.
        assert_eq!(silent_quick_command("A bit louder").unwrap().args, json!({"action": "change_master", "delta": 5}));
        assert_eq!(silent_quick_command("A little quieter").unwrap().args, json!({"action": "change_master", "delta": -5}));
    }

    /// Sir's own words from his conversation log (2026-11-07) that went to the model or to the
    /// wrong tool. (Written here in placeholder form where they named anything of his.)
    #[test]
    fn his_logged_words_reach_the_right_tool() {
        let fixed = |text: &str| tool(&crate::typos::normalize_user_input_typos(text));
        // "Deactivate orb mode" switched it on.
        for text in ["deactivate Orb mode", "disable orb mode", "turn orb mode off", "get out of orb mode", "Orbmodus beenden"] {
            assert_eq!(tool(text), Some(("control_orb_mode", json!({"enabled": false}))), "{text}");
        }
        assert_eq!(tool("Orb mode"), Some(("control_orb_mode", json!({"enabled": true}))));
        // The PC's own volume, and every app's.
        for text in ["set my PC's volume to 15%", "set my computer's volume to 15%", "set the PC volume to 15%"] {
            assert_eq!(tool(text), Some(("media_and_volume_control", json!({"action": "set_volume", "level": 15}))), "{text}");
        }
        for text in ["Set every volume back to 100%", "set all volumes to 100%", "set every app to 100%"] {
            assert_eq!(tool(text), Some(("control_audio_mixer", json!({"action": "set_all_apps", "level": 100}))), "{text}");
        }
        // Windows: full screen by name, another monitor.
        for text in ["make discord full screen", "make discord fullscreen", "put discord in full screen"] {
            assert_eq!(tool(text), Some(("window_management", json!({"action": "fullscreen", "target_window": "discord"}))), "{text}");
        }
        assert_eq!(tool("Make it full screen"), Some(("window_management", json!({"action": "fullscreen"}))), "the focus's, as before");
        assert_eq!(tool("Move Discord to another monitor."), Some(("move_window_to_monitor", json!({"app_or_window_name": "discord", "target_monitor": "other"}))));
        // The GPU back to him.
        for text in ["clear vram", "clear the vram", "free up my vram", "can you move out of my VRAM please?", "get out of my vram"] {
            assert_eq!(tool(text), Some(("manage_vram", json!({"action": "flush"}))), "{text}");
        }
        assert_eq!(fuzzy("go to standby").map(|t| t.1["action"].clone()), Some(json!("sleep_mode")));
        // Game mode by voice (2026-11-15); "gaming mode" stays the protocol.
        for text in ["game mode on", "turn on game mode", "switch to game mode", "start game mode please", "Spielmodus an"] {
            assert_eq!(tool(text), Some(("manage_vram", json!({"action": "game_on"}))), "{text}");
        }
        for text in ["game mode off", "turn game mode off", "leave game mode", "stop the game mode", "Spielmodus aus", "beende den Spielmodus"] {
            assert_eq!(tool(text), Some(("manage_vram", json!({"action": "game_off"}))), "{text}");
        }
        assert_ne!(tool("gaming mode").map(|t| t.0), Some("manage_vram"));
        // Sir, 2026-11-21: "activate goon mode" opens a private browser window, as Whisper may
        // write it, and through the brain's whole fast path (never read as "game mode").
        for text in ["Activate goon mode.", "JAI, activate goon mode", "goon mode on", "turn on goon mode", "enable gun mode", "start gooning mode", "goon mode", "aktiviere den goon modus"] {
            assert_eq!(tool(text), Some(("open_application", json!({"app_name": "private window"}))), "{text}");
            assert_eq!(crate::phrasings::route(text), Some(("open_application".to_string(), json!({"app_name": "private window"}))), "{text}");
        }
        assert_eq!(tool("activate game mode"), Some(("manage_vram", json!({"action": "game_on"}))), "game mode stays the game's");
        for (text, action) in [
            ("Start Batman.", "start"),
            ("launch gta 5", "start"),
            ("remind me after the game to do my homework", "after"),
            ("When GTA starts, set the volume to 40 and open Discord.", "routine"),
            ("how long have I played today?", "playtime"),
            ("any news for GTA?", "news"),
            ("Are there any free games this week?", "free"),
            ("What's using my VRAM?", "vram"),
            ("Tell me when the GTA download is done.", "watch_download"),
            ("stop gaming at 10 on school nights", "bedtime"),
            ("what games do I have", "list"),
        ] {
            assert_eq!(tool(text).map(|(name, args)| (name, args["action"].clone())), Some(("games", json!(action))), "{text}");
        }
        // The rose palette, as said and as heard.
        for text in ["make the interface rose", "make the theme rose", "switch to the rose theme", "make the interface rows", "Make the entire face a rose"] {
            assert_eq!(tool(text), Some(("configure_interface", json!({"hud_palette": "hot_rod"}))), "{text}");
        }
        // Opacity in his words (0 % ends on the floor, said so by the reply).
        assert_eq!(tool("make the opacity 50%"), Some(("configure_interface", json!({"opacity": 50}))));
        assert_eq!(tool("Make the interfaces opacity 0% please."), Some(("configure_interface", json!({"opacity": 0}))));
        let done = crate::interface::configure(&crate::interface::InterfaceSettings::default(), &json!({"opacity": 0})).unwrap();
        let said = crate::interface::spoken_ack(&json!({"opacity": 0}), &done.settings);
        assert!(said.contains("15 percent") && said.contains("lowest it goes"), "{said}");
        // Closing himself, in German and as the recogniser heard it.
        for text in ["kannst du dich selbst schließen?", "Stopp, gliese dich selbst.", "kannte ich selbst schließen, bitte?"] {
            assert_eq!(tool(text), Some(("shutdown_jai", json!({}))), "{text}");
        }
        // The window packer, said as a "don't".
        for text in ["don't make the windows overlap", "the windows shouldn't overlap", "Die Fenster sollen sich nicht überlappen"] {
            assert_eq!(tool(text), Some(("configure_interface", json!({"allow_window_overlap": false}))), "{text}");
        }
        assert_eq!(parse("don't close Discord"), Some(Intent::Answer("Very good, Sir. I'll leave it as it is.".into())), "a plain don't stays one");
        // The Integrations hub (Whisper: "integration sub").
        for text in ["Open the integration sub, please.", "open the integrations hub", "show my productivity settings"] {
            assert_eq!(tool(text), Some(("configure_productivity_service", json!({"service_name": "all", "action": "open"}))), "{text}");
        }
        // Playlists: "called", said twice, or none named.
        let m = |text: &str| tool(text).filter(|(name, _)| *name == "control_music").map(|t| t.1);
        assert_eq!(m("play back in black from my playlist called example rock"), Some(json!({"action": "play", "song_name": "back in black", "playlist_name": "example rock"})));
        assert_eq!(m("Now play back in black. From my playlist called Example Rock.").map(|a| (a["song_name"].clone(), a["playlist_name"].clone())), Some((json!("back in black"), json!("example rock"))));
        assert_eq!(m("play Example Rock playlist, my Example Rock playlist"), Some(json!({"action": "play", "playlist_name": "example rock"})));
        assert!(m("Can you play a specific playlist?").is_none_or(|a| a["playlist_name"] != "a specific"));
        // A typo of his.
        assert_eq!(crate::typos::normalize_user_input_typos("stop listeting"), "stop listening");
        assert!(fixed("stop listeting").is_none_or(|(name, _)| name != "close_application"));
        // "Should I restart my PC?" answered with the order (never the model's own plan).
        for (text, order) in [("Should I restart my PC?", "\"Restart my PC\""), ("How do I shut down my PC?", "\"Shut down my PC\""), ("Should I lock my PC?", "\"Lock my PC\""), ("Should I shut down the PC?", "\"Shut down the PC\"")] {
            match parse(text) {
                Some(Intent::Answer(line)) => assert!(line.contains(order), "{text}: {line}"),
                other => panic!("{text}: {other:?}"),
            }
        }
        assert!(matches!(parse("Should I restart my PC?"), Some(Intent::Answer(line)) if line.contains("ask once more")));
        assert!(matches!(parse("What happens if I restart my PC?"), Some(Intent::Answer(line)) if line.contains("anything unsaved")));
    }

    #[test]
    fn terminal_is_no_calendar_and_a_german_add_adds() {
        use crate::integration_router::route_text;
        assert!(route_text("Then Sanabhadra, Terminal.").is_none_or(|r| r.tool != "get_calendar_schedule"));
        for text in ["füge einen Termin in meinem Kalender um 15 Uhr hinzu", "flüge einen Event hinzu in meinem Kalender um 15 Uhr"] {
            let fixed = crate::typos::normalize_user_input_typos(text);
            let route = route_text(&fixed).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(route.tool, "create_calendar_event", "{text}");
            assert!(route.args["start_time"].as_str().is_some_and(|t| t.ends_with("15:00")), "{text}: {}", route.args);
            assert!(matches!(route.args["title"].as_str(), Some("Termin" | "Event")), "{text}: {}", route.args);
        }
        // A real title stays the title.
        assert_eq!(route_text("Füge Elternabend am Freitag um 19 Uhr zu meinem Kalender hinzu").unwrap().args["title"], "Elternabend");
    }
}


#[cfg(test)]
mod instant_answers {
    use super::*;

    /// 2026-11-15, game mode: the questions he asks in a game that need no model (the probe
    /// before: temperatures, the next lesson and "is game mode on" went to the CPU brain).
    #[test]
    fn instant_answers_need_no_model() {
        let tool = |q: &str| match parse(q) {
            Some(Intent::Tool { name, args }) => Some((name, args)),
            _ => None,
        };
        assert_eq!(tool("what are my temperatures"), Some(("get_system_status", json!({"reading": "all"}))));
        assert_eq!(tool("how are my PC temps"), Some(("get_system_status", json!({"reading": "all"}))));
        assert_eq!(tool("is game mode on"), Some(("manage_vram", json!({"action": "game_status"}))));
        assert_eq!(tool("am I in game mode"), Some(("manage_vram", json!({"action": "game_status"}))));
        for q in ["what's my next lesson", "when is my next class", "Was ist meine nächste Stunde?", "which lesson do I have next"] {
            assert_eq!(tool(q), Some(("next_lesson", json!({}))), "{q}");
        }
        for q in ["what's playing", "what's the volume", "how hot is my GPU"] {
            assert!(tool(q).is_some(), "{q}");
        }
        assert_eq!(parse("what's the date"), Some(Intent::Date));
    }
}
