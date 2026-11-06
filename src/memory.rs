//! Long-term memory in SQLite (`jarvis_memory.db`): WAL journaling, an FTS5 index (BM25 ranking)
//! kept in sync by triggers, and a `LIKE`-scoring fallback when FTS5 is unavailable.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use tracing::{info, warn};

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("{0}")]
    Validation(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: i64,
    pub key: String,
    pub value: String,
    pub category: String,
    pub importance: i32,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct RememberOutcome {
    pub entry: MemoryEntry,
    /// Previous value when an existing fact was corrected/overwritten.
    pub previous_value: Option<String>,
}

#[derive(Debug, Clone)]
pub enum DeleteOutcome {
    Deleted(Vec<MemoryEntry>),
    /// A fuzzy target matched several memories; nothing was deleted.
    Ambiguous(Vec<MemoryEntry>),
    NotFound,
}

const SELECT_COLUMNS: &str = "m.id, m.key, m.value, m.category, m.importance, m.created_at, m.updated_at";

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "my", "me", "i", "you", "your", "what", "who", "do", "does",
    "of", "to", "in", "on", "for", "and", "or", "about", "that", "this", "it", "der", "die", "das",
    "ein", "eine", "ist", "mein", "meine", "ich", "du", "und", "oder", "was", "wer", "wie", "zu",
    "von", "mit", "für", "fuer", "über", "ueber", "remember", "recall", "memory", "memories",
    // Weak verbs/fillers that would otherwise prefix-match unrelated facts ("use" → "user").
    "use", "used", "using", "have", "has", "had", "know", "tell", "like", "call", "called", "get",
    "got", "can", "could", "would", "should", "please", "jarvis", "sir", "habe", "hab", "hast",
    "kennst", "weißt", "weisst", "nutze", "benutze", "heißt", "heisst", "bitte",
];

#[derive(Clone)]
pub struct MemoryManager {
    conn: Arc<Mutex<Connection>>,
    fts: bool,
}

impl MemoryManager {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, MemoryError> {
        let conn = Connection::open(path.as_ref())?;
        conn.busy_timeout(Duration::from_secs(3))?;
        let journal: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.execute_batch(
            "PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS memories (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 key TEXT NOT NULL UNIQUE,
                 value TEXT NOT NULL,
                 category TEXT NOT NULL DEFAULT 'general',
                 importance INTEGER NOT NULL DEFAULT 5,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_memories_category ON memories(category);
             CREATE INDEX IF NOT EXISTS idx_memories_importance ON memories(importance DESC, updated_at DESC);
             CREATE TABLE IF NOT EXISTS conversation_logs (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 role TEXT NOT NULL,
                 content TEXT NOT NULL,
                 timestamp TEXT NOT NULL
             );",
        )?;

        let fts = match Self::init_fts(&conn) {
            Ok(()) => true,
            Err(e) => {
                warn!("[MEM] FTS5 unavailable ({e}); falling back to LIKE search.");
                false
            }
        };

        let manager = Self {
            conn: Arc::new(Mutex::new(conn)),
            fts,
        };
        manager.seed_if_empty();
        info!(
            "[MEM] Long-term memory ready at {:?} (journal={journal}, fts5={fts}, {} facts)",
            path.as_ref(),
            manager.count().unwrap_or(0)
        );
        Ok(manager)
    }

