//! Reminders in plain English or German: "remind me in 10 minutes to check the oven", "remind
//! me at 6 pm to call Mum", "erinnere mich morgen um 9 an den Zahnarzt". Persisted in
//! `jarvis_memory.db`, shown as a live "Reminders" widget in the HUD, and announced as alerts
//! when due (also the ones that fell due while Jarvis was offline).

use crate::widgets::WidgetEngine;
use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveTime, TimeZone, Timelike};
use regex::Regex;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{info, warn};

const WIDGET_ID: &str = "reminders";

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid reminder regex")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reminder {
    pub id: i64,
    pub text: String,
    pub due_ms: i64,
}

impl Reminder {
    pub fn due(&self) -> DateTime<Local> {
        Local.timestamp_millis_opt(self.due_ms).single().unwrap_or_else(Local::now)
    }
}

/// "at 18:30", "tomorrow at 09:00", "on Friday at 14:00".
pub fn spoken_when(due: DateTime<Local>, now: DateTime<Local>) -> String {
    let time = due.format("%H:%M");
    let days = (due.date_naive() - now.date_naive()).num_days();
    match days {
        0 => format!("at {time}"),
        1 => format!("tomorrow at {time}"),
        2..=6 => format!("on {} at {time}", due.format("%A")),
        _ => format!("on {} at {time}", due.format("%-d %B")),
    }
}

/// "in 10 minutes", "in 2 hours and 5 minutes".
pub fn spoken_in(due: DateTime<Local>, now: DateTime<Local>) -> String {
    spoken_duration(((due - now).num_milliseconds().max(0) + 500) / 1000)
}

/// "10 minutes", "2 hours and 5 minutes", "45 seconds" (59 s reads as a minute: the clock
/// has moved on a little since Sir asked).
pub fn spoken_duration(secs: i64) -> String {
    let secs = if (55..60).contains(&secs) { 60 } else { secs.max(0) };
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    let unit = |n: i64, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    match (h, m) {
        (0, 0) => unit(s.max(1), "second"),
        (0, m) => unit(m + i64::from(s >= 30), "minute"),
        (h, 0) => unit(h, "hour"),
        (h, m) => format!("{} and {}", unit(h, "hour"), unit(m, "minute")),
    }
}

