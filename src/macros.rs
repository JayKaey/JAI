//! Natural voice macros: Sir teaches Jarvis a routine in his own words ("when I say 'study time',
//! open my physics folder, start a 45 minute focus timer and play my lo-fi playlist"). Each step is
//! an ordinary request, run one after the other through the full pipeline (fast path, LLM,
//! tools) as if Sir had said it, so a step can be anything Jarvis understands. Stored in
//! `voice_macros` (`jarvis_memory.db`); the trigger phrase runs it from the fast path.

use crate::stark::StarkEvent;
use crate::tools::{str_arg, ToolContext};
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VoiceMacro {
    pub name: String,
    pub trigger_phrases: Vec<String>,
    pub steps: Vec<String>,
    pub description: String,
    pub usage_count: i64,
}

#[derive(Clone)]
pub struct MacroStore {
    conn: Arc<Mutex<Connection>>,
}

fn normalize(s: &str) -> String {
    crate::intents::normalize_phrase(s)
}

/// "open X, then start Y and play Z" → the separate steps.
pub fn split_steps(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut s = text.replace('\n', ";");
    for sep in [" and then ", ", then ", " then ", " danach ", ", dann ", " und dann ", " dann "] {
        s = s.replace(sep, ";");
    }
    for part in s.split(';') {
        // "open my folder, start a timer" (commas between imperatives).
        for piece in part.split(", ") {
            let p = piece.trim().trim_start_matches("and ").trim_start_matches("und ").trim().trim_end_matches(['.', ',']).trim();
            if p.split_whitespace().count() >= 2 {
                out.push(p.to_string());
            }
        }
    }
    out
}