    fn init_fts(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                 key, value, category,
                 content='memories', content_rowid='id',
                 tokenize='unicode61 remove_diacritics 2'
             );
             CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                 INSERT INTO memories_fts(rowid, key, value, category)
                 VALUES (new.id, new.key, new.value, new.category);
             END;
             CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                 INSERT INTO memories_fts(memories_fts, rowid, key, value, category)
                 VALUES ('delete', old.id, old.key, old.value, old.category);
             END;
             CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
                 INSERT INTO memories_fts(memories_fts, rowid, key, value, category)
                 VALUES ('delete', old.id, old.key, old.value, old.category);
                 INSERT INTO memories_fts(rowid, key, value, category)
                 VALUES (new.id, new.key, new.value, new.category);
             END;
             INSERT INTO memories_fts(memories_fts) VALUES ('rebuild');",
        )
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn seed_if_empty(&self) {
        if self.count().unwrap_or(1) > 0 {
            return;
        }
        let _ = self.remember_fact(
            "user_title",
            "Always address the user respectfully as 'Sir'.",
            Some("persona"),
            Some(10),
        );
        let _ = self.remember_fact(
            "language_preference",
            "Sir speaks German, English or a mix of both; Jarvis always replies in British English.",
            Some("preference"),
            Some(10),
        );
    }

    pub fn count(&self) -> Result<i64, MemoryError> {
        Ok(self
            .lock()
            .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?)
    }

    /// Normalises any phrase into a stable snake_case key.
    pub fn normalize_key(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        let mut last_us = true;
        for ch in raw.trim().to_lowercase().chars() {
            let mapped = match ch {
                'ä' => Some("ae"),
                'ö' => Some("oe"),
                'ü' => Some("ue"),
                'ß' => Some("ss"),
                _ => None,
            };
            if let Some(m) = mapped {
                out.push_str(m);
                last_us = false;
            } else if ch.is_ascii_alphanumeric() {
                out.push(ch);
                last_us = false;
            } else if !last_us {
                out.push('_');
                last_us = true;
            }
        }
        let trimmed = out.trim_matches('_');
        trimmed.chars().take(64).collect::<String>().trim_matches('_').to_string()
    }

    /// Inserts or updates (corrects) a fact.
    pub fn remember_fact(
        &self,
        key: &str,
        value: &str,
        category: Option<&str>,
        importance: Option<i32>,
    ) -> Result<RememberOutcome, MemoryError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(MemoryError::Validation("A memory needs a non-empty value.".into()));
        }
        let mut key = Self::normalize_key(key);
        if key.is_empty() {
            key = Self::normalize_key(&value.split_whitespace().take(4).collect::<Vec<_>>().join(" "));
        }
        if key.is_empty() {
            return Err(MemoryError::Validation("Could not derive a memory key.".into()));
        }
        let value: String = value.chars().take(1000).collect();
        let category = category
            .map(Self::normalize_key)
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "general".to_string());
        let importance = importance.unwrap_or(7).clamp(1, 10);
        let now = Utc::now().to_rfc3339();

        let conn = self.lock();
        let previous_value: Option<String> = conn
            .query_row("SELECT value FROM memories WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?;
        conn.execute(
            "INSERT INTO memories (key, value, category, importance, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(key) DO UPDATE SET
                 value = excluded.value,
                 category = excluded.category,
                 importance = excluded.importance,
                 updated_at = excluded.updated_at",
            params![key, value, category, importance, now],
        )?;
        let entry = conn.query_row(
            &format!("SELECT {SELECT_COLUMNS} FROM memories m WHERE m.key = ?1"),
            params![key],
            Self::map_row,
        )?;
        let previous_value = previous_value.filter(|p| p != &entry.value);
        Ok(RememberOutcome {
            entry,
            previous_value,
        })
    }

    /// Ranked search. With no usable query terms this lists by importance.
    pub fn recall(
        &self,
        query: Option<&str>,
        category: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        let limit = limit.clamp(1, 200) as i64;
        let category = category
            .map(Self::normalize_key)
            .filter(|c| !c.is_empty());
        let terms = query.map(search_terms).unwrap_or_default();

        if terms.is_empty() {
            return self.list_filtered(category.as_deref(), limit);
        }

        if self.fts {
            // Prefix-match only longer terms; short ones must match a whole token.
            let expr = terms
                .iter()
                .map(|t| if t.chars().count() >= 4 { format!("\"{t}\"*") } else { format!("\"{t}\"") })
                .collect::<Vec<_>>()
                .join(" OR ");
            let conn = self.lock();
            let result: rusqlite::Result<Vec<MemoryEntry>> = (|| {
                // BM25 with column weights: key 4×, value 1×, category 0.5×.
                let sql = format!(
                    "SELECT {SELECT_COLUMNS} FROM memories_fts f JOIN memories m ON m.id = f.rowid
                     WHERE memories_fts MATCH ?1 AND (?2 IS NULL OR m.category = ?2)
                     ORDER BY bm25(memories_fts, 4.0, 1.0, 0.5), m.importance DESC LIMIT ?3"
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(params![expr, category, limit], Self::map_row)?;
                rows.collect()
            })();
            match result {
                Ok(rows) if !rows.is_empty() => return Ok(rows),
                Ok(_) => {}
                Err(e) => warn!("[MEM] FTS query '{expr}' failed ({e}); using LIKE fallback."),
            }
        }

        // Fallback: score every memory by matched terms (tables are small).
        let all = self.list_filtered(category.as_deref(), 1000)?;
        let mut scored: Vec<(usize, MemoryEntry)> = all
            .into_iter()
            .filter_map(|m| {
                let hay = format!("{} {} {}", m.key, m.value, m.category).to_lowercase();
                let score = terms.iter().filter(|t| hay.contains(t.as_str())).count();
                (score > 0).then_some((score, m))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.importance.cmp(&a.1.importance)));
        Ok(scored.into_iter().take(limit as usize).map(|(_, m)| m).collect())
    }

    fn list_filtered(&self, category: Option<&str>, limit: i64) -> Result<Vec<MemoryEntry>, MemoryError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM memories m
             WHERE (?1 IS NULL OR m.category = ?1)
             ORDER BY m.importance DESC, m.updated_at DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![category, limit], Self::map_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list(&self, limit: usize) -> Result<Vec<MemoryEntry>, MemoryError> {
        self.list_filtered(None, limit.clamp(1, 1000) as i64)
    }

    /// Deletes by numeric id, exact key, or a fuzzy description that matches exactly one memory.
    pub fn delete(&self, id_or_key: &str) -> Result<DeleteOutcome, MemoryError> {
        let target = id_or_key.trim();
        if target.is_empty() {
            return Err(MemoryError::Validation("Nothing to delete was specified.".into()));
        }

        let find_by = |sql: &str, arg: &dyn rusqlite::ToSql| -> Result<Option<MemoryEntry>, MemoryError> {
            let conn = self.lock();
            Ok(conn
                .query_row(&format!("SELECT {SELECT_COLUMNS} FROM memories m WHERE {sql}"), [arg], Self::map_row)
                .optional()?)
        };

        let exact = if let Ok(id) = target.parse::<i64>() {
            find_by("m.id = ?1", &id)?
        } else {
            None
        };
        let exact = match exact {
            Some(e) => Some(e),
            None => find_by("m.key = ?1", &Self::normalize_key(target))?,
        };

        let victims = match exact {
            Some(entry) => vec![entry],
            None => {
                let candidates = self.recall(Some(target), None, 5)?;
                match candidates.len() {
                    0 => return Ok(DeleteOutcome::NotFound),
                    1 => candidates,
                    _ => return Ok(DeleteOutcome::Ambiguous(candidates)),
                }
            }
        };

        let conn = self.lock();
        for v in &victims {
            conn.execute("DELETE FROM memories WHERE id = ?1", params![v.id])?;
        }
        Ok(DeleteOutcome::Deleted(victims))
    }

    /// Highest-importance facts for the system prompt (ordering is stable to keep the LLM
    /// prompt prefix cacheable).
    pub fn core_for_prompt(&self, limit: usize) -> Vec<MemoryEntry> {
        let conn = self.lock();
        let result: rusqlite::Result<Vec<MemoryEntry>> = (|| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SELECT_COLUMNS} FROM memories m ORDER BY m.importance DESC, m.id ASC LIMIT ?1"
            ))?;
            let rows = stmt.query_map(params![limit as i64], Self::map_row)?;
            rows.collect()
        })();
        result.unwrap_or_else(|e| {
            warn!("[MEM] Could not load core memories: {e}");
            Vec::new()
        })
    }

    pub fn log_turn(&self, role: &str, content: &str) {
        let now = Utc::now().to_rfc3339();
        if let Err(e) = self.lock().execute(
            "INSERT INTO conversation_logs (role, content, timestamp) VALUES (?1, ?2, ?3)",
            params![role, content, now],
        ) {
            warn!("[MEM] Could not log conversation turn: {e}");
        }
    }

    /// The newest conversation turns, oldest first: (local time, role, text).
    pub fn recent_turns(&self, limit: usize) -> Vec<(String, String, String)> {
        let conn = self.lock();
        let rows: rusqlite::Result<Vec<(String, String, String)>> = (|| {
            let mut stmt = conn.prepare("SELECT timestamp, role, content FROM conversation_logs ORDER BY id DESC LIMIT ?1")?;
            let rows = stmt.query_map(params![limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect()
        })();
        let mut rows = rows.unwrap_or_default();
        rows.reverse();
        rows.into_iter().map(|(ts, role, text)| (local_time(&ts), role, text)).collect()
    }

    /// Past conversation lines matching `query` (all words) within the last `days`.
    pub fn search_conversation(&self, query: Option<&str>, days: u32, limit: usize) -> Vec<(String, String, String)> {
        let since = (Utc::now() - chrono::Duration::days(days.max(1) as i64)).to_rfc3339();
        let terms = query.map(search_terms).unwrap_or_default();
        let conn = self.lock();
        let rows: rusqlite::Result<Vec<(String, String, String)>> = (|| {
            let mut stmt = conn.prepare("SELECT timestamp, role, content FROM conversation_logs WHERE timestamp >= ?1 ORDER BY id DESC LIMIT 2000")?;
            let rows = stmt.query_map(params![since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect()
        })();
        let mut hits: Vec<(String, String, String)> = rows
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, _, content): &(String, String, String)| {
                let c = content.to_lowercase();
                terms.iter().all(|t| c.contains(t.as_str()))
            })
            .take(limit)
            .collect();
        hits.reverse();
        hits.into_iter().map(|(ts, role, text)| (local_time(&ts), role, text)).collect()
    }

    fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryEntry> {
        Ok(MemoryEntry {
            id: row.get(0)?,
            key: row.get(1)?,
            value: row.get(2)?,
            category: row.get(3)?,
            importance: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    }

    pub fn tool_definitions() -> Vec<Value> {
        vec![
            json!({"type": "function", "function": {
                "name": "remember_fact",
                "description": "Save or correct a durable fact about Sir (name, preferences, people, projects, habits, corrections). Re-using an existing key overwrites it.",
                "parameters": {"type": "object", "properties": {
                    "key": {"type": "string", "description": "Short snake_case id, e.g. 'favorite_editor', 'girlfriend_name'"},
                    "value": {"type": "string", "description": "The fact as one concise English sentence"},
                    "category": {"type": "string", "description": "preference | personal | project | schedule | system | general"},
                    "importance": {"type": "integer", "description": "1 (trivia) to 10 (core identity)"}
                }, "required": ["key", "value"]}
            }}),
            json!({"type": "function", "function": {
                "name": "recall_memories",
                "description": "Search long-term memory for facts about Sir.",
                "parameters": {"type": "object", "properties": {
                    "query": {"type": "string", "description": "Keywords to search for"},
                    "category": {"type": "string", "description": "Optional category filter"}
                }, "required": []}
            }}),
            json!({"type": "function", "function": {
                "name": "delete_memory",
                "description": "Forget a stored fact by its key, numeric id or a short description.",
                "parameters": {"type": "object", "properties": {
                    "id_or_key": {"type": "string", "description": "Memory key, id or description"}
                }, "required": ["id_or_key"]}
            }}),
        ]
    }
}

/// RFC 3339 (UTC) → "2026-10-20 20:33" local time.
fn local_time(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| ts.chars().take(16).collect())
}

/// The `search_conversation` tool: what Sir and Jarvis said, e.g. yesterday or about a topic.
pub fn conversation_tool(memory: &MemoryManager, args: &Value) -> Result<String, String> {
    let query = crate::tools::str_arg(args, &["query", "topic", "about"]);
    let days = args.get("days_back").and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok())).unwrap_or(7).clamp(1, 365) as u32;
    let lines = memory.search_conversation(query, days, 40);
    if lines.is_empty() {
        return Ok(format!("Nothing{} in the conversation log of the last {days} day(s).", query.map(|q| format!(" about '{q}'")).unwrap_or_default()));
    }
    Ok(lines
        .into_iter()
        .map(|(ts, role, text)| {
            let who = if role == "user" { "Sir" } else { "Jarvis" };
            format!("[{ts}] {who}: {}", text.chars().take(240).collect::<String>())
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn conversation_tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "search_conversation",
        "description": "Search what Sir and you said in earlier conversations (logged with dates): for 'what did we talk about yesterday', 'what did I say about X', 'what was that link/number you told me'.",
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string", "description": "Keywords; omit to get the most recent lines"},
            "days_back": {"type": "integer", "description": "How far back (default 7)"}
        }, "required": []}
    }})
}

