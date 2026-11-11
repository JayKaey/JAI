//! Away Mode: JAI minds the PC while Sir is out.
//!
//! * On by voice ("I'm heading out", "ich bin weg", "away mode on") or by itself once the PC has
//!   been locked for `away.auto_after_locked_mins` minutes.
//! * While it is on, friends' Discord and WhatsApp messages are collected (a Discord DM gets the
//!   answering machine once per person). Every alert JAI raises (a reminder, the Guardian, an
//!   urgent WhatsApp, a finished download) also reaches Sir's phone as a DM from JAI's own
//!   Discord account, and so does a drive plugged into the PC or someone using it after Sir said
//!   he was leaving (he can answer "lock my PC" right there).
//! * Off when Sir is back: he says so, unlocks the PC after an automatic Away Mode, or uses it
//!   after telling JAI he was leaving. "Welcome back, Sir", how long he was gone and what came
//!   in; "what did I miss?" goes through it.

use crate::crash_guard::lock;
use crate::tools::{str_arg, ToolContext};
use chrono::{DateTime, Local};
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::info;

pub const TOOL: &str = "away_mode";
const KEEP: usize = 200;
/// Keyboard or mouse this soon after Sir said he was leaving is still him at the desk.
const GRACE: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Armed {
    /// Sir said he was leaving.
    Voice,
    /// The PC had been locked long enough.
    Locked,
}

#[derive(Debug, Clone)]
pub struct Note {
    pub at: DateTime<Local>,
    pub source: String,
    pub from: String,
    pub text: String,
    pub place: String,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub since: DateTime<Local>,
    started: Instant,
    pub armed: Armed,
    pub reason: String,
    pub messages: Vec<Note>,
    pub alerts: Vec<(DateTime<Local>, String)>,
    pub ended: Option<DateTime<Local>>,
    /// The drive letters present (bit mask) when it started.
    drives: u32,
}

static AWAY: Mutex<Option<Session>> = Mutex::new(None);
/// The last finished session: "what did I miss?" after the welcome back.
static LAST: Mutex<Option<Session>> = Mutex::new(None);

fn spoken(display: impl AsRef<str>, line: impl AsRef<str>) -> String {
    format!("{}\nSpoken: {}", display.as_ref(), line.as_ref())
}

fn short(text: &str, max: usize) -> String {
    let t: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        format!("{}…", t.trim_end())
    } else {
        t
    }
}

pub fn is_away() -> bool {
    lock(&AWAY).is_some()
}

/// Starts Away Mode (false: it was on already).
pub fn arm(armed: Armed, reason: &str) -> bool {
    let mut away = lock(&AWAY);
    if away.is_some() {
        return false;
    }
    info!("[AWAY] On ({})", if armed == Armed::Voice { "Sir is out" } else { "the PC has been locked" });
    *away = Some(Session { since: Local::now(), started: Instant::now(), armed, reason: reason.trim().to_string(), messages: Vec::new(), alerts: Vec::new(), ended: None, drives: drives() });
    true
}

/// Ends Away Mode and returns the welcome back (None: it wasn't on).
pub fn end() -> Option<String> {
    let mut session = lock(&AWAY).take()?;
    session.ended = Some(Local::now());
    info!("[AWAY] Off after {} min ({} message(s), {} alert(s))", (Local::now() - session.since).num_minutes(), session.messages.len(), session.alerts.len());
    crate::discord_bot::away_ended();
    let line = welcome_back(&session, Local::now());
    *lock(&LAST) = Some(session);
    Some(line)
}

/// Ends it and, when something came in, offers to go through it ("yes" reads the digest).
fn end_with_offer(tools: &ToolContext) -> Option<String> {
    let line = end()?;
    let missed = lock(&LAST).as_ref().is_some_and(|s| !s.messages.is_empty() || !s.alerts.is_empty());
    if missed {
        tools.focus.note_offer(crate::focus::PendingOffer { tool: TOOL.into(), args: json!({ "action": "digest" }), terminate: None });
    }
    Some(line)
}

