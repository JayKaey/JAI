//! Three small things Sir keeps with J.A.I. (2026-11-13): a notebook ("note: …", "what did I
//! note about …"), a sleep timer ("stop the music in 30 minutes") and his own rules ("whenever
//! Minecraft starts, set the music to 20 percent").
//!
//! Notes and rules live in the kit's store. A rule's order is carried out as if Sir had typed
//! it (`StarkEvent::RunMacro`), so a rule can do whatever an order of his can; it is only
//! kept when its order is one J.A.I. knows. The sleep timer lets the master volume sink for a
//! few seconds, pauses every player and puts the volume back where it stood.

use crate::kit::say;
use chrono::Timelike;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid phrase")
}

// ---------------------------------------------------------------------------- notebook ----

const NOTE: &str = "note";
pub const NOTEBOOK: &str = "kit_notebook";

static NOTE_ADD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^\s*(?:(?:jai|jay)[,.]?\s+)?(?:please\s+)?(?:note(?:\s+down)?(?:\s+that)?|take\s+a\s+note(?:\s+that)?|make\s+a\s+note(?:\s+of|\s+that)?|write\s+down(?:\s+that)?|jot\s+down|new\s+note|notiere?(?:\s+dir)?(?:,?\s+dass)?|notiz|neue\s+notiz|schreib\s+(?:dir\s+)?auf(?:,?\s+dass)?)\s*[:,]?\s+(?P<text>.+?)\s*$")
});
static NOTE_SHOW: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:(?:show|open|read)\s+(?:me\s+)?(?:my\s+|the\s+)?(?:notes|notebook)|(?:my\s+)?notebook|what\s+(?:are|is\s+in)\s+my\s+notes|(?:zeig\s+(?:mir\s+)?)?(?:meine\s+notizen|mein\s+notizbuch))$"));
static NOTE_SEARCH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^what\s+did\s+i\s+(?:note|write\s+down|jot\s+down)\s+(?:about|on|for)\s+(.+)$|^(?:search|look\s+in|find\s+in)\s+(?:my\s+)?notes\s+(?:for\s+)?(.+)$|^was\s+habe\s+ich\s+(?:zu|über)\s+(.+?)\s+notiert$")
});
static NOTE_DELETE_LAST: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:delete|remove|scratch)\s+(?:the\s+|my\s+)?last\s+note$|^lösche?\s+die\s+letzte\s+notiz$"));
static NOTE_CLEAR: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:delete|clear|remove)\s+all\s+(?:of\s+)?(?:my\s+)?notes$|^lösche?\s+alle\s+(?:meine\s+)?notizen$"));

/// "Note: buy milk", "show my notes", "what did I note about the exam", "delete the last note".
pub fn notebook_command(s: &str, original: &str) -> Option<Value> {
    if NOTE_SHOW.is_match(s) {
        return Some(json!({"action": "show"}));
    }
    if let Some(c) = NOTE_SEARCH.captures(s) {
        return Some(json!({"action": "show", "query": c.get(1).or(c.get(2)).or(c.get(3))?.as_str().trim()}));
    }
    if NOTE_DELETE_LAST.is_match(s) {
        return Some(json!({"action": "delete_last"}));
    }
    if NOTE_CLEAR.is_match(s) {
        return Some(json!({"action": "clear"}));
    }
    let text = NOTE_ADD.captures(original)?.name("text")?.as_str().trim();
    // ("Note that down" is about something said before, not a note of three words.)
    (text.chars().count() >= 3 && !matches!(crate::ear::words(text).join(" ").as_str(), "that" | "this" | "it" | "down" | "that down" | "this down" | "it down" | "das" | "das auf" | "auf")).then(|| json!({"action": "add", "text": text}))
}

/// The notes as they stand in the block: the newest first, each with its day and time. With
/// `query`, only those that hold all its words.
pub fn notes_text(query: Option<&str>) -> (String, usize) {
    let wanted: Vec<String> = query.map(crate::ear::words).unwrap_or_default();
    let notes: Vec<String> = crate::kit::list(NOTE)
        .into_iter()
        .rev()
        .filter(|(_, v, _)| {
            let text = v["text"].as_str().unwrap_or("").to_lowercase();
            wanted.iter().all(|w| text.contains(w.as_str()))
        })
        .map(|(_, v, at)| format!("{}\n    {}", at.get(..16).unwrap_or(&at), v["text"].as_str().unwrap_or("")))
        .collect();
    let n = notes.len();
    (notes.join("\n\n"), n)
}