/// Lower-cased alphanumeric search terms without stopwords (safe to quote inside FTS5).
fn search_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .map(|t| t.to_lowercase())
        .filter(|t| t.chars().count() >= 2 && !STOPWORDS.contains(&t.as_str()))
        .collect();
    terms.dedup();
    terms.truncate(12);
    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (MemoryManager, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("jarvis_mem_test_{}.db", uuid::Uuid::new_v4()));
        (MemoryManager::open(&path).expect("open db"), path)
    }

    #[test]
    fn remember_recall_correct_and_delete() {
        let (mem, path) = temp_db();
        assert!(mem.fts, "bundled SQLite must provide FTS5");

        let first = mem
            .remember_fact("Favorite Editor", "Sir prefers VS Code for Rust work.", Some("preference"), Some(6))
            .unwrap();
        assert_eq!(first.entry.key, "favorite_editor");
        assert!(first.previous_value.is_none());

        let corrected = mem
            .remember_fact("favorite_editor", "Sir switched to Zed.", None, None)
            .unwrap();
        assert_eq!(corrected.previous_value.as_deref(), Some("Sir prefers VS Code for Rust work."));

        mem.remember_fact("coffee", "Sir drinks his coffee black.", Some("preference"), Some(5))
            .unwrap();
        let hits = mem.recall(Some("what editor do I use?"), None, 5).unwrap();
        assert_eq!(hits.first().map(|m| m.key.as_str()), Some("favorite_editor"));
        let umlaut = mem.remember_fact("Größe", "Sir ist 1,85 m groß.", None, None).unwrap();
        assert_eq!(umlaut.entry.key, "groesse");

        match mem.delete("coffee").unwrap() {
            DeleteOutcome::Deleted(v) => assert_eq!(v[0].key, "coffee"),
            other => panic!("unexpected {other:?}"),
        }
        // "sir" appears in several memories → must not mass-delete.
        assert!(matches!(mem.delete("sir").unwrap(), DeleteOutcome::Ambiguous(_)));

        mem.log_turn("user", "Jarvis, what is the boiling point of tungsten?");
        mem.log_turn("assistant", "Tungsten boils at about 5,900 degrees Celsius, Sir.");
        mem.log_turn("user", "Open Spotify");
        let hits = mem.search_conversation(Some("tungsten"), 1, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].1, "user");
        assert_eq!(mem.recent_turns(2).last().map(|t| t.2.as_str()), Some("Open Spotify"));
        assert!(conversation_tool(&mem, &json!({"query": "tungsten"})).unwrap().contains("Jarvis: Tungsten boils"));
        drop(mem);
        let _ = std::fs::remove_file(&path);
    }
}