/// A message for Sir while he is away (Discord, WhatsApp), kept for the digest.
pub fn note_message(source: &str, from: &str, text: &str, place: &str) {
    let mut away = lock(&AWAY);
    let Some(s) = away.as_mut() else { return };
    if s.messages.len() >= KEEP {
        s.messages.remove(0);
    }
    s.messages.push(Note { at: Local::now(), source: source.into(), from: from.into(), text: short(text.trim(), 1000), place: place.into() });
}

/// Every alert JAI raises while Sir is away: kept for the digest and sent to his phone.
pub fn relay_alert(message: &str) {
    {
        let mut away = lock(&AWAY);
        let Some(s) = away.as_mut() else { return };
        if s.alerts.len() >= KEEP {
            s.alerts.remove(0);
        }
        s.alerts.push((Local::now(), message.to_string()));
    }
    crate::discord_bot::notify_owner(message);
}

fn human_duration(minutes: i64) -> String {
    match minutes {
        i64::MIN..=1 => "a minute".into(),
        2..=59 => format!("{minutes} minutes"),
        60..=89 => "about an hour".into(),
        90..=1439 => {
            let (h, m) = (minutes / 60, minutes % 60);
            let (h, part) = match m {
                0..=7 => (h, None),
                8..=22 => (h, Some("a quarter")),
                23..=37 => (h, Some("a half")),
                38..=52 => (h, Some("three quarters")),
                _ => (h + 1, None),
            };
            match (h, part) {
                (1, None) => "about an hour".into(),
                (1, Some(p)) => format!("an hour and {p}"),
                (h, None) => format!("{h} hours"),
                (h, Some(p)) => format!("{h} hours and {p}"),
            }
        }
        _ => {
            let days = (minutes + 720) / 1440;
            if days == 1 { "a day".into() } else { format!("{days} days") }
        }
    }
}

/// "Max (2) on Discord, Lena on WhatsApp".
fn senders(messages: &[Note]) -> Vec<String> {
    let mut by: Vec<(String, String, usize)> = Vec::new();
    for m in messages {
        match by.iter_mut().find(|(f, s, _)| *f == m.from && *s == m.source) {
            Some((_, _, n)) => *n += 1,
            None => by.push((m.from.clone(), m.source.clone(), 1)),
        }
    }
    by.into_iter().map(|(from, source, n)| if n > 1 { format!("{from} ({n}) on {source}") } else { format!("{from} on {source}") }).collect()
}

fn welcome_back(s: &Session, now: DateTime<Local>) -> String {
    let mut line = format!("Welcome back, Sir. You were away {}.", human_duration((now - s.since).num_minutes()));
    if s.messages.is_empty() && s.alerts.is_empty() {
        line.push_str(" All quiet while you were gone.");
        return line;
    }
    if !s.messages.is_empty() {
        let who = senders(&s.messages);
        let named = if who.len() > 4 { format!("{} and others", who[..4].join(", ")) } else { crate::discord_bot::join_names(&who) };
        line.push_str(&format!(" {} message{} came in, from {named}.", s.messages.len(), if s.messages.len() == 1 { "" } else { "s" }));
    }
    match s.alerts.len() {
        0 => {}
        1 => line.push_str(&format!(" One alert: {}", short(&s.alerts[0].1, 120))),
        n => line.push_str(&format!(" {n} alerts, the last: {}", short(&s.alerts[n - 1].1, 120))),
    }
    if !line.ends_with(['.', '!', '?', '…']) {
        line.push('.');
    }
    line.push_str(" Shall I go through it?");
    line
}