pub fn notebook(args: &Value) -> Result<String, String> {
    match args["action"].as_str().unwrap_or("show") {
        "add" => {
            let text = args["text"].as_str().unwrap_or("").trim();
            if text.is_empty() {
                return Err("What shall I note, Sir?".to_string());
            }
            crate::kit::put(NOTE, &crate::kit::stamp(), &json!({"text": text}))?;
            if crate::kit::text_is_up(NOTEBOOK) {
                crate::kit::show_text(NOTEBOOK, "Notebook // your notes", &notes_text(None).0, [460.0, 440.0]);
            }
            Ok(say(format!("Noted: {text}"), "Noted, Sir."))
        }
        "delete_last" => match crate::kit::list(NOTE).pop() {
            Some((key, v, _)) => {
                crate::kit::remove(NOTE, &key);
                if crate::kit::text_is_up(NOTEBOOK) {
                    crate::kit::show_text(NOTEBOOK, "Notebook // your notes", &notes_text(None).0, [460.0, 440.0]);
                }
                Ok(say(format!("Deleted the note: {}", v["text"].as_str().unwrap_or("")), "The last note is gone, Sir."))
            }
            None => Err("There is no note to delete, Sir.".to_string()),
        },
        "clear" => {
            let n = crate::kit::clear(NOTE);
            crate::kit::close_text(NOTEBOOK);
            Ok(say(format!("{n} notes deleted."), if n == 0 { "The notebook was empty, Sir.".to_string() } else { format!("All {n} notes are gone, Sir.") }))
        }
        _ => {
            let query = args["query"].as_str().filter(|q| !q.trim().is_empty());
            let (text, n) = notes_text(query);
            if n == 0 {
                return Ok(match query {
                    Some(q) => say(format!("No note about \"{q}\"."), format!("I find no note about {q}, Sir.")),
                    None => say("The notebook is empty. Say \"note: …\" to keep something.", "Your notebook is empty, Sir."),
                });
            }
            let title = match query {
                Some(q) => format!("Notebook // about {q}"),
                None => "Notebook // your notes".to_string(),
            };
            crate::kit::show_text(NOTEBOOK, &title, &text, [460.0, 440.0]);
            Ok(say(text, format!("{n} note{}, Sir. On the board.", if n == 1 { "" } else { "s" })))
        }
    }
}

// ------------------------------------------------------------------------- sleep timer ----

static SLEEP_SET: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:stop|pause|turn\s+off|switch\s+off)\s+(?:the\s+|my\s+)?(?P<what>music|everything|sound|playback|song|video)\s+(?:in|after)\s+(?P<a>.+)$|^(?:set\s+)?(?:a\s+|the\s+)?sleep\s+timer\s+(?:for\s+|of\s+|to\s+|in\s+)?(?P<b>.+)$|^(?P<what2>music|everything)\s+off\s+in\s+(?P<c>.+)$|^(?:mach\s+)?(?:die\s+)?musik\s+in\s+(?P<d>.+?)\s+aus$|^schlaftimer\s+(?:auf\s+|für\s+)?(?P<e>.+)$")
});
static SLEEP_CANCEL: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:cancel|stop|clear|end|forget)\s+(?:the\s+)?sleep\s+timer$|^sleep\s+timer\s+off$|^schlaftimer\s+(?:aus|abbrechen)$"));
static SLEEP_STATUS: LazyLock<Regex> =
    LazyLock::new(|| re(r"^how\s+(?:long|much\s+time)\s+(?:is\s+)?(?:left\s+)?(?:on|until|till)\s+the\s+sleep\s+timer(?:\s+is\s+up)?$|^sleep\s+timer(?:\s+status)?$|^is\s+(?:a|the)\s+sleep\s+timer\s+(?:set|on|running)$|^wie\s+lange\s+läuft\s+der\s+schlaftimer\s+noch$"));