/// Sir's own words turned towards him: "stretch my legs" → "stretch your legs".
pub fn addressed_to_sir(text: &str) -> String {
    text.split(' ')
        .map(|w| {
            let (core, tail) = w.split_at(w.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '\'').len());
            let swapped = match core.to_lowercase().as_str() {
                "my" => "your",
                "me" => "you",
                "i" => "you",
                "mine" => "yours",
                "myself" => "yourself",
                "i'm" | "im" => "you're",
                "i've" => "you've",
                "i'll" => "you'll",
                "am" => "are",
                "our" => "your",
                _ => return w.to_string(),
            };
            format!("{swapped}{tail}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// --------------------------------------------------------------------------- parsing ----

fn number(word: &str) -> Option<f64> {
    if let Ok(n) = word.replace(',', ".").parse::<f64>() {
        return Some(n);
    }
    Some(match word {
        "a" | "an" | "one" | "ein" | "eine" | "einer" | "einem" | "einen" | "eins" => 1.0,
        "two" | "zwei" | "couple" => 2.0,
        "three" | "drei" => 3.0,
        "four" | "vier" => 4.0,
        "five" | "fünf" | "fuenf" => 5.0,
        "six" | "sechs" => 6.0,
        "seven" | "sieben" => 7.0,
        "eight" | "acht" => 8.0,
        "nine" | "neun" => 9.0,
        "ten" | "zehn" => 10.0,
        "eleven" | "elf" => 11.0,
        "twelve" | "zwölf" | "zwoelf" => 12.0,
        "fifteen" | "fünfzehn" => 15.0,
        "twenty" | "zwanzig" => 20.0,
        "thirty" | "dreißig" | "dreissig" => 30.0,
        "forty" | "vierzig" => 40.0,
        "forty five" | "fünfundvierzig" => 45.0,
        "fifty" | "fünfzig" => 50.0,
        "sixty" | "sechzig" => 60.0,
        "ninety" | "neunzig" => 90.0,
        "anderthalb" | "eineinhalb" => 1.5,
        _ => return None,
    })
}

fn unit_secs(unit: &str) -> Option<i64> {
    let u = unit.trim_end_matches('s');
    Some(match u {
        "second" | "sec" | "sekunde" | "sekunden" => 1,
        "minute" | "min" | "minuten" => 60,
        "hour" | "hr" | "stunde" | "stunden" => 3600,
        "day" | "tag" | "tage" | "tagen" => 86_400,
        "week" | "woche" | "wochen" => 604_800,
        _ => return None,
    })
}

static RELATIVE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\bin\s+(?:(?P<half>half an hour|a half hour|einer halben stunde|einer halbe stunde|halben stunde|a quarter of an hour|a quarter hour|einer viertelstunde|viertelstunde)|(?P<n1>[\d.,]+|a|an|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|fifteen|twenty|thirty|forty|fifty|sixty|ninety|couple of|ein|eine|einer|einem|zwei|drei|vier|fünf|sechs|sieben|acht|neun|zehn|elf|zwölf|zwanzig|dreißig|vierzig|fünfzig|sechzig|anderthalb|eineinhalb)\s+(?P<u1>seconds?|secs?|minutes?|mins?|hours?|hrs?|days?|weeks?|sekunden?|minuten?|stunden?|tagen?|tage|wochen?)(?:\s+(?:and|und)\s+(?:a\s+half|(?P<n2>[\d.,]+|a|an|one|two|three|four|five|ten|fifteen|twenty|thirty|forty|fifty|ein|eine|einer|zwei|drei|fünf|zehn|fünfzehn|zwanzig|dreißig|vierzig|fünfzig)\s+(?P<u2>seconds?|minutes?|mins?|sekunden?|minuten?)))?)\b")
});
static ABSOLUTE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?P<day>tomorrow|morgen|today|heute|tonight|heute abend)?\s*(?:at|um|for|gegen)\s+(?P<h>\d{1,2})(?:[:.](?P<m>\d{2}))?\s*(?P<ampm>a\.?m\.?|p\.?m\.?|uhr|o'?clock)?\b(?:\s+(?P<day2>tomorrow|morgen|today|heute))?")
});
static NAMED_TIME: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?P<day>tomorrow|morgen)?\s*(?:at\s+|um\s+|am\s+)?(?P<name>noon|midday|mittag|mittags|midnight|mitternacht|tonight|heute abend|tomorrow morning|morgen früh|morgen frueh)\b")
});
static REQUEST: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:jarvis\s*,?\s*)?(?:please\s+|bitte\s+|can you\s+|could you\s+|kannst du\s+|kannst du mich\s+)?(?:remind me|set a reminder|set reminder|create a reminder|add a reminder|erinnere mich|erinner mich|erinnere|stell eine erinnerung|erstelle eine erinnerung)\b(?P<rest>.*)$")
});

