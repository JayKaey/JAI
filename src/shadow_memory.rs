//! SHADOW // ACOUSTIC MEMORY: the last 30 minutes of what was said, searchable — Sir's turns
//! from the conversation log in SQLite and, while the Discord bridge listens, what the others in
//! his call said (RAM only). "What did I say ten minutes ago?", "search my last half hour for
//! Kaufland", "turn my last five thoughts into a note block".

use crate::hud_board::{self, Block, BlockKind, Note, Report};
use crate::tools::ToolContext;
use chrono::{DateTime, Duration as ChronoDuration, Local, SecondsFormat, Utc};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::time::Duration;

pub const TOOL: &str = "shadow_memory";
/// How far back the memory reaches.
pub const WINDOW_MINUTES: i64 = 30;

pub fn is_tool(name: &str) -> bool {
    name == TOOL
}

pub fn definitions() -> Vec<Value> {
    vec![json!({"type": "function", "function": {
        "name": TOOL,
        "description": "Your rolling 30-minute memory of what Sir said (and, while you listen to his Discord call, what the others said). action said_ago (minutes): what Sir said about that long ago. search (query): every line with those words. to_note (count): Sir's last lines as a note block on the board. recent (minutes).",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["said_ago", "search", "to_note", "recent"]},
            "minutes": {"type": "number"},
            "query": {"type": "string"},
            "count": {"type": "number"}
        }, "required": ["action"]}
    }})]
}

/// One remembered line.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub at: DateTime<Utc>,
    /// "Sir", "JAI" or "Call".
    pub who: String,
    pub text: String,
}

fn stamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Nanos, false)
}

/// The conversation log between two instants (Sir and JAI), oldest first.
pub fn lines_between(conn: &Connection, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Line> {
    let Ok(mut q) = conn.prepare("SELECT role, content, timestamp FROM conversation_logs WHERE timestamp >= ?1 AND timestamp <= ?2 ORDER BY timestamp") else { return Vec::new() };
    let rows = q.query_map(params![stamp(from), stamp(to)], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)));
    let Ok(rows) = rows else { return Vec::new() };
    rows.flatten()
        .filter_map(|(role, text, ts)| {
            let at = DateTime::parse_from_rfc3339(&ts).ok()?.with_timezone(&Utc);
            Some(Line { at, who: if role == "user" { "Sir".into() } else { "JAI".into() }, text })
        })
        .collect()
}

/// The memory's window up to `now`: the log plus the call (when the bridge listens).
fn window(conn: &Connection, now: DateTime<Utc>) -> Vec<Line> {
    let mut lines = lines_between(conn, now - ChronoDuration::minutes(WINDOW_MINUTES), now);
    let call_now = Local::now();
    for (clock, text) in crate::discord_bridge::recent(Duration::from_secs(WINDOW_MINUTES as u64 * 60)) {
        // "HH:MM:SS" today.
        if let Ok(t) = chrono::NaiveTime::parse_from_str(&clock, "%H:%M:%S") {
            if let Some(at) = call_now.date_naive().and_time(t).and_local_timezone(Local).single() {
                lines.push(Line { at: at.with_timezone(&Utc), who: "Call".into(), text });
            }
        }
    }
    lines.sort_by_key(|l| l.at);
    lines
}

/// What Sir said about `minutes` ago: his lines within ±3 minutes of then, else the last one
/// before it.
pub fn said_ago(lines: &[Line], now: DateTime<Utc>, minutes: i64) -> Vec<Line> {
    let then = now - ChronoDuration::minutes(minutes);
    let sir: Vec<&Line> = lines.iter().filter(|l| l.who == "Sir").collect();
    let near: Vec<Line> = sir.iter().filter(|l| (l.at - then).num_seconds().abs() <= 180).map(|l| (*l).clone()).collect();
    if !near.is_empty() {
        return near;
    }
    sir.iter().rev().find(|l| l.at <= then).map(|l| vec![(*l).clone()]).unwrap_or_default()
}

/// Every line containing all of `query`'s words (case-insensitive).
pub fn search(lines: &[Line], query: &str) -> Vec<Line> {
    let words: Vec<String> = query.to_lowercase().split_whitespace().filter(|w| w.len() > 1).map(String::from).collect();
    if words.is_empty() {
        return Vec::new();
    }
    lines.iter().filter(|l| { let t = l.text.to_lowercase(); words.iter().all(|w| t.contains(w.as_str())) }).cloned().collect()
}

/// Sir's last `count` lines as one note stack ("NOTE // YOUR LAST 5 THOUGHTS").
pub fn note_block(lines: &[Line], count: usize) -> Option<Block> {
    let sir: Vec<&Line> = lines.iter().filter(|l| l.who == "Sir").collect();
    let last: Vec<&Line> = sir.iter().rev().take(count).rev().copied().collect();
    if last.is_empty() {
        return None;
    }
    let notes: Vec<Note> = last
        .iter()
        .map(|l| Note { headline: l.text.split_whitespace().take(6).collect::<Vec<_>>().join(" "), body: format!("{} — {}", l.at.with_timezone(&Local).format("%H:%M"), l.text) })
        .collect();
    Some(Block::new(format!("NOTE // YOUR LAST {} THOUGHTS", notes.len()), "SHADOW", "SOURCE: SHADOW MEMORY", BlockKind::NoteStack { notes }))
}