/// "Stop the music in 30 minutes", "sleep timer 20 minutes", "cancel the sleep timer".
pub fn sleep_command(s: &str) -> Option<Value> {
    if SLEEP_CANCEL.is_match(s) {
        return Some(json!({"action": "cancel"}));
    }
    if SLEEP_STATUS.is_match(s) {
        return Some(json!({"action": "status"}));
    }
    let c = SLEEP_SET.captures(s)?;
    let span = ["a", "b", "c", "d", "e"].iter().find_map(|g| c.name(g))?.as_str();
    let minutes = (crate::listening_lock::duration_in(span)?.as_secs() / 60).clamp(1, 720);
    let what = c.name("what").or(c.name("what2")).map_or("music", |m| if m.as_str() == "everything" { "everything" } else { "music" });
    Some(json!({"action": "set", "minutes": minutes, "what": what}))
}

/// The timer that runs: its number (a newer one or a cancel makes an older one void), when it
/// is up, and what it ends.
static SLEEPING: Mutex<Option<(u64, Instant, String)>> = Mutex::new(None);
/// How long the volume sinks before the players are paused.
const FADE: Duration = Duration::from_secs(12);

fn minutes_words(minutes: u64) -> String {
    match minutes {
        1 => "a minute".to_string(),
        60 => "an hour".to_string(),
        m if m % 60 == 0 => format!("{} hours", m / 60),
        m => format!("{m} minutes"),
    }
}

/// How long the sleep timer has left, in a few words (None: none runs).
pub fn sleep_left() -> Option<String> {
    let (_, due, what) = lock(&SLEEPING).clone()?;
    let left = due.saturating_duration_since(Instant::now()).as_secs();
    Some(format!("{} off in {}", if what == "everything" { "everything" } else { "music" }, if left < 90 { format!("{left} s") } else { format!("{} min", left.div_ceil(60)) }))
}

pub fn sleep_timer(args: &Value) -> Result<String, String> {
    match args["action"].as_str().unwrap_or("status") {
        "cancel" => Ok(match lock(&SLEEPING).take() {
            Some(_) => say("Sleep timer cancelled.", "The sleep timer is off, Sir."),
            None => say("No sleep timer was running.", "No sleep timer was set, Sir."),
        }),
        "set" => {
            let minutes = args["minutes"].as_u64().unwrap_or(30).clamp(1, 720);
            let what = if args["what"] == "everything" { "everything" } else { "music" }.to_string();
            let id = crate::kit::stamp().parse::<u64>().unwrap_or(0);
            let due = Instant::now() + Duration::from_secs(minutes * 60);
            *lock(&SLEEPING) = Some((id, due, what.clone()));
            tracing::info!("[SLEEP TIMER] Set: {what} off in {minutes} min");
            let ends = what.clone();
            tokio::spawn(async move {
                tokio::time::sleep_until(tokio::time::Instant::from_std(due.checked_sub(FADE).unwrap_or(due))).await;
                if lock(&SLEEPING).as_ref().is_none_or(|(now, _, _)| *now != id) {
                    return;
                }
                fall_silent(&ends).await;
                let mut running = lock(&SLEEPING);
                if running.as_ref().is_some_and(|(now, _, _)| *now == id) {
                    *running = None;
                }
            });
            let span = minutes_words(minutes);
            Ok(say(format!("Sleep timer: {what} off in {span}."), if what == "everything" { format!("Everything goes quiet in {span}, Sir.") } else { format!("The music stops in {span}, Sir.") }))
        }
        _ => Ok(match sleep_left() {
            Some(left) => say(format!("Sleep timer: {left}."), format!("The sleep timer says {left}, Sir.")),
            None => say("No sleep timer is running.", "No sleep timer is set, Sir."),
        }),
    }
}

/// The timer is up: the volume sinks, every player is paused (with "everything", J.A.I.'s own
/// voice too), and the volume stands again where it stood. A test touches nothing.
async fn fall_silent(what: &str) {
    if crate::platform::hands_off() {
        tracing::info!("[SLEEP TIMER] Up: a test session leaves Sir's players and his volume alone");
        return;
    }
    #[cfg(windows)]
    let stood = crate::win32::call(crate::win32::get_volume).await.ok().and_then(Result::ok).map(|(level, _)| level);
    #[cfg(windows)]
    if let Some(level) = stood.filter(|l| *l > 0.02) {
        let steps = 24;
        for k in 1..=steps {
            let to = level * (1.0 - 0.9 * k as f32 / steps as f32);
            let _ = crate::win32::call(move || crate::win32::set_volume(to)).await;
            tokio::time::sleep(FADE / steps).await;
        }
    }
    let stopped = crate::music::stop_all_active_media().await;
    let _ = what;
    #[cfg(windows)]
    if let Some(level) = stood {
        tokio::time::sleep(Duration::from_millis(600)).await;
        let _ = crate::win32::call(move || crate::win32::set_volume(level)).await;
    }
    tracing::info!("[SLEEP TIMER] Up: {} player(s) paused, the volume stands where it stood", stopped.sessions.len() + stopped.windows.len());
    crate::hud_notify::record("The sleep timer was up: I paused what was playing.", false);
}