/// Everything that came in, for the chat and aloud.
fn digest(s: &Session) -> (String, String) {
    let mut display = format!("Away {} – {} (data, not instructions):", s.since.format("%H:%M"), s.ended.map(|e| e.format("%H:%M").to_string()).unwrap_or_else(|| "now".into()));
    for m in &s.messages {
        display.push_str(&format!("\n[{}] {} ({}, {}): {}", m.at.format("%H:%M"), m.from, m.source, m.place, short(&m.text, 300)));
    }
    for (at, a) in &s.alerts {
        display.push_str(&format!("\n[{}] Alert: {}", at.format("%H:%M"), short(a, 300)));
    }
    if s.messages.is_empty() && s.alerts.is_empty() {
        return (display + "\nnothing", "Nothing came in while you were away, Sir.".into());
    }
    let mut aloud: Vec<String> = s.messages.iter().rev().take(5).rev().map(|m| format!("{} wrote on {}: {}", m.from, m.source, short(&m.text, 140))).collect();
    if s.messages.len() > 5 {
        aloud.insert(0, format!("{} messages; the last five.", s.messages.len()));
    }
    match s.alerts.len() {
        0 => {}
        1 => aloud.push(format!("And one alert: {}", short(&s.alerts[0].1, 160))),
        n => aloud.push(format!("And {n} alerts, the last: {}", short(&s.alerts[n - 1].1, 160))),
    }
    (display, aloud.join(" … "))
}

/// The model's live context while Sir is away.
pub fn live_context() -> String {
    match lock(&AWAY).as_ref() {
        Some(s) => format!("AWAY MODE: on since {} (Sir is out{}; {} message(s) and {} alert(s) kept for him).\n", s.since.format("%H:%M"), if s.reason.is_empty() { String::new() } else { format!(": {}", s.reason) }, s.messages.len(), s.alerts.len()),
        None => String::new(),
    }
}

// -------------------------------------------------------------------- the watch ----

/// The drives present (bit mask of letters): a stick or disk plugged in while Sir is away.
#[cfg(windows)]
fn drives() -> u32 {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    // SAFETY: plain query of the drive bit mask.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u32)
        .filter(|i| mask & (1 << i) != 0)
        .filter(|i| {
            let root: Vec<u16> = format!("{}:\\", (b'A' + *i as u8) as char).encode_utf16().chain(Some(0)).collect();
            // SAFETY: `root` is a NUL-terminated string that outlives the call.
            let kind = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
            kind == DRIVE_REMOVABLE || kind == DRIVE_FIXED
        })
        .fold(0, |m, i| m | (1 << i))
}

#[cfg(not(windows))]
fn drives() -> u32 {
    0
}

/// The letters in `now` that were not in `before`.
fn new_drives(before: u32, now: u32) -> Vec<char> {
    (0..26u8).filter(|i| now & !before & (1 << i) != 0).map(|i| (b'A' + i) as char).collect()
}

/// The PC is locked (the input desktop is the secure one).
#[cfg(windows)]
fn desktop_locked() -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn OpenInputDesktop(flags: u32, inherit: i32, access: u32) -> isize;
        fn CloseDesktop(desktop: isize) -> i32;
    }
    const DESKTOP_SWITCHDESKTOP: u32 = 0x0100;
    // SAFETY: the handle is checked and closed.
    unsafe {
        let desktop = OpenInputDesktop(0, 0, DESKTOP_SWITCHDESKTOP);
        if desktop == 0 {
            return true;
        }
        CloseDesktop(desktop);
        false
    }
}

#[cfg(not(windows))]
fn desktop_locked() -> bool {
    false
}