impl MacroStore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let conn = Connection::open(path.as_ref()).map_err(|e| e.to_string())?;
        conn.busy_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        Self::with(conn)
    }

    pub fn in_memory() -> Self {
        Self::with(Connection::open_in_memory().expect("in-memory SQLite")).expect("in-memory macro schema")
    }

    fn with(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS voice_macros (
                 name TEXT PRIMARY KEY,
                 trigger_phrases TEXT NOT NULL DEFAULT '[]',
                 steps TEXT NOT NULL DEFAULT '[]',
                 description TEXT NOT NULL DEFAULT '',
                 usage_count INTEGER NOT NULL DEFAULT 0,
                 updated_at TEXT NOT NULL DEFAULT ''
             );",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn list(&self) -> Vec<VoiceMacro> {
        let db = self.db();
        let Ok(mut stmt) = db.prepare("SELECT name, trigger_phrases, steps, description, usage_count FROM voice_macros ORDER BY name") else { return Vec::new() };
        stmt.query_map([], |r| {
            Ok(VoiceMacro {
                name: r.get(0)?,
                trigger_phrases: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_default(),
                steps: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                description: r.get(3)?,
                usage_count: r.get(4)?,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    pub fn get(&self, name: &str) -> Option<VoiceMacro> {
        let n = normalize(name);
        let list = self.list();
        list.iter()
            .find(|m| normalize(&m.name) == n)
            .or_else(|| list.iter().find(|m| strsim::jaro_winkler(&normalize(&m.name), &n) > 0.9))
            .cloned()
    }

    pub fn save(&self, m: &VoiceMacro) -> Result<(), String> {
        if m.name.trim().is_empty() || m.steps.is_empty() {
            return Err("A macro needs a name and at least one step.".into());
        }
        self.db()
            .execute(
                "INSERT INTO voice_macros (name, trigger_phrases, steps, description, usage_count, updated_at) VALUES (?1, ?2, ?3, ?4, 0, ?5)
                 ON CONFLICT(name) DO UPDATE SET trigger_phrases = ?2, steps = ?3, description = ?4, updated_at = ?5",
                params![
                    m.name.trim(),
                    serde_json::to_string(&m.trigger_phrases).unwrap_or_default(),
                    serde_json::to_string(&m.steps).unwrap_or_default(),
                    m.description,
                    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
                ],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn delete(&self, name: &str) -> bool {
        let Some(m) = self.get(name) else { return false };
        self.db().execute("DELETE FROM voice_macros WHERE name = ?1", params![m.name]).map(|n| n > 0).unwrap_or(false)
    }

    fn used(&self, name: &str) {
        let _ = self.db().execute("UPDATE voice_macros SET usage_count = usage_count + 1 WHERE name = ?1", params![name]);
    }

    /// The macro Sir's words trigger: a trigger phrase (or "run the <name> macro").
    pub fn match_trigger(&self, text: &str) -> Option<VoiceMacro> {
        let s = normalize(text);
        if s.is_empty() {
            return None;
        }
        let list = self.list();
        let named = ["run the ", "run my ", "run ", "start the ", "start my ", "execute ", "do the "]
            .iter()
            .find_map(|p| s.strip_prefix(p))
            .and_then(|rest| rest.strip_suffix(" macro").or_else(|| rest.strip_suffix(" routine")))
            .map(str::trim);
        if let Some(n) = named {
            if let Some(m) = list.iter().find(|m| normalize(&m.name) == n) {
                return Some(m.clone());
            }
        }
        list.into_iter().find(|m| {
            m.trigger_phrases.iter().any(|t| {
                let t = normalize(t);
                !t.is_empty() && (t == s || (t.len() >= 6 && strsim::jaro_winkler(&t, &s) > 0.93))
            })
        })
    }
}

pub fn is_tool(name: &str) -> bool {
    name == "manage_voice_macro"
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "manage_voice_macro",
        "description": "Natural voice macros: create a multi-step routine Sir teaches you, triggered by his own phrase (\"when I say 'study time', open my physics folder, start a 45 minute focus timer and play my lo-fi playlist\"). Each step is a plain request in Sir's words. Also list, show, delete or run one.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["create", "list", "show", "delete", "run"]},
            "name": {"type": "string"},
            "trigger_phrase": {"type": "string"},
            "steps": {"type": "array", "items": {"type": "string"}, "description": "Each step as a request, e.g. 'open my physics folder'"},
            "description": {"type": "string"}
        }, "required": ["action"]}
    }})
}

pub fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let store = &tools.stark.macros;
    let action = str_arg(args, &["action"]).unwrap_or("list").to_lowercase();
    let name = str_arg(args, &["name", "macro", "macro_name"]).unwrap_or("").to_string();
    tools.stark.pulse(&["brain", "protocols"], false);
    match action.as_str() {
        "create" | "save" | "add" | "teach" | "update" => {
            let mut steps: Vec<String> = args.get("steps").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::trim).map(String::from)).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
            if steps.len() == 1 {
                steps = split_steps(&steps[0]);
            }
            if steps.is_empty() {
                if let Some(text) = str_arg(args, &["steps_text", "routine"]) {
                    steps = split_steps(text);
                }
            }
            let trigger = str_arg(args, &["trigger_phrase", "trigger", "phrase"]).unwrap_or(&name).to_string();
            let name = if name.is_empty() { trigger.clone() } else { name };
            let triggers: Vec<String> = trigger.split(['|', ';']).map(|t| t.trim().trim_matches(['"', '\'']).to_string()).filter(|t| !t.is_empty()).collect();
            let m = VoiceMacro { name: name.clone(), trigger_phrases: triggers.clone(), steps: steps.clone(), description: str_arg(args, &["description"]).unwrap_or("").to_string(), usage_count: 0 };
            store.save(&m)?;
            Ok(format!(
                "Voice macro '{name}' saved with {} step{} ({}); trigger: \"{}\".",
                steps.len(),
                if steps.len() == 1 { "" } else { "s" },
                steps.join(" → "),
                triggers.join("\" or \"")
            ))
        }
        "delete" | "remove" | "forget" => {
            if store.delete(&name) {
                Ok(format!("Deleted the voice macro '{name}'."))
            } else {
                Err(format!("No voice macro called '{name}'."))
            }
        }
        "show" | "describe" => {
            let m = store.get(&name).ok_or_else(|| format!("No voice macro called '{name}'."))?;
            Ok(format!("'{}' (trigger \"{}\", used {}×): {}", m.name, m.trigger_phrases.join("\", \""), m.usage_count, m.steps.join(" → ")))
        }
        "run" | "start" | "execute" => {
            let m = store.get(&name).ok_or_else(|| format!("No voice macro called '{name}'."))?;
            store.used(&m.name);
            let _ = tools.stark.events.send(StarkEvent::RunMacro { name: m.name.clone(), steps: m.steps.clone() });
            Ok(format!("Running the '{}' macro: {} step{} ({}).", m.name, m.steps.len(), if m.steps.len() == 1 { "" } else { "s" }, m.steps.join(" → ")))
        }
        _ => {
            let all = store.list();
            if all.is_empty() {
                Ok("No voice macros yet. Teach me one: \"when I say 'study time', open … then …\".".into())
            } else {
                Ok(all.iter().map(|m| format!("- {} (\"{}\"): {}", m.name, m.trigger_phrases.join("\", \""), m.steps.join(" → "))).collect::<Vec<_>>().join("\n"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_split_and_triggers_match() {
        assert_eq!(
            split_steps("open my physics folder, start a 45 minute focus timer and then play my lo-fi playlist"),
            vec!["open my physics folder", "start a 45 minute focus timer", "play my lo-fi playlist"]
        );
        let store = MacroStore::in_memory();
        store
            .save(&VoiceMacro { name: "Study time".into(), trigger_phrases: vec!["study time".into()], steps: vec!["open Discord".into()], description: String::new(), usage_count: 0 })
            .unwrap();
        assert_eq!(store.match_trigger("Jarvis, study time!").map(|m| m.name), Some("Study time".into()));
        assert_eq!(store.match_trigger("run the study time macro").map(|m| m.name), Some("Study time".into()));
        assert!(store.match_trigger("open Spotify").is_none());
        assert!(store.delete("study time"));
        assert!(store.list().is_empty());
    }
}