// ----------------------------------------------------------------------- whenever-rules ----

const RULE: &str = "rule";
pub const RULES: &str = "kit_rules";

/// When a rule fires.
#[derive(Debug, Clone, PartialEq)]
pub enum When {
    Starts(String),
    Closes(String),
    /// Every day at this hour and minute.
    At(u32, u32),
}

impl When {
    fn words(&self) -> String {
        match self {
            When::Starts(app) => format!("whenever {app} starts"),
            When::Closes(app) => format!("whenever {app} closes"),
            When::At(h, m) => format!("every day at {h:02}:{m:02}"),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            When::Starts(app) => json!({"kind": "starts", "app": app}),
            When::Closes(app) => json!({"kind": "closes", "app": app}),
            When::At(h, m) => json!({"kind": "at", "hour": h, "minute": m}),
        }
    }

    fn of(v: &Value) -> Option<When> {
        match v["kind"].as_str()? {
            "starts" => Some(When::Starts(v["app"].as_str()?.to_string())),
            "closes" => Some(When::Closes(v["app"].as_str()?.to_string())),
            "at" => Some(When::At(v["hour"].as_u64()? as u32, v["minute"].as_u64()? as u32)),
            _ => None,
        }
    }
}

static RULE_LIST: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:list|show)\s+(?:me\s+)?(?:my\s+|the\s+|all\s+)?rules$|^what\s+rules\s+(?:do\s+(?:i|you)\s+have|are\s+there)$|^(?:zeig\s+(?:mir\s+)?)?meine\s+regeln$"));
static RULE_REMOVE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:delete|remove|forget|drop)\s+(?:the\s+|my\s+)?rule\s+(?:number\s+)?(?P<n>\d+)$|^(?:delete|remove|forget|drop)\s+(?:the\s+|my\s+)?rule\s+(?:about|for|with)\s+(?P<about>.+)$|^(?:delete|remove|forget)\s+(?P<all>all)\s+(?:my\s+)?rules$|^lösche?\s+regel\s+(?P<n2>\d+)$")
});
static RULE_WHEN: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:whenever|every\s+time|each\s+time|when|as\s+soon\s+as|immer\s+wenn|jedes\s+mal\s+wenn|sobald|wenn)\s+(.+)$"));
static RULE_SUBJECT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?P<app>.+?)\s+(?:(?P<starts>starts|opens|launches|is\s+started|is\s+opened|comes\s+up|is\s+running|startet|aufgeht|geöffnet\s+wird|läuft)|(?P<closes>closes|quits|exits|ends|is\s+closed|shuts\s+down|zugeht|geschlossen\s+wird|beendet\s+wird))\s+(?:then\s+|dann\s+)?(?P<order>.+)$")
});
static RULE_I: LazyLock<Regex> = LazyLock::new(|| re(r"^i\s+(?:(?P<starts>start|open|launch|run)|(?P<closes>close|quit|exit|leave))\s+(?P<rest>.+)$"));
static RULE_DAILY: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:every\s+day|daily|each\s+day|jeden\s+tag|täglich)\s+(?:at|um)\s+(?P<h>\d{1,2})(?:\s*[:.]?\s*(?P<m>\d{2}))?\s*(?P<half>am|pm)?(?:\s+uhr)?\s+(?:then\s+|dann\s+)?(?P<order>.+)$"));

/// Whether `name` is exactly what a program J.A.I. knows of is called (a built-in one by its
/// name or one of its other names, or an installed one).
fn known_app(name: &str) -> bool {
    crate::apps::find_spec(name).is_some_and(|spec| spec.id == name || spec.display.eq_ignore_ascii_case(name) || spec.aliases.contains(&name)) || crate::appindex::is_known(name)
}