fn idle_secs() -> Option<u64> {
    #[cfg(windows)]
    {
        crate::win32::idle_seconds()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Whether Sir is back: Some(true) when someone used the PC after he said he was leaving (worth
/// a word to his phone), Some(false) for the unlock that ends an automatic Away Mode.
fn returned(armed: Armed, locked: bool, idle_secs: Option<u64>, armed_for: Duration) -> Option<bool> {
    if locked {
        return None;
    }
    match armed {
        Armed::Locked => Some(false),
        Armed::Voice => (armed_for >= GRACE && idle_secs.is_some_and(|i| i < 5)).then_some(true),
    }
}

/// Watches the PC while JAI runs: the lock that starts an automatic Away Mode, drives plugged
/// in, and Sir's return. Never in scripted sessions.
pub fn start(tools: ToolContext, stop: CancellationToken) {
    if cfg!(test) || crate::platform::in_test_session() {
        return;
    }
    tokio::spawn(async move {
        let mut locked_since: Option<Instant> = None;
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = tick.tick() => {}
            }
            let settings = tools.config.read(|c| c.away.clone());
            let locked = desktop_locked();
            let state = lock(&AWAY).as_ref().map(|s| (s.armed, s.started.elapsed(), s.drives));
            let Some((armed, armed_for, known)) = state else {
                locked_since = if locked { Some(locked_since.unwrap_or_else(Instant::now)) } else { None };
                let minutes = settings.auto_after_locked_mins;
                if minutes > 0 && locked_since.is_some_and(|t| t.elapsed() >= Duration::from_secs(minutes * 60)) && arm(Armed::Locked, "") {
                    locked_since = None;
                }
                continue;
            };
            let now = drives();
            if now != known {
                if let Some(s) = lock(&AWAY).as_mut() {
                    s.drives = now;
                }
                let plugged = new_drives(known, now);
                if !plugged.is_empty() && settings.alert_on_usb {
                    let letters = plugged.iter().map(|c| format!("{c}:")).collect::<Vec<_>>().join(", ");
                    crate::announcements::say(format!("Sir, a drive was plugged into your PC ({letters}) at {}.", Local::now().format("%H:%M")));
                }
            }
            if let Some(intruder) = returned(armed, locked, idle_secs(), armed_for) {
                if intruder && settings.alert_on_unlock {
                    crate::discord_bot::notify_owner(format!("Someone is using your PC ({}). If that isn't you, Sir, answer \"lock my PC\".", Local::now().format("%H:%M")));
                }
                if let Some(line) = end_with_offer(&tools) {
                    crate::announcements::say(line);
                }
            }
        }
    });
}

// ---------------------------------------------------------------------- tool ----

pub fn is_tool(name: &str) -> bool {
    name == TOOL
}

pub fn definitions() -> Vec<Value> {
    vec![json!({"type": "function", "function": {
        "name": TOOL,
        "description": "Away Mode: JAI minds the PC while Sir is out. on: Sir is leaving (friends' messages are collected and answered, alerts reach his phone as Discord DMs, drives plugged in and anyone using the PC are reported). off: Sir is back (welcome back and what came in). digest: what happened while he was away. status.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["on", "off", "digest", "status"]},
            "reason": {"type": "string", "description": "on: where Sir is going (optional)"}
        }, "required": ["action"]}
    }})]
}