fn spoken(display: impl AsRef<str>, line: impl AsRef<str>) -> String {
    format!("{}\nSpoken: {}", display.as_ref(), line.as_ref())
}

fn listing(lines: &[Line]) -> String {
    lines.iter().map(|l| format!("[{}] {}: {}", l.at.with_timezone(&Local).format("%H:%M:%S"), l.who, l.text)).collect::<Vec<_>>().join("\n")
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let db = tools.config.read(|c| c.memory_db_path.clone());
    let conn = Connection::open(&db).map_err(|e| e.to_string())?;
    let now = Utc::now();
    let lines = window(&conn, now);
    let number = |key: &str, default: f64| args[key].as_f64().or_else(|| args[key].as_str().and_then(|s| s.parse().ok())).unwrap_or(default);
    match args["action"].as_str().unwrap_or("recent") {
        "said_ago" => {
            let minutes = number("minutes", 10.0).clamp(1.0, WINDOW_MINUTES as f64) as i64;
            let found = said_ago(&lines, now, minutes);
            match found.first() {
                Some(first) => Ok(spoken(listing(&found), format!("About {minutes} minutes ago you said: \"{}\"", first.text.trim_end_matches('.')))),
                None => Ok(spoken("Nothing from then.", format!("You didn't say anything to me around {minutes} minutes ago, Sir."))),
            }
        }
        "search" => {
            let query = args["query"].as_str().unwrap_or("").trim().to_string();
            let found = search(&lines, &query);
            if found.is_empty() {
                return Ok(spoken(format!("No line about '{query}'."), format!("Nothing about {query} in the last half hour, Sir.")));
            }
            Ok(spoken(listing(&found), format!("{} line{} about {query}, Sir. The latest: \"{}\"", found.len(), if found.len() == 1 { "" } else { "s" }, found.last().map(|l| l.text.as_str()).unwrap_or(""))))
        }
        "to_note" => {
            let count = number("count", 5.0).clamp(1.0, 12.0) as usize;
            let block = note_block(&lines, count).ok_or("You haven't said anything to me in the last half hour, Sir.")?;
            let n = match &block.kind { BlockKind::NoteStack { notes } => notes.len(), _ => 0 };
            hud_board::present(Report { topic: "Shadow memory".into(), created_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(), blocks: vec![block], sources: Vec::new(), id: None });
            Ok(spoken(format!("Note block with your last {n} thoughts."), format!("Your last {n} thoughts are on the board, Sir.")))
        }
        _ => {
            let minutes = number("minutes", 10.0).clamp(1.0, WINDOW_MINUTES as f64) as i64;
            let since = now - ChronoDuration::minutes(minutes);
            let recent: Vec<Line> = lines.into_iter().filter(|l| l.at >= since).collect();
            Ok(spoken(listing(&recent), format!("{} lines in the last {minutes} minutes, Sir.", recent.len())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with(rows: &[(&str, &str, DateTime<Utc>)]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE conversation_logs (id INTEGER PRIMARY KEY AUTOINCREMENT, role TEXT NOT NULL, content TEXT NOT NULL, timestamp TEXT NOT NULL);").unwrap();
        for (role, text, at) in rows {
            c.execute("INSERT INTO conversation_logs (role, content, timestamp) VALUES (?1, ?2, ?3)", params![role, text, stamp(*at)]).unwrap();
        }
        c
    }

    #[test]
    fn what_did_i_say_ten_minutes_ago() {
        let now = Utc::now();
        let m = |k: i64| now - ChronoDuration::minutes(k);
        let c = db_with(&[("user", "Shift at Kaufland on Friday from 15:00", m(25)), ("user", "remind me to study induction", m(11)), ("assistant", "Very good, Sir.", m(11)), ("user", "what's the weather", m(2)), ("user", "too old", m(45))]);
        let lines = lines_between(&c, m(WINDOW_MINUTES), now);
        assert_eq!(lines.len(), 4, "the 45-minute-old line is outside the window");
        let found = said_ago(&lines, now, 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "remind me to study induction");
        assert_eq!(search(&lines, "kaufland friday").len(), 1);
        assert!(search(&lines, "").is_empty());
        let block = note_block(&lines, 5).unwrap();
        match &block.kind {
            BlockKind::NoteStack { notes } => {
                assert_eq!(notes.len(), 3, "Sir's lines only");
                assert!(notes[2].body.ends_with("what's the weather"));
            }
            _ => panic!(),
        }
        assert!(block.label.contains("LAST 3 THOUGHTS"));
    }
}