/// When a time expression in `text` points to, and the text with that expression removed.
pub fn parse_when(text: &str, now: DateTime<Local>) -> Option<(DateTime<Local>, String)> {
    let lower = text.to_lowercase();
    if let Some(c) = RELATIVE.captures(&lower) {
        let mut secs = if c.name("half").is_some() {
            let w = &c["half"];
            if w.contains("quarter") || w.contains("viertel") { 900 } else { 1800 }
        } else {
            let n = number(c.name("n1")?.as_str().trim_end_matches(" of"))?;
            let mut s = (n * unit_secs(c.name("u1")?.as_str())? as f64) as i64;
            if c.get(0)?.as_str().contains("and a half") {
                s += unit_secs(c.name("u1")?.as_str())? / 2;
            }
            if let (Some(n2), Some(u2)) = (c.name("n2"), c.name("u2")) {
                s += (number(n2.as_str())? * unit_secs(u2.as_str())? as f64) as i64;
            }
            s
        };
        secs = secs.max(1);
        let m = c.get(0)?;
        let rest = format!("{} {}", &lower[..m.start()], &lower[m.end()..]);
        return Some((now + ChronoDuration::seconds(secs), rest));
    }
    let (time, day_word, span) = if let Some(c) = ABSOLUTE.captures(&lower) {
        let mut h: u32 = c["h"].parse().ok()?;
        let m: u32 = c.name("m").map_or(Some(0), |m| m.as_str().parse().ok())?;
        let ampm = c.name("ampm").map(|a| a.as_str().replace('.', "")).unwrap_or_default();
        if ampm.starts_with('p') && h < 12 {
            h += 12;
        } else if ampm.starts_with('a') && h == 12 {
            h = 0;
        }
        let day = c.name("day").or(c.name("day2")).map(|d| d.as_str().to_string()).unwrap_or_default();
        // "at 7" with nothing else in the evening: the next 7 o'clock.
        if (day == "tonight" || day == "heute abend") && h < 12 {
            h += 12;
        }
        (NaiveTime::from_hms_opt(h, m, 0)?, day, c.get(0)?.range())
    } else {
        let c = NAMED_TIME.captures(&lower)?;
        let (h, tomorrow) = match &c["name"] {
            "noon" | "midday" | "mittag" | "mittags" => (12, false),
            "midnight" | "mitternacht" => (0, true),
            "tonight" | "heute abend" => (20, false),
            _ => (8, true),
        };
        let day = if tomorrow || c.name("day").is_some() { "tomorrow".to_string() } else { String::new() };
        (NaiveTime::from_hms_opt(h, 0, 0)?, day, c.get(0)?.range())
    };
    let mut date = now.date_naive();
    if matches!(day_word.as_str(), "tomorrow" | "morgen") {
        date += ChronoDuration::days(1);
    }
    let mut due = Local.from_local_datetime(&date.and_time(time)).single()?;
    if due <= now && !matches!(day_word.as_str(), "tomorrow" | "morgen") {
        due += ChronoDuration::days(1);
    }
    let rest = format!("{} {}", &lower[..span.start], &lower[span.end..]);
    Some((due, rest))
}

/// What to be reminded of, from what is left once the time is removed.
fn clean_message(rest: &str, original: &str) -> String {
    let mut s = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    s = s.trim_matches(|c: char| c == ',' || c == '.' || c == '!' || c == '?' || c.is_whitespace()).to_string();
    for _ in 0..3 {
        for prefix in ["to ", "that ", "about ", "of ", "an ", "daran, ", "daran ", "dass ", "zu ", "please ", "bitte ", "me to ", "mich an ", "mich "] {
            if let Some(r) = s.strip_prefix(prefix) {
                s = r.trim().to_string();
            }
        }
        for suffix in [" please", " bitte", " sir", " jarvis"] {
            if let Some(r) = s.strip_suffix(suffix) {
                s = r.trim().to_string();
            }
        }
    }
    let s = s.trim_matches(|c: char| c == ',' || c.is_whitespace()).to_string();
    // Restore Sir's capitalisation where the text is still found in the original.
    let restored = original
        .to_lowercase()
        .find(&s)
        .and_then(|i| original.get(i..i + s.len()))
        .map(str::to_string)
        .unwrap_or(s);
    restored.trim().to_string()
}

/// A full "remind me …" request: (when, what). None when it is not one or has no time.
pub fn parse_request(text: &str, now: DateTime<Local>) -> Option<(DateTime<Local>, String)> {
    let lower = text.trim().to_lowercase();
    let c = REQUEST.captures(&lower)?;
    let rest = c.name("rest")?.as_str();
    let (due, remaining) = parse_when(rest, now)?;
    let message = clean_message(&remaining, text);
    Some((due, if message.is_empty() { "the thing you asked me to remind you about".into() } else { message }))
}

// ----------------------------------------------------------------------------- store ----

struct Inner {
    conn: Option<Mutex<Connection>>,
    items: RwLock<Vec<Reminder>>,
    alerts: broadcast::Sender<String>,
    widgets: Option<WidgetEngine>,
    next_id: Mutex<i64>,
}

#[derive(Clone)]
pub struct ReminderStore {
    inner: Arc<Inner>,
}