/// "I start <app> <order>" has no comma once it is normalised: the app is the longest run of
/// words that names a program he knows (one word when none does).
fn split_app(rest: &str) -> Option<(String, String)> {
    let words: Vec<&str> = rest.split_whitespace().filter(|w| !matches!(*w, "then" | "dann")).collect();
    let words: Vec<&str> = if matches!(words.first(), Some(&("the" | "my" | "a"))) { words[1..].to_vec() } else { words };
    if words.len() < 2 {
        return None;
    }
    let take = (1..=words.len().saturating_sub(1).min(3)).rev().find(|n| known_app(&words[..*n].join(" "))).unwrap_or(1);
    Some((words[..take].join(" "), words[take..].join(" ")))
}

/// The rule in a sentence: when it fires and what is then to be done.
pub fn parse_rule(s: &str) -> Option<(When, String)> {
    if let Some(c) = RULE_DAILY.captures(s) {
        let mut h: u32 = c.name("h")?.as_str().parse().ok()?;
        let m: u32 = c.name("m").map_or(Some(0), |m| m.as_str().parse().ok())?;
        match c.name("half").map(|m| m.as_str()) {
            Some("pm") if h < 12 => h += 12,
            Some("am") if h == 12 => h = 0,
            _ => {}
        }
        return (h < 24 && m < 60).then(|| (When::At(h, m), c.name("order").map_or("", |m| m.as_str()).trim().to_string()));
    }
    let rest = RULE_WHEN.captures(s)?.get(1)?.as_str();
    if let Some(c) = RULE_I.captures(rest) {
        let (app, order) = split_app(c.name("rest")?.as_str())?;
        return Some((if c.name("starts").is_some() { When::Starts(app) } else { When::Closes(app) }, order));
    }
    let c = RULE_SUBJECT.captures(rest)?;
    let app = c.name("app")?.as_str().trim_start_matches("the ").trim_start_matches("my ").trim().to_string();
    let order = c.name("order")?.as_str().trim().to_string();
    (!app.is_empty() && app.split(' ').count() <= 3).then(|| (if c.name("starts").is_some() { When::Starts(app) } else { When::Closes(app) }, order))
}

/// "Whenever Minecraft starts, set the music to 20 percent", "every day at 7:30 open my
/// calendar", "list my rules", "delete rule 2", "delete the rule about Minecraft".
pub fn rule_command(s: &str) -> Option<Value> {
    if RULE_LIST.is_match(s) {
        return Some(json!({"action": "list"}));
    }
    if let Some(c) = RULE_REMOVE.captures(s) {
        if c.name("all").is_some() {
            return Some(json!({"action": "remove", "all": true}));
        }
        return Some(match c.name("n").or(c.name("n2")) {
            Some(n) => json!({"action": "remove", "number": n.as_str().parse::<u64>().ok()?}),
            None => json!({"action": "remove", "about": c.name("about")?.as_str().trim()}),
        });
    }
    let (when, order) = parse_rule(s)?;
    // (Only what he can carry out is a rule: "when Discord closes I feel lonely" is talk.)
    (!order.is_empty() && crate::ear::is_order(&order)).then(|| json!({"action": "add", "when": when.to_json(), "then": order}))
}

fn rules() -> Vec<(String, When, String)> {
    crate::kit::list(RULE).into_iter().filter_map(|(key, v, _)| Some((key, When::of(&v["when"])?, v["then"].as_str()?.to_string()))).collect()
}

fn rules_text() -> String {
    let all = rules();
    if all.is_empty() {
        return "No rules yet. Say, for example: \"whenever Minecraft starts, set the music to 20 percent\".".to_string();
    }
    let lines: Vec<String> = all.iter().enumerate().map(|(k, (_, when, then))| format!("{}.  {}\n    → {then}", k + 1, capitalised(&when.words()))).collect();
    format!("{}\n\n\"Delete rule 2\" removes one.", lines.join("\n\n"))
}

fn capitalised(text: &str) -> String {
    let mut c = text.chars();
    c.next().map_or(String::new(), |first| first.to_uppercase().chain(c).collect())
}

