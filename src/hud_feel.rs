//! The look-and-feel features Sir chose one by one on 2026-11-12, as he asks for them: the
//! phrases that switch them (`command`, part of the fast path's `configure_interface`
//! arguments), the ones that are not settings but requests of the moment (`apply`: subtitles,
//! lyrics, tonight's night mode, "what did I miss?", the voice set-up) and what he says to each
//! (`spoken`). The settings themselves live in `InterfaceSettings`; what each feature draws and
//! does lives in its own module.

use chrono::{NaiveDateTime, Timelike};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn flag(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

// ------------------------------------------------------------------------------ phrases ----

/// A switch of the interface as Sir names it: its `configure_interface` key, the words that
/// name it, and the words that mean "off" for it on top of the general ones.
struct Switch {
    key: &'static str,
    names: &'static str,
}

/// (Names are alternations inside one group; the longest first where one contains another.)
const SWITCHES: [Switch; 11] = [
    Switch { key: "calm_hud", names: r"calm\s+mode|calm\s+hud|quiet\s+hud|ruhiger\s+modus|ruhemodus" },
    Switch { key: "live_words", names: r"live\s+words|live\s+captions?|live\s+transcript|my\s+words\s+(?:while|as)\s+i\s+(?:speak|talk)|live\s+mitschrift" },
    Switch { key: "startup_sequence", names: r"start\s*-?\s*up\s+sequence|boot\s+sequence|power\s*-?\s*up\s+sequence|startsequenz" },
    Switch { key: "idle_screen", names: r"idle\s+screen|calm\s+screen|screen\s*saver|ruhebildschirm" },
    Switch { key: "night_mode", names: r"automatic\s+night\s+mode|night\s+mode\s+by\s+the\s+clock|automatischer\s+nachtmodus" },
    Switch { key: "night_now", names: r"night\s+mode|nachtmodus" },
    Switch { key: "edge_light", names: r"edge\s+lights?|edge\s+glow|screen\s+edge\s+lights?|randlicht" },
    Switch { key: "music_visuals", names: r"music\s+visuals?|music\s+visuali[sz]er|visuali[sz]er|musikvisualisierung" },
    Switch { key: "mini_strip", names: r"mini\s+mode|mini\s+strip|mini\s+bar|minimodus" },
    Switch { key: "desktop_background", names: r"live\s+desktop(?:\s+background)?|desktop\s+background|live\s+wallpaper|live\s+hintergrund" },
    Switch { key: "subtitles", names: r"live\s+subtitles|subtitles|untertitel" },
];

/// "turn on night mode", "night mode off", "mini mode", "schalte den nachtmodus aus".
static SWITCH: LazyLock<Regex> = LazyLock::new(|| {
    let names: Vec<String> = SWITCHES.iter().map(|s| format!("(?P<{}>{})", s.key, s.names)).collect();
    Regex::new(&format!(
        r"^(?:(?:please\s+)?(?:turn|switch|put|go\s+(?:in)?to|enter|start|begin|open|show|use|give\s+me|schalte?|mach|starte?)\s+)?(?P<pre>on|off|an|aus|enable|disable|stop|leave|end|exit|close|hide|no|without|skip)?\s*(?:the\s+|your\s+|my\s+|a\s+|den\s+|die\s+|das\s+|deinen?\s+)?(?:{})(?:\s+(?P<post>on|off|an|aus|ein|again|please|now))?$",
        names.join("|")
    ))
    .expect("valid switch regex")
});

/// Phrases that are not "<switch> on/off": each (pattern, arguments).
static PHRASES: LazyLock<Vec<(Regex, Value)>> = LazyLock::new(|| {
    let p = |pattern: &str, args: Value| (Regex::new(pattern).expect("valid phrase regex"), args);
    vec![
        // Calm or dense.
        p(r"^(?:show\s+(?:me\s+)?(?:everything|all\s+(?:the\s+)?readouts|the\s+readouts)|dense\s+(?:mode|hud)|busy\s+(?:mode|hud)|zeig\s+(?:mir\s+)?alles)$", json!({"calm_hud": false})),
        p(r"^(?:hide\s+the\s+readouts|calm\s+(?:the\s+hud|down\s+the\s+hud|it\s+down)|less\s+on\s+(?:the\s+)?screen)$", json!({"calm_hud": true})),
        p(r"^(?:stop\s+showing\s+my\s+words|don'?t\s+show\s+my\s+words)$", json!({"live_words": false})),
        p(r"^show\s+(?:me\s+)?my\s+words(?:\s+(?:while|as)\s+i\s+(?:speak|talk))?$", json!({"live_words": true})),
        // Night.
        p(r"^(?:day\s+mode|it'?s\s+(?:day|morning)|tagmodus)$", json!({"night_now": false})),
        p(r"^(?:never\s+(?:use|go\s+(?:in)?to)\s+night\s+mode|no\s+night\s+mode\s+by\s+(?:yourself|itself))$", json!({"night_mode": false})),
        // Music.
        p(r"^(?:move|dance|pulse)\s+(?:with|to)\s+the\s+music$", json!({"music_visuals": true})),
        p(r"^(?:stop|don'?t)\s+(?:moving|dancing|pulsing|move|dance|pulse)\s+(?:with|to)\s+the\s+music$", json!({"music_visuals": false})),
        // Mini mode and the desktop.
        p(r"^(?:go\s+mini|shrink\s+to\s+(?:a|the)\s+strip)$", json!({"mini_strip": true})),
        p(r"^(?:be|become)\s+my\s+(?:desktop\s+)?(?:background|wallpaper)$", json!({"desktop_background": true})),
        p(r"^(?:stop\s+being|leave)\s+my\s+(?:desktop\s+)?(?:background|wallpaper)$", json!({"desktop_background": false})),
        // Subtitles.
        p(r"^(?:caption|subtitle)\s+(?:this|that|it|the\s+video|the\s+stream|the\s+call)$", json!({"subtitles": true})),
        p(r"^(?:translate\s+(?:the\s+)?subtitles|(?:english\s+subtitles|subtitles\s+in\s+english)|translated\s+subtitles|(?:caption|subtitle)\s+(?:this|that|it)\s+in\s+english|untertitel\s+auf\s+englisch)$", json!({"subtitles": true, "subtitles_translate": true})),
        p(r"^(?:stop\s+translating(?:\s+the\s+subtitles)?|subtitles\s+in\s+the\s+original|original\s+subtitles)$", json!({"subtitles": true, "subtitles_translate": false})),
        // Lyrics.
        p(r"^(?:(?:show|display|give)\s+(?:me\s+)?(?:the\s+)?lyrics|lyrics(?:\s+on|\s+please)?|sing\s+along|zeig\s+(?:mir\s+)?(?:den\s+)?(?:songtext|liedtext))$", json!({"lyrics": true})),
        p(r"^(?:(?:hide|stop|close|remove)\s+(?:the\s+)?lyrics|lyrics\s+off|no\s+(?:more\s+)?lyrics)$", json!({"lyrics": false})),
        // What he showed while Sir was not looking.
        p(r"^(?:what\s+did\s+i\s+miss|did\s+i\s+miss\s+(?:anything|something)|(?:show|open)\s+(?:me\s+)?(?:my\s+|the\s+)?(?:notifications|notification\s+history|missed\s+notifications)|was\s+habe\s+ich\s+verpasst)$", json!({"missed": true})),
        // His voice.
        p(r"^(?:learn\s+my\s+voice|(?:start\s+|run\s+|open\s+)?(?:the\s+)?voice\s+set\s*-?\s*up|calibrate\s+(?:to\s+)?my\s+voice|tune\s+(?:in\s+)?to\s+my\s+voice|lern\s+meine\s+stimme)$", json!({"voice_setup": true})),
        p(r"^(?:only\s+(?:obey|listen\s+to|answer(?:\s+to)?)\s+me|(?:obey|listen\s+to)\s+(?:only\s+)?(?:me|my\s+voice)\s+(?:only|alone)|listen\s+only\s+to\s+(?:me|my\s+voice)|voice\s+lock(?:\s+on)?|h(?:ö|oe)r\s+nur\s+auf\s+mich)$", json!({"voice_lock": true})),
        p(r"^(?:obey\s+(?:everyone|everybody|anyone)|listen\s+to\s+(?:everyone|everybody|anyone)(?:\s+again)?|voice\s+lock\s+off|h(?:ö|oe)r\s+auf\s+alle)$", json!({"voice_lock": false})),
        // Everything in one place.
        p(r"^(?:(?:open|show)\s+(?:me\s+)?(?:the\s+|your\s+|all\s+)?settings(?:\s+panel)?|settings|einstellungen(?:\s+(?:öffnen|zeigen))?)$", json!({"settings_panel": true})),
    ]
});

/// A request for one of these features as `configure_interface` arguments (`s`: the fast
/// path's normalised phrase).
pub fn command(s: &str) -> Option<Value> {
    let s = s.trim();
    if let Some((_, args)) = PHRASES.iter().find(|(pattern, _)| pattern.is_match(s)) {
        return Some(args.clone());
    }
    let c = SWITCH.captures(s)?;
    let key = SWITCHES.iter().map(|sw| sw.key).find(|key| c.name(key).is_some())?;
    let word = |name: &str| c.name(name).map(|m| m.as_str());
    let off = |w: &str| matches!(w, "off" | "aus" | "disable" | "stop" | "leave" | "end" | "exit" | "close" | "hide" | "no" | "without" | "skip");
    let on = !(word("pre").is_some_and(off) || word("post").is_some_and(off));
    Some(json!({ key: on }))
}

// ---------------------------------------------------------------------- a game in front ----

/// Whether a game (or another program that fills its screen) is in front: the live words and
/// the subtitles pause then, so they cost the game nothing. Looked up at most every 3 s.
pub fn game_in_front() -> bool {
    static SEEN: Mutex<Option<(std::time::Instant, bool)>> = Mutex::new(None);
    let mut seen = lock(&SEEN);
    if let Some((at, gaming)) = *seen {
        if at.elapsed() < std::time::Duration::from_secs(3) {
            return gaming;
        }
    }
    let gaming = game_in_front_now();
    *seen = Some((std::time::Instant::now(), gaming));
    gaming
}

#[cfg(windows)]
fn game_in_front_now() -> bool {
    let (process, own) = crate::win32::foreground_info().unwrap_or_default();
    !own && (is_game(&process) || crate::win32::foreground_is_fullscreen())
}

#[cfg(not(windows))]
fn game_in_front_now() -> bool {
    false
}

/// A game's process (the listed games; a voice chat in front is no game).
pub fn is_game(process: &str) -> bool {
    let p = process.to_lowercase();
    let p = p.trim_end_matches(".exe");
    crate::ears::is_game_or_voice_chat(p) && !["discord", "discordptb", "discordcanary", "teamspeak", "ts3client_win64", "ts3client", "mumble", "guilded"].contains(&p)
}

// ------------------------------------------------------------------- requests of the moment ----

/// The arguments that are requests of the moment, not settings (`interface::configure` names
/// them in its summary; `apply` carries them out).
pub const RUNTIME_KEYS: [(&str, &str); 8] = [
    ("night_now", "night mode"),
    ("subtitles", "subtitles"),
    ("subtitles_translate", "subtitle translation"),
    ("lyrics", "lyrics"),
    ("missed", "notification history"),
    ("voice_setup", "voice set-up"),
    ("voice_lock", "voice lock"),
    ("settings_panel", "settings panel"),
];

/// Tonight's night mode as Sir set it by hand: (on, the morning it lapses).
static NIGHT_OVERRIDE: Mutex<Option<(bool, NaiveDateTime)>> = Mutex::new(None);
/// Live subtitles: wanted, and whether translated into English.
static SUBTITLES: AtomicBool = AtomicBool::new(false);
static SUBTITLES_TRANSLATE: AtomicBool = AtomicBool::new(false);
static LYRICS: AtomicBool = AtomicBool::new(false);
static MISSED: AtomicBool = AtomicBool::new(false);
static VOICE_SETUP: AtomicBool = AtomicBool::new(false);
static SETTINGS_PANEL: AtomicBool = AtomicBool::new(false);
/// "Only obey me" / "obey everyone", for the voice lock to pick up.
static VOICE_LOCK: Mutex<Option<bool>> = Mutex::new(None);

/// Night runs from this hour to that one.
pub const NIGHT_FROM: u32 = 22;
pub const NIGHT_UNTIL: u32 = 6;

/// The next time the clock shows `NIGHT_UNTIL` after `now`.
fn next_morning(now: NaiveDateTime) -> NaiveDateTime {
    let morning = now.date().and_hms_opt(NIGHT_UNTIL, 0, 0).unwrap_or(now);
    if now < morning { morning } else { morning + chrono::Duration::days(1) }
}

/// Whether it is night for him at `now`: by the clock (22:00 to 6:00) when `by_clock` is on,
/// unless Sir said otherwise for tonight ("night mode off" holds until the morning, "night mode"
/// at noon holds until the next one).
pub fn night_at(now: NaiveDateTime, by_clock: bool) -> bool {
    let mut held = lock(&NIGHT_OVERRIDE);
    if held.is_some_and(|(_, until)| now >= until) {
        *held = None;
    }
    match *held {
        Some((on, _)) => on,
        None => by_clock && (now.hour() >= NIGHT_FROM || now.hour() < NIGHT_UNTIL),
    }
}

/// Whether it is night for him now.
pub fn night(by_clock: bool) -> bool {
    night_at(chrono::Local::now().naive_local(), by_clock)
}

/// Whether night mode goes by the clock (the setting, as the HUD last saw it), and whether it
/// is night right now as of the last look (`night_active`): the sound thread reads the latter.
static NIGHT_BY_CLOCK: AtomicBool = AtomicBool::new(true);
static NIGHT_NOW: AtomicBool = AtomicBool::new(false);

pub fn set_night_by_clock(on: bool) {
    NIGHT_BY_CLOCK.store(on, Ordering::Release);
}

/// Whether it is night for him right now (looked up, and noted for `night_gain`). Never in a
/// test session: what a scripted session checks does not depend on the hour it runs at.
pub fn night_active() -> bool {
    let night = !crate::platform::in_test_session() && night(NIGHT_BY_CLOCK.load(Ordering::Acquire));
    NIGHT_NOW.store(night, Ordering::Release);
    night
}

/// How loud he is at night against the day (his voice and nothing else: the cues are off).
pub const NIGHT_GAIN: f32 = 0.6;

/// The share of his voice's volume right now (as of the last `night_active`).
pub fn night_gain() -> f32 {
    if NIGHT_NOW.load(Ordering::Relaxed) { NIGHT_GAIN } else { 1.0 }
}

/// What the language model is told at night.
pub fn live_context() -> String {
    if night_active() { "NIGHT MODE: it is late and Sir wants quiet. Answer in one short sentence.\n".to_string() } else { String::new() }
}

pub fn set_night(on: bool, now: NaiveDateTime) {
    *lock(&NIGHT_OVERRIDE) = Some((on, next_morning(now)));
}

/// Live subtitles wanted: Some(translate into English).
pub fn subtitles() -> Option<bool> {
    SUBTITLES.load(Ordering::Acquire).then(|| SUBTITLES_TRANSLATE.load(Ordering::Acquire))
}

pub fn set_subtitles(on: bool) {
    SUBTITLES.store(on, Ordering::Release);
}

pub fn lyrics() -> bool {
    LYRICS.load(Ordering::Acquire)
}

pub fn set_lyrics(on: bool) {
    LYRICS.store(on, Ordering::Release);
}

/// The playing song's current line, while the lyrics are on (published by the lyrics module).
static LYRIC_LINE: Mutex<Option<String>> = Mutex::new(None);

pub fn lyric_line() -> Option<String> {
    lyrics().then(|| lock(&LYRIC_LINE).clone()).flatten()
}

pub fn publish_lyric_line(line: Option<String>) {
    *lock(&LYRIC_LINE) = line;
}

/// "What did I miss?" was asked (once).
pub fn take_missed() -> bool {
    MISSED.swap(false, Ordering::AcqRel)
}

/// "Learn my voice" was asked (once).
pub fn take_voice_setup() -> bool {
    VOICE_SETUP.swap(false, Ordering::AcqRel)
}

/// The settings panel was asked for (once).
pub fn take_settings_panel() -> bool {
    SETTINGS_PANEL.swap(false, Ordering::AcqRel)
}

pub fn open_settings_panel() {
    SETTINGS_PANEL.store(true, Ordering::Release);
}

/// Whether the voice lock is on right now (published by the voice lock; the settings panel
/// shows it).
static VOICE_LOCKED: AtomicBool = AtomicBool::new(false);

pub fn voice_locked() -> bool {
    VOICE_LOCKED.load(Ordering::Acquire)
}

pub fn publish_voice_locked(on: bool) {
    VOICE_LOCKED.store(on, Ordering::Release);
}

/// "Only obey me" (true) or "obey everyone" (false) was asked (once).
pub fn take_voice_lock() -> Option<bool> {
    lock(&VOICE_LOCK).take()
}

/// Carries out the requests of the moment in `configure_interface`'s arguments.
pub fn apply(args: &Value) {
    if let Some(on) = flag(args, "night_now") {
        set_night(on, chrono::Local::now().naive_local());
    }
    if let Some(on) = flag(args, "subtitles") {
        set_subtitles(on);
    }
    if let Some(on) = flag(args, "subtitles_translate") {
        SUBTITLES_TRANSLATE.store(on, Ordering::Release);
    }
    if let Some(on) = flag(args, "lyrics") {
        set_lyrics(on);
    }
    if flag(args, "missed") == Some(true) {
        MISSED.store(true, Ordering::Release);
    }
    if flag(args, "voice_setup") == Some(true) {
        VOICE_SETUP.store(true, Ordering::Release);
    }
    if let Some(on) = flag(args, "voice_lock") {
        *lock(&VOICE_LOCK) = Some(on);
    }
    if flag(args, "settings_panel") == Some(true) {
        open_settings_panel();
    }
}

// ------------------------------------------------------------------------- what he says ----

/// His answer to a request for one of these features (None: not one of them).
pub fn spoken(args: &Value) -> Option<String> {
    let on = |key: &str| flag(args, key);
    let say = |yes: &str, no: &str, state: bool| Some(if state { yes } else { no }.to_string());
    if let Some(v) = on("calm_hud") {
        return say("Calm it is, Sir. The readouts step back until you reach for them.", "Everything on screen, Sir.", v);
    }
    if let Some(v) = on("live_words") {
        return say("You'll see your words as you say them, Sir.", "Very well, Sir. Your words appear once you have finished.", v);
    }
    if let Some(v) = on("startup_sequence") {
        return say("The start-up sequence is back, Sir.", "Straight to work from now on, Sir. No start-up sequence.", v);
    }
    if let Some(v) = on("idle_screen") {
        return say("I'll dim down to the clock when you are away, Sir.", "The board stays as it is when you are away, Sir.", v);
    }
    if let Some(v) = on("night_mode") {
        return say("Night mode from ten to six, Sir, by the clock.", "I'll leave night mode to you, Sir.", v);
    }
    if let Some(v) = on("night_now") {
        return say("Night mode, Sir. Dim and quiet until the morning.", "Night mode off for tonight, Sir.", v);
    }
    if let Some(v) = on("edge_light") {
        return say("The edge light is on, Sir.", "Edge light off, Sir.", v);
    }
    if let Some(v) = on("music_visuals") {
        return say("I'll move with the music, Sir.", "I'll hold still, Sir.", v);
    }
    if let Some(v) = on("mini_strip") {
        return say("Mini mode, Sir. I'm the strip at the top of your screen.", "Back to the full board, Sir.", v);
    }
    if let Some(v) = on("desktop_background") {
        return say("I'm your desktop background now, Sir.", "Your desktop is your own again, Sir.", v);
    }
    if let Some(v) = on("subtitles_translate").filter(|_| on("subtitles") != Some(false)) {
        return say("Subtitles in English, Sir.", "Subtitles in the language spoken, Sir.", v);
    }
    if let Some(v) = on("subtitles") {
        return say("Subtitles on, Sir.", "Subtitles off, Sir.", v);
    }
    if let Some(v) = on("lyrics") {
        return say("The lyrics, Sir, as the song goes.", "Lyrics off, Sir.", v);
    }
    if on("missed") == Some(true) {
        return Some("Here is what came up, Sir.".to_string());
    }
    if on("voice_setup") == Some(true) {
        return Some("Let us get acquainted, Sir. Read me the lines on the screen.".to_string());
    }
    if let Some(v) = on("voice_lock") {
        return say("From now on I answer to your voice alone, Sir.", "I'll answer to anyone again, Sir.", v);
    }
    if on("settings_panel") == Some(true) {
        return Some("Everything I can be set to, Sir, in one place.".to_string());
    }
    None
}

/// The tool schema's lines for these arguments (appended to `configure_interface`'s).
pub fn schema() -> Vec<(&'static str, Value)> {
    let b = |description: &str| json!({"type": "boolean", "description": description});
    vec![
        ("calm_hud", b("Calm HUD: the edge readouts step back until the mouse comes near.")),
        ("live_words", b("Sir's words appear while he speaks.")),
        ("startup_sequence", b("The power-up sequence at start.")),
        ("idle_screen", b("Dim to a clock when unused for some minutes.")),
        ("night_mode", b("Go into night mode by the clock (22:00-6:00).")),
        ("night_now", b("Night mode for tonight (dim, quiet, brief), or off for tonight.")),
        ("edge_light", b("The screen's edges glow for a moment when something happens.")),
        ("music_visuals", b("The emblem moves with the music that plays on the PC.")),
        ("mini_strip", b("Mini mode: a slim strip at the top of a screen instead of the board.")),
        ("desktop_background", b("The emblem and clock as the live desktop background.")),
        ("subtitles", b("Live subtitles of what is said in a video, stream or call.")),
        ("subtitles_translate", b("Translate the live subtitles into English.")),
        ("lyrics", b("The playing song's lyrics, line by line, under the emblem.")),
        ("missed", b("Show the notifications of the last hours ('what did I miss?').")),
        ("voice_setup", b("The one-minute set-up that tunes him to Sir's voice.")),
        ("voice_lock", b("Only obey Sir's own voice (false: obey everyone).")),
        ("settings_panel", b("Open the settings panel (every switch, searchable).")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn says(text: &str) -> Option<Value> {
        command(&text.to_lowercase())
    }

    #[test]
    fn the_features_answer_to_their_names() {
        for (text, want) in [
            ("night mode", json!({"night_now": true})),
            ("turn on night mode", json!({"night_now": true})),
            ("night mode off", json!({"night_now": false})),
            ("turn off night mode", json!({"night_now": false})),
            ("schalte den nachtmodus aus", json!({"night_now": false})),
            ("day mode", json!({"night_now": false})),
            ("automatic night mode off", json!({"night_mode": false})),
            ("never use night mode", json!({"night_mode": false})),
            ("night mode by the clock", json!({"night_mode": true})),
            ("calm mode", json!({"calm_hud": true})),
            ("calm mode off", json!({"calm_hud": false})),
            ("show everything", json!({"calm_hud": false})),
            ("hide the readouts", json!({"calm_hud": true})),
            ("live words off", json!({"live_words": false})),
            ("show my words while i speak", json!({"live_words": true})),
            ("skip the start-up sequence", json!({"startup_sequence": false})),
            ("startup sequence on", json!({"startup_sequence": true})),
            ("idle screen off", json!({"idle_screen": false})),
            ("edge light on", json!({"edge_light": true})),
            ("turn off the edge light", json!({"edge_light": false})),
            ("move with the music", json!({"music_visuals": true})),
            ("stop moving with the music", json!({"music_visuals": false})),
            ("music visuals off", json!({"music_visuals": false})),
            ("mini mode", json!({"mini_strip": true})),
            ("go mini", json!({"mini_strip": true})),
            ("leave mini mode", json!({"mini_strip": false})),
            ("mini mode off", json!({"mini_strip": false})),
            ("live desktop on", json!({"desktop_background": true})),
            ("be my desktop background", json!({"desktop_background": true})),
            ("desktop background off", json!({"desktop_background": false})),
            ("subtitles on", json!({"subtitles": true})),
            ("turn on subtitles", json!({"subtitles": true})),
            ("subtitles off", json!({"subtitles": false})),
            ("untertitel aus", json!({"subtitles": false})),
            ("caption this", json!({"subtitles": true})),
            ("translate the subtitles", json!({"subtitles": true, "subtitles_translate": true})),
            ("subtitles in english", json!({"subtitles": true, "subtitles_translate": true})),
            ("show the lyrics", json!({"lyrics": true})),
            ("show me the lyrics", json!({"lyrics": true})),
            ("hide the lyrics", json!({"lyrics": false})),
            ("lyrics off", json!({"lyrics": false})),
            ("what did i miss", json!({"missed": true})),
            ("show my notifications", json!({"missed": true})),
            ("learn my voice", json!({"voice_setup": true})),
            ("voice setup", json!({"voice_setup": true})),
            ("only obey me", json!({"voice_lock": true})),
            ("listen only to my voice", json!({"voice_lock": true})),
            ("obey everyone", json!({"voice_lock": false})),
            ("open settings", json!({"settings_panel": true})),
            ("show me all settings", json!({"settings_panel": true})),
        ] {
            assert_eq!(says(text), Some(want), "{text}");
        }
    }

    /// Talk about these things is not an order for them.
    #[test]
    fn talk_about_them_is_no_order() {
        for text in [
            "is night mode on",
            "what is night mode",
            "i like the night",
            "the subtitles of this film are bad",
            "who wrote the lyrics",
            "what are the lyrics about",
            "open the settings of firefox",
            "the music is too loud",
            "miss you",
            "obey",
            "my voice is hoarse",
            "mini golf",
            "calm down",
        ] {
            assert_eq!(says(text), None, "{text}");
        }
    }

    #[test]
    fn every_request_has_an_answer_and_a_schema_line() {
        let keys: Vec<&str> = schema().iter().map(|(key, _)| *key).collect();
        for sw in &SWITCHES {
            assert!(keys.contains(&sw.key), "{} has no schema line", sw.key);
            assert!(spoken(&json!({ sw.key: true })).is_some() && spoken(&json!({ sw.key: false })).is_some(), "{}", sw.key);
        }
        for (_, args) in PHRASES.iter() {
            assert!(spoken(args).is_some(), "{args}");
            assert!(args.as_object().unwrap().keys().all(|k| keys.contains(&k.as_str())), "{args}");
        }
        for (key, _) in RUNTIME_KEYS {
            assert!(keys.contains(&key), "{key}");
        }
        assert_eq!(spoken(&json!({"accent_color": "red"})), None);
    }

    /// Night by the clock runs from ten to six; "night mode off" holds until the morning and
    /// "night mode" at noon until the next one; the automatic can be switched off.
    #[test]
    fn night_follows_the_clock_until_sir_says_otherwise() {
        let _one = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        *lock(&NIGHT_OVERRIDE) = None;
        let at = |day: u32, h: u32, m: u32| chrono::NaiveDate::from_ymd_opt(2026, 11, day).unwrap().and_hms_opt(h, m, 0).unwrap();
        assert!(!night_at(at(12, 21, 59), true));
        assert!(night_at(at(12, 22, 0), true));
        assert!(night_at(at(13, 5, 59), true));
        assert!(!night_at(at(13, 6, 0), true));
        assert!(!night_at(at(12, 23, 0), false), "not by himself when the automatic is off");
        // "Night mode off" at eleven: off until six, then the clock again.
        set_night(false, at(12, 23, 0));
        assert!(!night_at(at(13, 2, 0), true));
        assert!(!night_at(at(13, 6, 1), true));
        assert!(night_at(at(13, 22, 30), true), "the next night is night again");
        // "Night mode" at noon: on until the next morning.
        set_night(true, at(14, 12, 0));
        assert!(night_at(at(14, 15, 0), true) && night_at(at(15, 5, 0), false));
        assert!(!night_at(at(15, 6, 0), true));
        assert!(lock(&NIGHT_OVERRIDE).is_none(), "the morning clears what was said");
    }

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn a_voice_chat_in_front_is_no_game() {
        assert!(is_game("javaw.exe") && is_game("cs2") && is_game("Cyberpunk2077.exe"));
        assert!(!is_game("Discord.exe") && !is_game("firefox.exe") && !is_game("Code.exe"));
    }

    #[test]
    fn requests_of_the_moment_are_taken_once() {
        let _one = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        apply(&json!({"subtitles": true, "subtitles_translate": true}));
        assert_eq!(subtitles(), Some(true));
        apply(&json!({"subtitles": false}));
        assert_eq!(subtitles(), None);
        apply(&json!({"subtitles_translate": false, "lyrics": true, "missed": true, "voice_setup": true, "voice_lock": true, "settings_panel": true}));
        assert!(lyrics() && take_missed() && !take_missed() && take_voice_setup() && !take_voice_setup());
        assert_eq!((take_voice_lock(), take_voice_lock()), (Some(true), None));
        assert!(take_settings_panel() && !take_settings_panel());
        apply(&json!({"lyrics": false}));
        assert!(!lyrics());
    }
}