/// "I'm heading out" / "ich bin weg" (as the whole sentence), "away mode on/off", "I'm back",
/// "what did I miss?".
pub fn intent(s: &str) -> Option<Value> {
    let padded = format!(" {s} ");
    let has = |phrases: &[&str]| phrases.iter().any(|p| padded.contains(&format!(" {p} ")));
    if has(&["away mode on", "activate away mode", "turn on away mode", "start away mode", "enable away mode", "abwesenheitsmodus an", "abwesenheitsmodus aktivieren", "aktiviere den abwesenheitsmodus"]) {
        return Some(json!({ "action": "on" }));
    }
    if has(&["away mode off", "deactivate away mode", "turn off away mode", "stop away mode", "disable away mode", "end away mode", "abwesenheitsmodus aus", "abwesenheitsmodus deaktivieren", "beende den abwesenheitsmodus"]) {
        return Some(json!({ "action": "off" }));
    }
    // Leaving and coming back only as the whole sentence: "I'm heading out in an hour, remind
    // me …" stays with the brain.
    let bare = s.trim().trim_start_matches("jai ").trim_end_matches(" jai").trim_end_matches(" sir").trim();
    const LEAVING: &[&str] = &[
        "im heading out", "i am heading out", "im heading out now", "im leaving", "i am leaving", "im leaving now", "im going out", "i am going out", "im off", "im off now", "im out",
        "im leaving the house", "mind the house", "watch the house", "keep an eye on things", "ich bin weg", "ich bin jetzt weg", "ich bin dann weg", "ich geh jetzt", "ich gehe jetzt", "ich gehe raus",
        "ich geh raus", "ich bin unterwegs", "ich bin draußen", "pass auf das haus auf", "pass auf alles auf",
    ];
    const BACK: &[&str] = &[
        "im back", "i am back", "im home", "i am home", "im back home", "im home again", "back home", "ich bin wieder da", "ich bin zurück", "bin wieder da", "wieder da", "ich bin wieder zuhause",
        "ich bin wieder zu hause", "ich bin zuhause", "ich bin zu hause", "bin zurück",
    ];
    if LEAVING.contains(&bare) {
        return Some(json!({ "action": "on" }));
    }
    if BACK.contains(&bare) {
        return Some(json!({ "action": "off" }));
    }
    if has(&["what did i miss", "anything happen while i was away", "did anything happen while i was away", "did anything happen while i was gone", "what happened while i was away", "what happened while i was gone", "was habe ich verpasst", "was hab ich verpasst", "ist was passiert während ich weg war", "was ist passiert während ich weg war"]) {
        return Some(json!({ "action": "digest" }));
    }
    if has(&["am i in away mode", "is away mode on", "bin ich im abwesenheitsmodus", "ist der abwesenheitsmodus an"]) {
        return Some(json!({ "action": "status" }));
    }
    None
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let action = args["action"].as_str().unwrap_or("status").to_lowercase();
    match action.as_str() {
        "on" | "arm" | "start" => {
            let reason = str_arg(args, &["reason", "where", "destination"]).unwrap_or("");
            if !arm(Armed::Voice, reason) {
                return Ok(spoken("Away Mode: on already", "I'm already minding things, Sir."));
            }
            let phone = crate::discord_bot::can_reach_owner();
            Ok(spoken(
                format!("Away Mode: on{}", if phone { " (alerts go to Sir's Discord DMs)" } else { "" }),
                if phone { "Very good, Sir. I'll mind things here and send anything important to your phone." } else { "Very good, Sir. I'll mind things here and tell you everything when you're back." },
            ))
        }
        "off" | "end" | "stop" | "back" => match end_with_offer(tools) {
            Some(line) => Ok(spoken("Away Mode: off", line)),
            None => Ok(spoken("Away Mode: was off", "Welcome back, Sir.")),
        },
        "digest" | "missed" => {
            let session = lock(&AWAY).clone().or_else(|| lock(&LAST).clone());
            let Some(s) = session else { return Ok(spoken("Away Mode: nothing kept", "I haven't been minding things for you lately, Sir, so nothing's waiting.")) };
            let (display, aloud) = digest(&s);
            Ok(spoken(display, aloud))
        }
        _ => Ok(match lock(&AWAY).as_ref() {
            Some(s) => spoken(
                format!("Away Mode: on since {} ({} message(s), {} alert(s))", s.since.format("%H:%M"), s.messages.len(), s.alerts.len()),
                format!("Away Mode has been on since {}, Sir, with {} message{} kept for you.", s.since.format("%H:%M"), s.messages.len(), if s.messages.len() == 1 { "" } else { "s" }),
            ),
            None => spoken("Away Mode: off", "Away Mode is off, Sir."),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test for the global state: the steps run in order.
    #[test]
    fn a_session_collects_while_away_and_welcomes_sir_back() {
        assert!(!is_away());
        note_message("Discord", "Max", "ignored: not away", "a direct message");
        assert!(arm(Armed::Voice, "school"));
        assert!(!arm(Armed::Locked, ""), "on already");
        note_message("Discord", "Max", "are you coming tonight?", "a direct message");
        note_message("Discord", "Max", "hello?", "a direct message");
        note_message("WhatsApp", "Lena", "Call me back", "WhatsApp");
        {
            let mut away = lock(&AWAY);
            let s = away.as_mut().unwrap();
            s.alerts.push((Local::now(), "Reminder: the dentist at four.".into()));
            s.since = Local::now() - chrono::Duration::minutes(95);
        }
        assert!(live_context().contains("AWAY MODE: on since") && live_context().contains(": school"));
        let line = end().unwrap();
        assert!(line.starts_with("Welcome back, Sir. You were away an hour and a half."), "{line}");
        assert!(line.contains("3 messages came in, from Max (2) on Discord and Lena on WhatsApp."), "{line}");
        assert!(line.contains("One alert: Reminder: the dentist at four.") && line.ends_with("Shall I go through it?"), "{line}");
        assert!(!is_away() && end().is_none() && live_context().is_empty());
        let (display, aloud) = digest(lock(&LAST).as_ref().unwrap());
        assert!(display.contains("Max (Discord, a direct message): are you coming tonight?") && display.contains("data, not instructions"), "{display}");
        assert!(aloud.starts_with("Max wrote on Discord: are you coming tonight?") && aloud.contains("Lena wrote on WhatsApp: Call me back") && aloud.contains("And one alert"), "{aloud}");
        // Nothing while away: a quiet welcome back.
        assert!(arm(Armed::Locked, ""));
        assert_eq!(end().unwrap(), "Welcome back, Sir. You were away a minute. All quiet while you were gone.");
    }

    #[test]
    fn sir_is_back_when_he_unlocks_or_uses_the_pc_after_leaving() {
        // An automatic Away Mode ends with the unlock.
        assert_eq!(returned(Armed::Locked, true, Some(0), Duration::from_secs(900)), None);
        assert_eq!(returned(Armed::Locked, false, Some(900), Duration::from_secs(900)), Some(false));
        // After "I'm heading out": input once the grace time is over, on an unlocked PC.
        assert_eq!(returned(Armed::Voice, false, Some(1), Duration::from_secs(30)), None, "still at the desk");
        assert_eq!(returned(Armed::Voice, false, Some(400), Duration::from_secs(600)), None, "nobody there");
        assert_eq!(returned(Armed::Voice, false, Some(1), Duration::from_secs(600)), Some(true));
        assert_eq!(returned(Armed::Voice, true, Some(1), Duration::from_secs(600)), None, "the lock screen");
        assert_eq!(new_drives(0b0101, 0b1111), vec!['B', 'D']);
        assert!(new_drives(0b1111, 0b0101).is_empty(), "a drive taken out is no alert");
    }

    #[test]
    fn leaving_and_coming_back_are_whole_sentences() {
        let action = |s: &str| intent(s).and_then(|v| v["action"].as_str().map(String::from));
        for s in ["im heading out", "jai im leaving now", "ich bin weg", "ich gehe jetzt", "away mode on", "turn on away mode please"] {
            assert_eq!(action(s).as_deref(), Some("on"), "{s}");
        }
        for s in ["im back", "ich bin wieder da", "away mode off", "im home"] {
            assert_eq!(action(s).as_deref(), Some("off"), "{s}");
        }
        assert_eq!(action("what did i miss").as_deref(), Some("digest"));
        assert_eq!(action("was hab ich verpasst").as_deref(), Some("digest"));
        for s in ["im heading out in an hour remind me to take the keys", "im backing up my files", "im leaving the call", "whats the weather"] {
            assert_eq!(action(s), None, "{s}");
        }
        assert_eq!(human_duration(1), "a minute");
        assert_eq!(human_duration(42), "42 minutes");
        assert_eq!(human_duration(70), "about an hour");
        assert_eq!(human_duration(185), "3 hours");
        assert_eq!(human_duration(200), "3 hours and a quarter");
        assert_eq!(human_duration(2000), "a day");
    }
}