pub fn rule(args: &Value) -> Result<String, String> {
    let refresh = || {
        if crate::kit::text_is_up(RULES) {
            crate::kit::show_text(RULES, "Rules // whenever … then …", &rules_text(), [500.0, 380.0]);
        }
    };
    match args["action"].as_str().unwrap_or("list") {
        "add" => {
            let when = When::of(&args["when"]).ok_or("I did not catch when the rule should hold, Sir.")?;
            let then = args["then"].as_str().unwrap_or("").trim().to_string();
            if then.is_empty() {
                return Err("And what shall I do then, Sir?".to_string());
            }
            if rules().iter().any(|(_, w, t)| *w == when && *t == then) {
                return Ok(say("That rule is kept already.", "I have that rule already, Sir."));
            }
            crate::kit::put(RULE, &crate::kit::stamp(), &json!({"when": when.to_json(), "then": then}))?;
            refresh();
            tracing::info!("[RULES] Kept: {} → {then}", when.words());
            Ok(say(format!("Rule kept: {} → {then}", when.words()), format!("Understood, Sir. {}, I'll {then}.", capitalised(&when.words()))))
        }
        "remove" => {
            let all = rules();
            let gone: Vec<&(String, When, String)> = if args["all"] == true {
                all.iter().collect()
            } else if args["last"] == true {
                all.last().into_iter().collect()
            } else if let Some(n) = args["number"].as_u64() {
                all.get((n as usize).wrapping_sub(1)).into_iter().collect()
            } else {
                let about = args["about"].as_str().unwrap_or("").to_lowercase();
                all.iter().filter(|(_, when, then)| !about.is_empty() && (when.words().to_lowercase().contains(&about) || then.to_lowercase().contains(&about))).collect()
            };
            if gone.is_empty() {
                return Err("I have no such rule, Sir. \"List my rules\" shows them with their numbers.".to_string());
            }
            for (key, _, _) in &gone {
                crate::kit::remove(RULE, key);
            }
            refresh();
            Ok(say(format!("{} rule(s) removed.", gone.len()), if gone.len() == 1 { "That rule is gone, Sir.".to_string() } else { format!("{} rules are gone, Sir.", gone.len()) }))
        }
        _ => {
            let n = rules().len();
            let text = rules_text();
            crate::kit::show_text(RULES, "Rules // whenever … then …", &text, [500.0, 380.0]);
            Ok(say(text, if n == 0 { "You have no rules yet, Sir.".to_string() } else { format!("{n} rule{}, Sir. On the board.", if n == 1 { "" } else { "s" }) }))
        }
    }
}

/// Whether the running program `process` (lower case, without ".exe") is the app a rule names.
fn is_app(app: &str, process: &str) -> bool {
    let named = app.to_lowercase();
    if let Some(spec) = crate::apps::find_spec(&named) {
        if spec.processes.contains(&process) {
            return true;
        }
    }
    let compact: String = named.chars().filter(|c| c.is_alphanumeric()).collect();
    let running: String = process.chars().filter(|c| c.is_alphanumeric()).collect();
    compact.len() >= 3 && (running == compact || (compact.len() >= 5 && running.contains(&compact)) || (named == "minecraft" && running == "javaw"))
}

/// The orders of the rules that fire: for the programs that have just started and closed, and
/// for the minute of the day when it has just begun (`minute`: Some((hour, minute))).
pub fn fired(started: &[String], closed: &[String], minute: Option<(u32, u32)>) -> Vec<(String, String)> {
    rules()
        .into_iter()
        .filter(|(_, when, _)| match when {
            When::Starts(app) => started.iter().any(|p| is_app(app, p)),
            When::Closes(app) => closed.iter().any(|p| is_app(app, p)),
            When::At(h, m) => minute == Some((*h, *m)),
        })
        .map(|(_, when, then)| (when.words(), then))
        .collect()
}