impl ReminderStore {
    pub fn open<P: AsRef<Path>>(db_path: P, widgets: WidgetEngine) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(db_path.as_ref())?;
        conn.busy_timeout(Duration::from_secs(3))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS reminders (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 text TEXT NOT NULL,
                 due_ms INTEGER NOT NULL,
                 created_ms INTEGER NOT NULL
             );",
        )?;
        let items: Vec<Reminder> = {
            let mut stmt = conn.prepare("SELECT id, text, due_ms FROM reminders ORDER BY due_ms ASC")?;
            let rows = stmt.query_map([], |r| Ok(Reminder { id: r.get(0)?, text: r.get(1)?, due_ms: r.get(2)? }))?;
            rows.flatten().collect()
        };
        info!("[REMIND] {} pending reminder(s)", items.len());
        let store = Self::with(items, Some(conn), Some(widgets));
        store.sync_widget();
        Ok(store)
    }

    pub fn in_memory() -> Self {
        Self::with(Vec::new(), None, None)
    }

    fn with(items: Vec<Reminder>, conn: Option<Connection>, widgets: Option<WidgetEngine>) -> Self {
        let (alerts, _) = broadcast::channel(16);
        let next = items.iter().map(|r| r.id).max().unwrap_or(0) + 1;
        Self {
            inner: Arc::new(Inner { conn: conn.map(Mutex::new), items: RwLock::new(items), alerts, widgets, next_id: Mutex::new(next) }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.inner.alerts.subscribe()
    }

    pub fn list(&self) -> Vec<Reminder> {
        self.inner.items.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn add(&self, text: &str, due: DateTime<Local>) -> Reminder {
        let text: String = text.trim().chars().take(300).collect();
        let due_ms = due.timestamp_millis();
        let id = match &self.inner.conn {
            Some(conn) => {
                let conn = conn.lock().unwrap_or_else(|e| e.into_inner());
                match conn.execute("INSERT INTO reminders (text, due_ms, created_ms) VALUES (?1, ?2, ?3)", params![text, due_ms, Local::now().timestamp_millis()]) {
                    Ok(_) => conn.last_insert_rowid(),
                    Err(e) => {
                        warn!("[REMIND] Could not save the reminder: {e}");
                        self.next_id()
                    }
                }
            }
            None => self.next_id(),
        };
        let reminder = Reminder { id, text, due_ms };
        {
            let mut items = self.inner.items.write().unwrap_or_else(|e| e.into_inner());
            items.push(reminder.clone());
            items.sort_by_key(|r| r.due_ms);
        }
        info!("[REMIND] #{id} '{}' at {}", reminder.text, reminder.due().format("%Y-%m-%d %H:%M:%S"));
        self.sync_widget();
        reminder
    }

    fn next_id(&self) -> i64 {
        let mut n = self.inner.next_id.lock().unwrap_or_else(|e| e.into_inner());
        *n += 1;
        *n - 1
    }

    fn delete(&self, ids: &[i64]) {
        self.inner.items.write().unwrap_or_else(|e| e.into_inner()).retain(|r| !ids.contains(&r.id));
        if let Some(conn) = &self.inner.conn {
            let conn = conn.lock().unwrap_or_else(|e| e.into_inner());
            for id in ids {
                let _ = conn.execute("DELETE FROM reminders WHERE id = ?1", params![id]);
            }
        }
        self.sync_widget();
    }

    /// By id, "all", "last"/"next", or words of the text.
    pub fn cancel(&self, target: &str) -> Vec<Reminder> {
        let items = self.list();
        let t = target.trim().to_lowercase();
        let hits: Vec<Reminder> = if t == "all" || t == "alle" || t == "everything" {
            items
        } else if t == "next" || t == "first" {
            items.into_iter().take(1).collect()
        } else if t == "last" || t == "latest" {
            items.into_iter().max_by_key(|r| r.id).into_iter().collect()
        } else if let Ok(id) = t.trim_start_matches('#').parse::<i64>() {
            items.into_iter().filter(|r| r.id == id).collect()
        } else {
            items.into_iter().filter(|r| r.text.to_lowercase().contains(&t)).collect()
        };
        let ids: Vec<i64> = hits.iter().map(|r| r.id).collect();
        if !ids.is_empty() {
            self.delete(&ids);
        }
        hits
    }

    /// Mirrors the pending reminders into the HUD's "Reminders" widget.
    fn sync_widget(&self) {
        let Some(widgets) = &self.inner.widgets else { return };
        let items = self.list();
        if items.is_empty() {
            if widgets.widget(WIDGET_ID).is_some() {
                widgets.remove(WIDGET_ID);
            }
            return;
        }
        let now = Local::now();
        let rows: Vec<Value> = items
            .iter()
            .take(12)
            .map(|r| json!({ "key": spoken_when(r.due(), now).trim_start_matches("at ").to_string(), "value": r.text }))
            .collect();
        let _ = widgets.create_or_update(&json!({ "id": WIDGET_ID, "type": "KeyValueList", "title": "Reminders", "items": rows }));
    }

    /// Announces due reminders (and those that fell due while Jarvis was off).
    pub fn start_runtime(&self) {
        let store = self.clone();
        tokio::spawn(async move {
            let started = Local::now();
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            let mut last_minute = started.minute();
            loop {
                tick.tick().await;
                let now = Local::now();
                let due: Vec<Reminder> = store.list().into_iter().filter(|r| r.due_ms <= now.timestamp_millis()).collect();
                if !due.is_empty() {
                    store.delete(&due.iter().map(|r| r.id).collect::<Vec<_>>());
                    for r in due {
                        let missed = r.due_ms < started.timestamp_millis() - 60_000;
                        let what = addressed_to_sir(r.text.trim_end_matches('.'));
                        let message = if missed {
                            format!("Sir, while I was offline you asked to be reminded {}: {what}.", spoken_when(r.due(), now))
                        } else {
                            format!("Sir, a reminder: {what}.")
                        };
                        info!("[REMIND] Due: #{} '{}'", r.id, r.text);
                        let _ = store.inner.alerts.send(message);
                    }
                }
                // Keep "tomorrow at …" labels right across midnight.
                if now.minute() != last_minute {
                    last_minute = now.minute();
                    if now.hour() == 0 && now.minute() == 0 {
                        store.sync_widget();
                    }
                }
            }
        });
    }

    /// The `reminders` tool: set / list / cancel.
    pub fn tool(&self, args: &Value) -> Result<String, String> {
        let action = crate::tools::str_arg(args, &["action"]).unwrap_or("set").to_lowercase();
        let now = Local::now();
        match action.as_str() {
            "set" | "add" | "create" => {
                let text = crate::tools::str_arg(args, &["text", "message", "what", "reminder"]).ok_or("reminders(set) needs 'text'.")?;
                let due = if let Some(ms) = args.get("due_ms").and_then(Value::as_i64) {
                    Local.timestamp_millis_opt(ms).single().ok_or("invalid due time")?
                } else if let Some(mins) = args.get("in_minutes").and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok())) {
                    now + ChronoDuration::seconds((mins * 60.0).round().max(1.0) as i64)
                } else if let Some(when) = crate::tools::str_arg(args, &["at", "when", "time"]) {
                    let spec = if when.chars().next().is_some_and(|c| c.is_ascii_digit()) { format!("at {when}") } else { when.to_string() };
                    parse_when(&spec, now).map(|(d, _)| d).ok_or_else(|| format!("I could not understand the time '{when}'."))?
                } else {
                    parse_when(text, now).map(|(d, _)| d).ok_or("reminders(set) needs 'in_minutes' or 'at'.")?
                };
                let r = self.add(text, due);
                Ok(format!("Reminder #{} set {} (in {}): {}", r.id, spoken_when(due, now), spoken_in(due, now), r.text))
            }
            "list" | "show" => {
                let items = self.list();
                if items.is_empty() {
                    return Ok("No reminders are pending.".into());
                }
                Ok(items
                    .iter()
                    .map(|r| format!("#{} {} (in {}): {}", r.id, spoken_when(r.due(), now), spoken_in(r.due(), now), r.text))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "cancel" | "delete" | "remove" => {
                let target = crate::tools::str_arg(args, &["id", "text", "target"]).unwrap_or("next");
                let removed = self.cancel(target);
                if removed.is_empty() {
                    Err(format!("No reminder matches '{target}'."))
                } else {
                    Ok(format!("Cancelled {} reminder(s): {}", removed.len(), removed.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("; ")))
                }
            }
            other => Err(format!("Unknown reminders action '{other}' (set, list, cancel).")),
        }
    }
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "reminders",
        "description": "Reminders for Sir: action set (text + in_minutes, or at 'HH:MM' / 'tomorrow 9:00'), list, or cancel (id, words of the text, 'next' or 'all'). Jarvis announces them when due.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["set", "list", "cancel"]},
            "text": {"type": "string", "description": "What to remind Sir of"},
            "in_minutes": {"type": "number"},
            "at": {"type": "string", "description": "e.g. 18:30, 6 pm, tomorrow 9:00"},
            "id": {"type": "string"}
        }, "required": ["action"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 21, h, m, 0).unwrap()
    }

    #[test]
    fn relative_reminders() {
        let now = at(10, 0);
        let (due, what) = parse_request("Remind me in 10 minutes to check the oven", now).unwrap();
        assert_eq!((due, what.as_str()), (at(10, 10), "check the oven"));
        let (due, what) = parse_request("Jarvis, remind me in an hour and 15 minutes that the pizza is ready", now).unwrap();
        assert_eq!((due, what.as_str()), (at(11, 15), "the pizza is ready"));
        let (due, what) = parse_request("Erinnere mich in 5 Minuten an den Tee", now).unwrap();
        assert_eq!((due, what.as_str()), (at(10, 5), "den Tee"));
        let (due, _) = parse_request("remind me in half an hour to stretch", now).unwrap();
        assert_eq!(due, at(10, 30));
        let (due, _) = parse_request("Erinnere mich in einer halben Stunde ans Lüften", now).unwrap();
        assert_eq!(due, at(10, 30));
        let (due, _) = parse_request("remind me in two hours to call Tony", now).unwrap();
        assert_eq!(due, at(12, 0));
    }

    #[test]
    fn absolute_reminders() {
        let now = at(10, 0);
        let (due, what) = parse_request("Remind me at 6 pm to call Mum", now).unwrap();
        assert_eq!((due, what.as_str()), (at(18, 0), "call Mum"));
        let (due, what) = parse_request("Erinnere mich um 18:30 Uhr, die Wäsche aufzuhängen", now).unwrap();
        assert_eq!((due, what.as_str()), (at(18, 30), "die Wäsche aufzuhängen"));
        let (due, _) = parse_request("remind me at 9 to stand up", now).unwrap();
        assert_eq!(due, at(9, 0) + ChronoDuration::days(1), "9 o'clock has passed today");
        let (due, what) = parse_request("remind me tomorrow at 9:15 about the dentist", now).unwrap();
        assert_eq!((due, what.as_str()), (at(9, 15) + ChronoDuration::days(1), "the dentist"));
        let (due, _) = parse_request("Erinnere mich morgen um 7 an das Meeting", now).unwrap();
        assert_eq!(due, at(7, 0) + ChronoDuration::days(1));
        let (due, _) = parse_request("remind me at noon to eat", now).unwrap();
        assert_eq!(due, at(12, 0));
        assert!(parse_request("remind me to buy milk", now).is_none(), "no time: the LLM asks when");
        assert!(parse_request("What time is it", now).is_none());
    }

    #[test]
    fn store_and_tool() {
        let store = ReminderStore::in_memory();
        let out = store.tool(&json!({"action": "set", "text": "Feed the cat", "in_minutes": 30})).unwrap();
        assert!(out.contains("Feed the cat") && out.contains("30 minutes"), "{out}");
        store.tool(&json!({"action": "set", "text": "Call Pepper", "at": "23:59"})).unwrap();
        assert_eq!(store.list().len(), 2);
        assert!(store.tool(&json!({"action": "list"})).unwrap().contains("Call Pepper"));
        assert_eq!(store.cancel("cat").len(), 1);
        assert_eq!(store.cancel("all").len(), 1);
        assert!(store.list().is_empty());
        assert_eq!(spoken_in(at(10, 0) + ChronoDuration::seconds(5430), at(10, 0)), "1 hour and 30 minutes");
        assert_eq!(spoken_in(at(10, 1), at(10, 0) + ChronoDuration::milliseconds(1100)), "1 minute");
        assert_eq!(spoken_duration(10), "10 seconds");
        assert_eq!(addressed_to_sir("stretch my legs"), "stretch your legs");
        assert_eq!(addressed_to_sir("call me, I'm late for my train."), "call you, you're late for your train.");
        assert_eq!(addressed_to_sir("den Tee"), "den Tee");
    }
}