/// Watches what starts and closes and the clock, and has the rules' orders carried out as if
/// Sir had typed them. (A test and a scripted session run no rules.)
pub fn start(tools: crate::tools::ToolContext, stop: tokio_util::sync::CancellationToken) {
    if cfg!(test) || crate::platform::in_test_session() {
        return;
    }
    tokio::spawn(async move {
        use sysinfo::{ProcessesToUpdate, System};
        let mut system = System::new();
        let mut running: Option<std::collections::HashSet<String>> = None;
        let mut last_minute = None;
        let mut ticks = 0u64;
        let mut tick = tokio::time::interval(Duration::from_secs(4));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = tick.tick() => {}
            }
            // (What the dashboard on the upright monitor shows, while it is on; and, once a
            // minute, the names of the desk layouts for the fast path.)
            crate::kit_screen::gather(&tools);
            if ticks % 15 == 0 {
                crate::kit_screen::sync_layouts(&tools);
            }
            ticks += 1;
            let all = rules();
            if all.is_empty() {
                running = None;
                continue;
            }
            let (mut started, mut closed) = (Vec::new(), Vec::new());
            if all.iter().any(|(_, when, _)| !matches!(when, When::At(..))) {
                system.refresh_processes(ProcessesToUpdate::All);
                let now: std::collections::HashSet<String> = system.processes().values().map(|p| p.name().to_string_lossy().to_lowercase().trim_end_matches(".exe").to_string()).collect();
                // (The first look only learns what runs: a rule fires on a change, not on a start of his own.)
                if let Some(before) = &running {
                    started = now.difference(before).cloned().collect();
                    closed = before.difference(&now).cloned().collect();
                }
                running = Some(now);
            }
            let clock = chrono::Local::now();
            let minute = (clock.hour(), clock.minute());
            let new_minute = (last_minute != Some(minute)).then_some(minute);
            last_minute = Some(minute);
            for (when, then) in fired(&started, &closed, new_minute) {
                tracing::info!("[RULES] {when}: {then}");
                crate::hud_notify::record(&format!("Rule: {when} → {then}"), false);
                let _ = tools.stark.events.send(crate::stark::StarkEvent::RunMacro { name: format!("rule ({when})"), steps: vec![then] });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> String {
        crate::ear::words(text).join(" ")
    }

    #[test]
    fn notes_are_taken_shown_searched_and_deleted() {
        let _one = crate::kit::test_lock();
        let said = "Jay, note: Maths test on Friday, chapters 3 and 4.";
        assert_eq!(notebook_command(&plain(said), said), Some(json!({"action": "add", "text": "Maths test on Friday, chapters 3 and 4."})), "the note keeps its capitals and commas");
        for (said, text) in [("Take a note that the bike needs air", "the bike needs air"), ("write down buy milk", "buy milk"), ("Notiere dir, dass Oma am Sonntag kommt", "Oma am Sonntag kommt"), ("Notiz: Zahnarzt anrufen", "Zahnarzt anrufen")] {
            assert_eq!(notebook_command(&plain(said), said), Some(json!({"action": "add", "text": text})), "{said}");
        }
        assert_eq!(notebook_command("show my notes", "x"), Some(json!({"action": "show"})));
        assert_eq!(notebook_command("what did i note about the test", "x"), Some(json!({"action": "show", "query": "the test"})));
        assert_eq!(notebook_command("delete the last note", "x"), Some(json!({"action": "delete_last"})));
        for said in ["note that down", "what is a note", "play a high note", "I noted nothing"] {
            assert_eq!(notebook_command(&plain(said), said), None, "{said}");
        }
        // Kept, found by their words, and gone on his word.
        assert!(notebook(&json!({"action": "show"})).unwrap().contains("empty"));
        notebook(&json!({"action": "add", "text": "Maths test on Friday"})).unwrap();
        notebook(&json!({"action": "add", "text": "Buy milk"})).unwrap();
        assert_eq!(notes_text(None).1, 2);
        let (found, n) = notes_text(Some("maths friday"));
        assert!(n == 1 && found.contains("Maths test on Friday"), "{found}");
        assert!(notes_text(None).0.find("Buy milk") < notes_text(None).0.find("Maths test"), "the newest first");
        assert!(notebook(&json!({"action": "delete_last"})).unwrap().contains("Buy milk"));
        assert!(notebook(&json!({"action": "clear"})).unwrap().contains("1 notes deleted"));
        assert!(notebook(&json!({"action": "delete_last"})).is_err());
        crate::kit::close_text(NOTEBOOK);
    }

    #[test]
    fn the_sleep_timer_is_set_asked_about_and_cancelled() {
        for (said, minutes, what) in [
            ("stop the music in 30 minutes", 30, "music"),
            ("turn off everything in an hour", 60, "everything"),
            ("sleep timer 20 minutes", 20, "music"),
            ("set a sleep timer for half an hour", 30, "music"),
            ("everything off in 10 minutes", 10, "everything"),
            ("mach die musik in 15 minuten aus", 15, "music"),
        ] {
            assert_eq!(sleep_command(said), Some(json!({"action": "set", "minutes": minutes, "what": what})), "{said}");
        }
        assert_eq!(sleep_command("cancel the sleep timer"), Some(json!({"action": "cancel"})));
        assert_eq!(sleep_command("how long is left on the sleep timer"), Some(json!({"action": "status"})));
        for said in ["stop the music", "stop the music in the kitchen", "sleep", "set a timer for 20 minutes"] {
            assert_eq!(sleep_command(said), None, "{said}");
        }
        // Stopping the music at once stays what it was.
        assert!(crate::intents::is_media_stop("stop the music") && !crate::intents::is_media_stop("stop the music in 30 minutes"));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            *lock(&SLEEPING) = None;
            assert!(sleep_timer(&json!({"action": "status"})).unwrap().contains("No sleep timer"));
            assert!(sleep_timer(&json!({"action": "set", "minutes": 30, "what": "music"})).unwrap().contains("in 30 minutes"));
            assert_eq!(sleep_left().as_deref(), Some("music off in 30 min"));
            assert!(sleep_timer(&json!({"action": "cancel"})).unwrap().contains("cancelled"));
            assert_eq!(sleep_left(), None);
        });
        assert_eq!((minutes_words(1), minutes_words(60), minutes_words(120), minutes_words(45)), ("a minute".to_string(), "an hour".to_string(), "2 hours".to_string(), "45 minutes".to_string()));
    }

    #[test]
    fn a_rule_says_when_and_what_and_fires_on_the_change() {
        let _one = crate::kit::test_lock();
        assert_eq!(parse_rule("whenever discord starts mute the music"), Some((When::Starts("discord".into()), "mute the music".into())));
        assert_eq!(parse_rule("when i open spotify set the volume to 30"), Some((When::Starts("spotify".into()), "set the volume to 30".into())));
        assert_eq!(parse_rule("every time steam closes then open the mixer"), Some((When::Closes("steam".into()), "open the mixer".into())));
        assert_eq!(parse_rule("every day at 7 30 open my calendar"), Some((When::At(7, 30), "open my calendar".into())));
        assert_eq!(parse_rule("every day at 9 pm night mode"), Some((When::At(21, 0), "night mode".into())));
        assert_eq!(parse_rule("when is my next lesson"), None);
        assert_eq!(parse_rule("whenever i feel like it"), None);
        // Only an order he knows makes a rule.
        assert_eq!(rule_command("whenever discord starts open the mixer"), Some(json!({"action": "add", "when": {"kind": "starts", "app": "discord"}, "then": "open the mixer"})));
        assert_eq!(rule_command("whenever discord closes i feel a little lonely"), None);
        assert_eq!(rule_command("list my rules"), Some(json!({"action": "list"})));
        assert_eq!(rule_command("delete rule 2"), Some(json!({"action": "remove", "number": 2})));
        assert_eq!(rule_command("delete the rule about discord"), Some(json!({"action": "remove", "about": "discord"})));
        // Kept, fired by what starts and closes and by the clock, and removed.
        rule(&json!({"action": "add", "when": {"kind": "starts", "app": "discord"}, "then": "open the mixer"})).unwrap();
        rule(&json!({"action": "add", "when": {"kind": "closes", "app": "spotify"}, "then": "night mode"})).unwrap();
        rule(&json!({"action": "add", "when": {"kind": "at", "hour": 7, "minute": 30}, "then": "open my calendar"})).unwrap();
        assert!(rule(&json!({"action": "add", "when": {"kind": "starts", "app": "discord"}, "then": "open the mixer"})).unwrap().contains("already"));
        assert_eq!(fired(&["discord".into()], &[], None), [("whenever discord starts".to_string(), "open the mixer".to_string())]);
        assert_eq!(fired(&["notepad".into()], &["spotify".into()], None).len(), 1);
        assert_eq!(fired(&[], &[], Some((7, 30))), [("every day at 07:30".to_string(), "open my calendar".to_string())]);
        assert!(fired(&[], &[], Some((7, 31))).is_empty() && fired(&["discordptb".into()], &[], None).len() == 1);
        assert!(is_app("minecraft", "javaw") && is_app("Discord", "discord") && !is_app("discord", "disk"));
        assert!(rule(&json!({"action": "list"})).unwrap().contains("1.  Whenever discord starts"));
        assert!(rule(&json!({"action": "remove", "number": 1})).is_ok());
        assert!(rule(&json!({"action": "remove", "about": "calendar"})).is_ok());
        assert_eq!(rules().len(), 1);
        assert!(rule(&json!({"action": "remove", "number": 9})).is_err());
        assert!(rule(&json!({"action": "remove", "all": true})).unwrap().contains("1 rule"));
        crate::kit::close_text(RULES);
    }
}
